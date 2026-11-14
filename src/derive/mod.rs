//! Hashes and lock files derived from a fetched source.

pub mod prepare;

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::Path,
};

use miette::Diagnostic;
use nix_compat::nixhash::{HashAlgo, NixHash};
use thiserror::Error;

use crate::{
    config::{DeriveKind, DeriveSpec, Strategy},
    fetch::{FetchError, Fetcher},
    nixpkgs::nix_file,
};

#[derive(Debug, Error, Diagnostic)]
pub enum DeriveError {
    #[error("{context}")]
    #[diagnostic(code(fetchrix::derive::io))]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    #[diagnostic(code(fetchrix::derive::nix))]
    Nix(String),
    #[error(transparent)]
    #[diagnostic(transparent)]
    Fetch(#[from] FetchError),
    #[error("invalid package-lock.json: {0}")]
    #[diagnostic(
        code(fetchrix::derive::npm_lock),
        help("`importNpmLock` needs `lockfileVersion` 2 or later")
    )]
    NpmLock(String),
    #[error("invalid pnpm-lock.yaml: {0}")]
    #[diagnostic(code(fetchrix::derive::pnpm_lock))]
    PnpmLock(String),
    #[error("invalid go.sum: {0}")]
    #[diagnostic(code(fetchrix::derive::go_sum))]
    GoSum(String),
    #[error("invalid Cargo.lock: {0}")]
    #[diagnostic(code(fetchrix::derive::cargo_lock))]
    CargoLock(String),
    #[error("{0} is not supported yet")]
    #[diagnostic(code(fetchrix::derive::unsupported))]
    Unsupported(&'static str),
    #[error("prepare: {0}")]
    #[diagnostic(code(fetchrix::derive::prepare))]
    Prepare(String),
}

type Result<T, E = DeriveError> = std::result::Result<T, E>;

pub enum Output {
    Hash(NixHash),
    Lockfile {
        /// Paths relative to the source root, lock file first, then the
        /// files its consumer reads beside it.
        files: Vec<(String, Vec<u8>)>,
        /// `importCargoLock` hashes of git dependencies, keyed by `name-version`.
        output_hashes: BTreeMap<String, NixHash>,
    },
}

pub struct Deriver<'a> {
    pub fetcher: &'a Fetcher<'a>,
    /// Directory of the configuration; relative flake references and files
    /// in `fod` resolve against it.
    pub config_dir: &'a Path,
    /// Package version passed to `fod` functions.
    pub version: &'a str,
    /// Directory of the prepared artifacts, one file per name.
    pub artifacts: Option<&'a Path>,
}

