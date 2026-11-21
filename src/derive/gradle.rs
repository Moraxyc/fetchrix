use std::{net::TcpListener, path::Path, process::Stdio, time::Duration};

use tokio::process::Command;

use super::{DeriveError, Deriver, Result, eval_target, nix_string, tail};
use crate::config::DeriveSpec;

impl Deriver<'_> {
    pub(super) async fn gradle_deps(&self, spec: &DeriveSpec, src: &Path) -> Result<Vec<u8>> {
        let io = |source| DeriveError::Io {
            context: "cannot record Gradle dependencies".into(),
            source,
        };
        let work = tempfile::tempdir().map_err(io)?;
        let mut args = vec!["eval".into(), "--raw".into()];
        args.extend(self.fetcher.nixpkgs.installable("path"));
        let root = self.nix(&args).await?;
        if !root.status.success() {
            return Err(DeriveError::Nix(tail(&root.stderr)));
        }
        let root = String::from_utf8_lossy(&root.stdout);
        let pkgs = format!("import (/. + {}) {{ }}", nix_string(&root));
        let tools = format!(
            "let p = {pkgs}; in p.buildEnv {{ name = \"gradle-recorder\"; paths = [ p.mitm-cache p.openssl p.python3 p.coreutils p.bash ] ++ p.lib.optional p.stdenv.isLinux p.bubblewrap; }}"
        );
        let tools = run(Command::new("nix").args([
            "build",
            "--impure",
            "--no-link",
            "--print-out-paths",
            "--expr",
            &tools,
        ]))
        .await?;
        let tools = String::from_utf8_lossy(&tools).trim().to_owned();
        let bin = |name: &str| format!("{tools}/bin/{name}");
        certificate(&bin("openssl"), work.path()).await?;
        let listener = TcpListener::bind("127.0.0.1:0").map_err(io)?;
        let addr = listener.local_addr().map_err(io)?;
        drop(listener);
        let address = addr.to_string();
        let mut proxy = Command::new(bin("mitm-cache"))
            .current_dir(work.path())
            .args([
                "-l",
                &address,
                "record",
                "--reject",
                r"\.(md5|sha(1|256|512:?):?)$",
                "--forget-redirects-from",
                ".*",
                "--record-text",
                r"/maven-metadata\.xml$",
            ])
            .stdout(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(io)?;
        let mut ready = false;
        for _ in 0..40 {
            if tokio::net::TcpStream::connect(addr).await.is_ok() {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        if !ready {
            return Err(DeriveError::Nix(
                "mitm-cache did not start listening".into(),
            ));
        }
        let apply = gradle_apply(spec, src, self.version, self.artifacts, &pkgs);
        let mut args = vec!["eval".into(), "--impure".into(), "--raw".into()];
        args.extend(eval_target(
            spec.attr.as_deref().expect("validation requires attr"),
            spec.file.as_deref(),
            self.config_dir,
        ));
        args.extend(["--apply".into(), apply.into()]);
        let drv = self.nix(&args).await?;
        if !drv.status.success() {
            return Err(DeriveError::Nix(tail(&drv.stderr)));
        }
        gradle_shell(
            spec,
            work.path(),
            &tools,
            addr,
            &String::from_utf8_lossy(&drv.stdout),
        )
        .await?;
        let pid = proxy.id().expect("the proxy has not been waited on");
        run(Command::new(bin("kill")).args(["-INT", &pid.to_string()])).await?;
        proxy.wait().await.map_err(io)?;
        run(Command::new(bin("python3"))
            .arg(format!(
                "{root}/pkgs/development/tools/build-managers/gradle/compress-deps-json.py"
            ))
            .arg(work.path().join("out.json"))
            .arg(work.path().join("deps.json")))
        .await?;
        std::fs::read(work.path().join("deps.json")).map_err(io)
    }
}

async fn run(command: &mut Command) -> Result<Vec<u8>> {
    let output = command
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|source| DeriveError::Io {
            context: "cannot run Gradle dependency recorder".into(),
            source,
        })?;
    if !output.status.success() {
        return Err(DeriveError::Nix(tail(&output.stderr)));
    }
    Ok(output.stdout)
}

fn gradle_apply(
    spec: &DeriveSpec,
    src: &Path,
    version: &str,
    artifacts: Option<&Path>,
    pkgs: &str,
) -> String {
    let src = format!(
        "builtins.path {{ path = /. + {}; name = \"source\"; }}",
        nix_string(&src.to_string_lossy())
    );
    let drv = if let Some(select) = &spec.select {
        let artifacts = artifacts.map_or_else(|| "{}".into(), |dir| format!("(let dir = /. + {}; in builtins.mapAttrs (name: _: dir + \"/${{name}}\") (builtins.readDir dir))", nix_string(&dir.to_string_lossy())));
        let select = select
            .split('.')
            .map(nix_string)
            .collect::<Vec<_>>()
            .join(".");
        format!(
            "(value {{ src = {src}; version = {}; artifacts = {artifacts}; }}).{select}",
            nix_string(version)
        )
    } else {
        format!("value.overrideAttrs {{ src = {src}; }}")
    };
    format!(
        "value: let p = {pkgs}; drv = {drv}; in (p.srcOnly (drv.overrideAttrs (old: {{ mitmCache = \"\"; gradleInitScript = p.path + \"/pkgs/development/tools/build-managers/gradle/init-deps.gradle\"; stdenv = old.stdenv or p.stdenvNoCC; }}))).drvPath"
    )
}

async fn gradle_shell(
    spec: &DeriveSpec,
    work: &Path,
    tools: &str,
    addr: std::net::SocketAddr,
    drv: &str,
) -> Result<()> {
    let io = |source| DeriveError::Io {
        context: "cannot write Gradle build script".into(),
        source,
    };
    let script = work.join("build.sh");
    std::fs::write(&script, r#"set -eo pipefail
if [ -e "${NIX_ATTRS_SH_FILE:-}" ]; then . "$NIX_ATTRS_SH_FILE"; fi
export http_proxy="$MITM_CACHE_ADDRESS" https_proxy="$MITM_CACHE_ADDRESS"
export SSL_CERT_FILE="$MITM_CACHE_CA" NIX_SSL_CERT_FILE="$MITM_CACHE_CA"
export GRADLE_USER_HOME="$MITM_CACHE_CERT_DIR/gradle" IN_GRADLE_UPDATE_DEPS=1
mkdir -p "$MITM_CACHE_CERT_DIR/build/out"
cd "$MITM_CACHE_CERT_DIR/build"
export out="$PWD/out"
source "$stdenv/setup"
phases="${prePhases[*]:-} unpackPhase patchPhase ${preConfigurePhases[*]:-} configurePhase gradleUpdateScript" genericBuild
"#).map_err(io)?;
    let mut shell = if spec.use_bwrap.unwrap_or(cfg!(target_os = "linux")) {
        let mut c = Command::new(format!("{tools}/bin/bwrap"));
        c.args([
            "--unshare-all",
            "--share-net",
            "--die-with-parent",
            "--ro-bind",
            "/",
            "/",
            "--tmpfs",
            "/home",
            "--bind",
            "/tmp",
            "/tmp",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--bind",
        ])
        .arg(work)
        .arg(work)
        .args(["--", "nix-shell"]);
        c
    } else {
        Command::new("nix-shell")
    };
    for (key, value) in [
        ("MITM_CACHE_HOST", "127.0.0.1".to_owned()),
        ("MITM_CACHE_PORT", addr.port().to_string()),
        ("MITM_CACHE_ADDRESS", addr.to_string()),
        ("MITM_CACHE_CA", work.join("ca.cer").display().to_string()),
        ("MITM_CACHE_CERT_DIR", work.display().to_string()),
    ] {
        shell.env(key, value).args(["--keep", key]);
    }
    shell
        .env("NIX_BUILD_SHELL", format!("{tools}/bin/bash"))
        .args(["--pure", "--run"])
        .arg(format!("bash {}", script.display()))
        .arg(drv.trim());
    run(&mut shell).await?;
    Ok(())
}

async fn certificate(openssl: &str, work: &Path) -> Result<()> {
    run(Command::new(openssl)
        .current_dir(work)
        .args(["genrsa", "-out", "ca.key", "2048"]))
    .await?;
    run(Command::new(openssl).current_dir(work).args([
        "req",
        "-x509",
        "-new",
        "-nodes",
        "-key",
        "ca.key",
        "-sha256",
        "-days",
        "1",
        "-out",
        "ca.cer",
        "-subj",
        "/CN=fetchrix",
    ]))
    .await?;
    Ok(())
}