impl Deriver<'_> {
    /// Computes the output of `spec` for the source tree at `src`.
    ///
    /// # Errors
    ///
    /// Returns an error if the strategy fails or is not supported.
    pub async fn derive(&self, spec: &DeriveSpec, src: &Path) -> Result<Output> {
        match spec.strategy {
            Strategy::Fod => {
                let attr = spec
                    .attr
                    .as_deref()
                    .ok_or(DeriveError::Unsupported("`fod` without `attr`"))?;
                self.fod(attr, spec, src).await.map(Output::Hash)
            }
            Strategy::Lockfile => self.lockfile(spec, src).await,
        }
    }

    /// Builds `attr`, from `file` when given, with `src` and an empty output
    /// hash, and reads the real hash from the mismatch error. The derivation
    /// must be fixed-output before the override, which would make any
    /// derivation one.
    async fn fod(&self, attr: &str, spec: &DeriveSpec, src: &Path) -> Result<NixHash> {
        #[derive(serde::Deserialize)]
        struct Drvs {
            fod: String,
            empty: String,
        }
        let target = spec
            .select
            .as_deref()
            .map_or_else(|| format!("`{attr}`"), |s| format!("`{s}` of `{attr}`"));
        let apply = fod_apply(src, spec.select.as_deref(), self.version, self.artifacts);
        let mut args: Vec<OsString> = vec!["eval".into(), "--impure".into(), "--raw".into()];
        args.extend(eval_target(attr, spec.file.as_deref(), self.config_dir));
        args.extend(["--apply".into(), apply.into()]);
        let eval = self.nix(&args).await?;
        if !eval.status.success() {
            return Err(DeriveError::Nix(format!(
                "cannot evaluate `{attr}`:\n{}",
                tail(&eval.stderr)
            )));
        }
        let drvs: Drvs = serde_json::from_slice(&eval.stdout)
            .map_err(|e| DeriveError::Nix(format!("unexpected `nix eval` output: {e}")))?;
        let show = self
            .nix(&["derivation", "show", drvs.fod.as_str(), drvs.empty.as_str()])
            .await?;
        if !show.status.success() {
            return Err(DeriveError::Nix(format!(
                "cannot show the derivations of {target}:\n{}",
                tail(&show.stderr)
            )));
        }
        if fixed_output_hash(&show.stdout, &drvs.fod)?.is_none() {
            return Err(DeriveError::Nix(format!(
                "{target} is not a fixed-output derivation"
            )));
        }
        match fixed_output_hash(&show.stdout, &drvs.empty)? {
            Some(hash) if is_empty_hash(&hash) => {}
            hash => {
                return Err(DeriveError::Nix(format!(
                    "{target} keeps the hash `{}` after the override",
                    hash.unwrap_or_default()
                )));
            }
        }
        let out = format!("{}^out", drvs.empty);
        let build = self.nix(&["build", "--no-link", out.as_str()]).await?;
        if build.status.success() {
            return Err(DeriveError::Nix(format!(
                "{target} built with an empty hash"
            )));
        }
        let got = mismatch_got(&String::from_utf8_lossy(&build.stderr), &drvs.empty).ok_or_else(
            || DeriveError::Nix(format!("cannot build {target}:\n{}", tail(&build.stderr))),
        )?;
        NixHash::from_sri(&got)
            .map_err(|e| DeriveError::Nix(format!("unexpected hash `{got}`: {e}")))
    }

    async fn lockfile(&self, spec: &DeriveSpec, src: &Path) -> Result<Output> {
        let sub_dir = spec.sub_dir.as_deref();
        let read = |name: &'static str| {
            let rel = sub_dir.map_or_else(|| name.to_owned(), |dir| format!("{dir}/{name}"));
            let path = src.join(&rel);
            async move {
                tokio::fs::read(&path)
                    .await
                    .map(|content| (rel, content))
                    .map_err(|source| DeriveError::Io {
                        context: format!("cannot read {name} from the source"),
                        source,
                    })
            }
        };
        let mut output_hashes = BTreeMap::new();
        let files = match spec.kind {
            Some(DeriveKind::CargoVendor) => {
                let lock = read("Cargo.lock").await?;
                let mut hashes: BTreeMap<GitDep, NixHash> = BTreeMap::new();
                for (key, dep) in cargo_git_deps(&lock.1)? {
                    let hash = if let Some(hash) = hashes.get(&dep) {
                        hash.clone()
                    } else {
                        let hash = self.fetcher.fetchgit_hash(&dep.url, &dep.rev).await?;
                        hashes.insert(dep, hash.clone());
                        hash
                    };
                    output_hashes.insert(key, hash);
                }
                vec![lock]
            }
            Some(DeriveKind::NpmDeps) => {
                let lock = read("package-lock.json").await?;
                check_npm_lock(&lock.1)?;
                vec![lock, read("package.json").await?]
            }
            Some(DeriveKind::PnpmDeps) => {
                let lock = read("pnpm-lock.yaml").await?;
                check_pnpm_lock(&lock.1)?;
                vec![lock, read("package.json").await?]
            }
            Some(DeriveKind::GoModules) => {
                let go_sum = read("go.sum").await?;
                check_go_sum(&go_sum.1)?;
                vec![go_sum, read("go.mod").await?]
            }
            None => return Err(DeriveError::Unsupported("`lockfile` without `kind`")),
        };
        Ok(Output::Lockfile {
            files,
            output_hashes,
        })
    }

    async fn nix(&self, args: &[impl AsRef<OsStr>]) -> Result<std::process::Output> {
        tokio::process::Command::new("nix")
            .args(args)
            .current_dir(self.config_dir)
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|source| DeriveError::Io {
                context: "cannot run nix".into(),
                source,
            })
    }
}

/// Arguments that select `attr` for `nix eval`: a flake installable, or an
/// attribute path in `file`.
fn eval_target(attr: &str, file: Option<&str>, config_dir: &Path) -> Vec<OsString> {
    match file {
        Some(file) => vec!["--file".into(), nix_file(file, config_dir), attr.into()],
        None => vec![attr.into()],
    }
}

/// The `--apply` function from the value of `attr` to JSON with the `drvPath`
/// of the FOD, `fod`, and of it with an empty hash, `empty`. Without `select`
/// the value is the FOD and its `src` is replaced; with it the value is a
/// function of `{ src, version, artifacts }` whose result holds the FOD at
/// `select`.
fn fod_apply(src: &Path, select: Option<&str>, version: &str, artifacts: Option<&Path>) -> String {
    let src = format!(
        "builtins.path {{ path = /. + {}; name = \"source\"; }}",
        nix_string(&src.to_string_lossy())
    );
    // Fetchers such as `fetchurl` and `fetchgit` compute `outputHash` from
    // their arguments, so overriding `hash` alone leaves it unchanged.
    let empty_hash = "({ outputHash = \"\"; outputHashAlgo = \"sha256\"; } \
                      // (if old ? hash then { hash = \"\"; } else { }))";
    let json = |fod: String| {
        format!(
            "let fod = {fod}; in builtins.toJSON {{ fod = fod.drvPath; \
             empty = (fod.overrideAttrs (old: {empty_hash})).drvPath; }}"
        )
    };
    let Some(select) = select else {
        return format!(
            "drv: {}",
            json(format!("drv.overrideAttrs {{ src = {src}; }}"))
        );
    };
    let artifacts = artifacts.map_or_else(
        || "{ }".to_owned(),
        |dir| {
            format!(
                "(let dir = {}; in builtins.mapAttrs \
                 (name: _: builtins.path {{ path = /. + \"${{dir}}/${{name}}\"; inherit name; }}) \
                 (builtins.readDir (/. + dir)))",
                nix_string(&dir.to_string_lossy())
            )
        },
    );
    let select: Vec<_> = select.split('.').map(nix_string).collect();
    format!(
        "f: {}",
        json(format!(
            "(f {{ src = {src}; version = {version}; artifacts = {artifacts}; }}).{select}",
            version = nix_string(version),
            select = select.join("."),
        ))
    )
}

/// The hash of the only output of `drv` in `nix derivation show` output, or
/// `None` if it is not fixed. Reads format version 4, where the derivations
/// sit under `derivations` keyed by base name, and the earlier bare map
/// keyed by path.
fn fixed_output_hash(show: &[u8], drv: &str) -> Result<Option<String>> {
    let bad =
        |why: &str| DeriveError::Nix(format!("unexpected `nix derivation show` output: {why}"));
    let json: serde_json::Value = serde_json::from_slice(show).map_err(|e| bad(&e.to_string()))?;
    let drvs = json.get("derivations").unwrap_or(&json);
    let outputs = drvs
        .as_object()
        .and_then(|m| m.get(drv).or_else(|| m.get(base_name(drv))))
        .and_then(|d| d.get("outputs"))
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| bad("no derivation outputs"))?;
    if outputs.len() != 1 {
        return Ok(None);
    }
    Ok(outputs
        .values()
        .next()
        .and_then(|o| o.get("hash"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned))
}

/// Whether `hash` is the all-zero SHA-256 hash Nix puts in place of `""`.
fn is_empty_hash(hash: &str) -> bool {
    NixHash::from_str(hash, Some(HashAlgo::Sha256))
        .is_ok_and(|h| h.digest_as_bytes().iter().all(|&b| b == 0))
}

/// The `got` hash of the fixed-output mismatch of `drv`, built with the empty
/// hash. A mismatch of a dependency names its own derivation and is not the
/// answer. Nix may build `drv` resolved to another path, so a derivation
/// with the same name answers too if the hash it specified is the empty one.
fn mismatch_got(stderr: &str, drv: &str) -> Option<String> {
    const MISMATCH: &str = "hash mismatch in fixed-output derivation '";
    let name = |path: &str| {
        base_name(path)
            .split_once('-')
            .map(|(_, name)| name.to_owned())
    };
    let field = |block: &str, key: &str| {
        block
            .lines()
            .find_map(|l| l.trim().strip_prefix(key))
            .map(str::trim)
            .map(str::to_owned)
    };
    let text = strip_ansi(stderr);
    let mut rest = text.as_str();
    while let Some(at) = rest.find(MISMATCH) {
        let (path, after) = rest[at + MISMATCH.len()..].split_once('\'')?;
        let block = after.split(MISMATCH).next().unwrap_or(after);
        let ours = path == drv
            || (name(path) == name(drv)
                && field(block, "specified:").is_some_and(|h| is_empty_hash(&h)));
        if ours {
            return field(block, "got:");
        }
        rest = after;
    }
    None
}

fn base_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// `s` without ANSI escape sequences, which Nix adds when colour is forced.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
        } else if chars.next() == Some('[') {
            chars.by_ref().find(|c| ('@'..='~').contains(c));
        }
    }
    out
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct GitDep {
    url: String,
    rev: String,
}

/// Git dependencies of a Cargo.lock, keyed by `name-version` as
/// `importCargoLock` expects.
fn cargo_git_deps(content: &[u8]) -> Result<Vec<(String, GitDep)>> {
    #[derive(serde::Deserialize)]
    struct Lock {
        #[serde(default)]
        package: Vec<Package>,
    }
    #[derive(serde::Deserialize)]
    struct Package {
        name: String,
        version: String,
        source: Option<String>,
    }
    let text = std::str::from_utf8(content).map_err(|e| DeriveError::CargoLock(e.to_string()))?;
    let lock: Lock = toml::from_str(text).map_err(|e| DeriveError::CargoLock(e.to_string()))?;
    lock.package
        .into_iter()
        .filter_map(|p| {
            let source = p.source?.strip_prefix("git+")?.to_owned();
            Some((p.name, p.version, source))
        })
        .map(|(name, version, source)| {
            let (url, rev) = source.split_once('#').ok_or_else(|| {
                DeriveError::CargoLock(format!("git source without a commit: {source}"))
            })?;
            let url = url.split_once('?').map_or(url, |(url, _)| url);
            Ok((
                format!("{name}-{version}"),
                GitDep {
                    url: url.to_owned(),
                    rev: rev.to_owned(),
                },
            ))
        })
        .collect()
}

/// Rejects lock files without the `packages` map that `importNpmLock` reads.
fn check_npm_lock(content: &[u8]) -> Result<()> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Lock {
        lockfile_version: u32,
    }
    let lock: Lock =
        serde_json::from_slice(content).map_err(|e| DeriveError::NpmLock(e.to_string()))?;
    if lock.lockfile_version < 2 {
        return Err(DeriveError::NpmLock(format!(
            "lockfileVersion {} has no `packages`",
            lock.lockfile_version
        )));
    }
    Ok(())
}

/// Rejects pnpm lock files missing the `lockfileVersion` key.
fn check_pnpm_lock(content: &[u8]) -> Result<()> {
    let text = std::str::from_utf8(content).map_err(|e| DeriveError::PnpmLock(e.to_string()))?;
    if !text.lines().any(|l| l.starts_with("lockfileVersion:")) {
        return Err(DeriveError::PnpmLock(
            "missing `lockfileVersion` key".into(),
        ));
    }
    Ok(())
}

/// Rejects go.sum files that are not valid UTF-8.
fn check_go_sum(content: &[u8]) -> Result<()> {
    std::str::from_utf8(content).map_err(|e| DeriveError::GoSum(e.to_string()))?;
    Ok(())
}

fn nix_string(s: &str) -> String {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace("${", "\\${");
    format!("\"{escaped}\"")
}

/// The last lines of a command's stderr, enough to show the failure.
fn tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<_> = text.lines().collect();
    lines[lines.len().saturating_sub(20)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_cargo_git_deps() {
        let lock = br#"
            version = 4

            [[package]]
            name = "app"
            version = "0.1.0"

            [[package]]
            name = "serde"
            version = "1.0.0"
            source = "registry+https://github.com/rust-lang/crates.io-index"
            checksum = "00"

            [[package]]
            name = "nix-compat"
            version = "0.1.0"
            source = "git+https://git.snix.dev/snix/snix.git?branch=canon#abc123"
        "#;
        assert_eq!(
            cargo_git_deps(lock).expect("the lock is valid"),
            [(
                "nix-compat-0.1.0".to_owned(),
                GitDep {
                    url: "https://git.snix.dev/snix/snix.git".to_owned(),
                    rev: "abc123".to_owned(),
                }
            )]
        );
    }

    #[test]
    fn checks_npm_lock_version() {
        check_npm_lock(br#"{"lockfileVersion": 3, "packages": {}}"#).expect("v3 is supported");
        assert!(matches!(
            check_npm_lock(br#"{"lockfileVersion": 1, "dependencies": {}}"#),
            Err(DeriveError::NpmLock(_))
        ));
    }

    #[test]
    fn selects_attr_from_file() {
        let dir = Path::new("/repo");
        assert_eq!(
            eval_target(".#foo.goModules", None, dir),
            [OsString::from(".#foo.goModules")]
        );
        assert_eq!(
            eval_target("foo.goModules", Some("default.nix"), dir),
            [
                OsString::from("--file"),
                "/repo/default.nix".into(),
                "foo.goModules".into()
            ]
        );
        assert_eq!(
            eval_target("hello", Some("<nixpkgs>"), dir),
            [OsString::from("--file"), "<nixpkgs>".into(), "hello".into()]
        );
    }

    #[test]
    fn applies_package_function() {
        let src = Path::new("/tmp/src");
        let plain = fod_apply(src, None, "1.0", None);
        assert!(
            plain.starts_with("drv: let fod = drv.overrideAttrs { src = "),
            "{plain}"
        );
        assert!(
            plain.contains(
                r#"in builtins.toJSON { fod = fod.drvPath; empty = (fod.overrideAttrs (old: ({ outputHash = ""; outputHashAlgo = "sha256"; } // (if old ? hash"#
            ),
            "{plain}"
        );
        let called = fod_apply(
            src,
            Some("passthru.goModules"),
            "1.0",
            Some(Path::new("/tmp/artifacts")),
        );
        assert!(
            called.starts_with(
                r#"f: let fod = (f { src = builtins.path { path = /. + "/tmp/src"; name = "source"; }; version = "1.0"; artifacts = (let dir = "/tmp/artifacts";"#
            ),
            "{called}"
        );
        assert!(
            called.contains(r#"})."passthru"."goModules"; in builtins.toJSON {"#),
            "{called}"
        );
        assert!(fod_apply(src, Some("goModules"), "1.0", None).contains("artifacts = { }; })."),);
    }

    #[test]
    fn reads_fixed_output_hash() {
        let v4 = br#"{"derivations":{"x-COPYING.drv":{"outputs":{"out":{"hash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","method":"flat"}}}},"version":4}"#;
        let hash = fixed_output_hash(v4, "/nix/store/x-COPYING.drv").expect("v4 is valid");
        assert!(hash.as_deref().is_some_and(is_empty_hash), "{hash:?}");
        let v3 = format!(
            r#"{{"/nix/store/x-src.drv":{{"outputs":{{"out":{{"hash":"{}","hashAlgo":"r:sha256","path":"/nix/store/y-src"}}}}}}}}"#,
            "0".repeat(64)
        );
        let hash = fixed_output_hash(v3.as_bytes(), "/nix/store/x-src.drv").expect("v3 is valid");
        assert!(hash.as_deref().is_some_and(is_empty_hash), "{hash:?}");
        let kept = br#"{"/nix/store/x.drv":{"outputs":{"out":{"hash":"sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=","method":"nar"}}}}"#;
        let hash = fixed_output_hash(kept, "/nix/store/x.drv").expect("the output is valid");
        assert!(
            hash.as_deref().is_some_and(|h| !is_empty_hash(h)),
            "{hash:?}"
        );
        let both = br#"{"derivations":{"a-hello.drv":{"outputs":{"out":{"path":"y-hello"}}},"b-hello.drv":{"outputs":{"out":{"hash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","method":"flat"}}}},"version":4}"#;
        assert_eq!(
            fixed_output_hash(both, "/nix/store/a-hello.drv").expect("v4 is valid"),
            None
        );
        assert!(matches!(
            fixed_output_hash(both, "/nix/store/c-hello.drv"),
            Err(DeriveError::Nix(_))
        ));
    }

    #[test]
    fn takes_mismatch_of_the_derivation() {
        let drv = "/nix/store/f9i6-COPYING.drv";
        let stderr = "error: hash mismatch in fixed-output derivation '/nix/store/a-dep.drv':\n\
                      \x20        specified: sha256-AAAA\n\
                      \x20           got:    sha256-dep=\n\
                      error: build of '\x1b[35;1m/nix/store/f9i6-COPYING.drv\x1b[0m' on 'ssh-ng://b' failed: \
                      hash mismatch in fixed-output derivation '\x1b[35;1m/nix/store/f9i6-COPYING.drv\x1b[0m':\n\
                      \x20        specified: \x1b[35;1msha256-AAAA\x1b[0m\n\
                      \x20           got:    \x1b[35;1msha256-yc8G=\x1b[0m\x1b[0m\n";
        assert_eq!(mismatch_got(stderr, drv).as_deref(), Some("sha256-yc8G="));
        assert_eq!(mismatch_got(stderr, "/nix/store/other.drv"), None);
        let empty = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
        let resolved = format!(
            "error: hash mismatch in fixed-output derivation '/nix/store/a-source.drv':\n\
             \x20 specified: sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=\n\
             \x20 got:    sha256-dep=\n\
             error: hash mismatch in fixed-output derivation '/nix/store/b-source.drv':\n\
             \x20 specified: {empty}\n\
             \x20 got:    sha256-ours=\n"
        );
        assert_eq!(
            mismatch_got(&resolved, "/nix/store/c-source.drv").as_deref(),
            Some("sha256-ours=")
        );
    }

    #[test]
    fn escapes_nix_strings() {
        assert_eq!(nix_string(r#"/a"b\${c}"#), r#""/a\"b\\\${c}""#);
    }
}
