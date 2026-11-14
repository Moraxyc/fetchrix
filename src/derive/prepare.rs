//! Writable copy of a fetched source changed by `prepare` steps.

use std::{
    ffi::OsStr,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use tempfile::TempDir;
use tokio::process::Command;

use super::{DeriveError, Result, tail};
use crate::{
    config::{Artifact, Prepare, Step},
    nixpkgs::Nixpkgs,
};

/// The prepared tree and the declared artifacts read from it.
pub struct Prepared {
    _work: TempDir,
    pub path: PathBuf,
    /// Artifact contents keyed by name.
    pub artifacts: Vec<(String, Vec<u8>)>,
    /// The artifacts as files named by their names, for Nix to read.
    pub artifact_dir: PathBuf,
}

pub struct Preparer<'a> {
    /// Provides the tools of built-in steps.
    pub nixpkgs: &'a Nixpkgs,
    /// Directory of the configuration; hooks resolve against it.
    pub config_dir: &'a Path,
    pub version: &'a str,
}

impl Preparer<'_> {
    /// Copies `src`, runs the steps of `prepare` in the copy, and reads its
    /// artifacts.
    ///
    /// # Errors
    ///
    /// Returns an error if copying, a step, or reading an artifact fails.
    ///
    /// # Panics
    ///
    /// Never; copying the tree does not panic.
    pub async fn prepare(&self, prepare: &Prepare, src: &Path) -> Result<Prepared> {
        if !src.is_dir() {
            return Err(DeriveError::Unsupported(
                "`prepare` on a single-file source",
            ));
        }
        let work = tempfile::tempdir().map_err(io("cannot create a temporary directory"))?;
        let path = work.path().join("source");
        let git_dir = work.path().join("git");
        let home = work.path().join("home");
        {
            let (src, path) = (src.to_owned(), path.clone());
            tokio::task::spawn_blocking(move || copy_tree(&src, &path))
                .await
                .expect("copying does not panic")
                .map_err(io("cannot copy the source"))?;
        }
        std::fs::create_dir(&home).map_err(io("cannot create a temporary directory"))?;
        let git = Git {
            dir: &git_dir,
            tree: &path,
        };
        let wants_patch = prepare
            .artifacts
            .values()
            .any(|a| matches!(a, Artifact::Patch { .. }));
        if wants_patch {
            git.run(&["init", "--quiet"]).await?;
            git.snapshot().await?;
            git.run(&[
                "-c",
                "user.name=fetchrix",
                "-c",
                "user.email=fetchrix@localhost",
                "commit",
                "--quiet",
                "--allow-empty",
                "--no-verify",
                "--message=source",
            ])
            .await?;
        }

        for step in &prepare.steps {
            let cwd = step
                .sub_dir()
                .map_or_else(|| path.clone(), |d| path.join(d));
            let mut command = self.command(step, &cwd);
            command
                .current_dir(&cwd)
                .env("HOME", &home)
                .env("COMPOSER_HOME", home.join("composer"))
                .env("npm_config_cache", home.join("npm"))
                .env("FETCHRIX_SOURCE", &path)
                .env("FETCHRIX_VERSION", self.version);
            run(&mut command, &describe(step)).await?;
        }

        let artifact_dir = work.path().join("artifacts");
        std::fs::create_dir(&artifact_dir).map_err(io("cannot create a temporary directory"))?;
        let mut artifacts = Vec::new();
        if wants_patch {
            git.snapshot().await?;
        }
        for (name, artifact) in &prepare.artifacts {
            let content =
                match artifact {
                    Artifact::File { path: file } => tokio::fs::read(path.join(file))
                        .await
                        .map_err(|source| DeriveError::Io {
                            context: format!("cannot read artifact `{name}` from {file}"),
                            source,
                        })?,
                    Artifact::Patch { paths } => git.diff(paths).await?,
                };
            tokio::fs::write(artifact_dir.join(name), &content)
                .await
                .map_err(io("cannot write an artifact"))?;
            artifacts.push((name.clone(), content));
        }
        Ok(Prepared {
            _work: work,
            path,
            artifacts,
            artifact_dir,
        })
    }

    fn command(&self, step: &Step, cwd: &Path) -> Command {
        let nix_tool = |installable: &str, program: &str| {
            let mut command = Command::new("nix");
            command
                .arg("shell")
                .args(self.nixpkgs.installable(installable))
                .args(["--command", program]);
            command
        };
        match step {
            Step::NpmLock { args, .. } => {
                let mut command = nix_tool("nodejs", "npm");
                command
                    .args([
                        "install",
                        "--package-lock-only",
                        "--ignore-scripts",
                        "--no-audit",
                        "--no-fund",
                    ])
                    .args(args);
                command
            }
            Step::ComposerLock { args, .. } => {
                let mut command = nix_tool("php.packages.composer", "composer");
                command
                    .arg("update")
                    .args(cwd.join("composer.lock").exists().then_some("--lock"))
                    .args([
                        "--no-install",
                        "--no-scripts",
                        "--no-plugins",
                        "--no-interaction",
                    ])
                    .args(args);
                command
            }
            Step::GoModDownload { args, .. } => {
                let mut command = nix_tool("go", "go");
                command.args(["mod", "download"]).args(args);
                command
            }
            Step::PnpmLock { args, .. } => {
                let mut command = nix_tool("nodePackages.pnpm", "pnpm");
                command
                    .args([
                        "install",
                        "--lockfile-only",
                        "--ignore-scripts",
                        "--no-frozen-lockfile",
                    ])
                    .args(args);
                command
            }
            Step::CargoGenerateLock { args, .. } => {
                let mut command = nix_tool("cargo", "cargo");
                command.args(["generate-lockfile"]).args(args);
                command
            }
            Step::Hook { run, .. } => {
                let (program, args) = run.split_first().expect("validation requires a program");
                let mut command = Command::new(self.config_dir.join(program));
                command.args(args);
                command
            }
        }
    }
}

fn describe(step: &Step) -> String {
    match step {
        Step::NpmLock { .. } => "npm-lock".into(),
        Step::ComposerLock { .. } => "composer-lock".into(),
        Step::GoModDownload { .. } => "go-mod-download".into(),
        Step::PnpmLock { .. } => "pnpm-lock".into(),
        Step::CargoGenerateLock { .. } => "cargo-generate-lock".into(),
        Step::Hook { run, .. } => format!("hook `{}`", run.join(" ")),
    }
}

/// Runs `git` on the copy with a repository outside it, so a `.git` in the
/// source is left alone and ignore rules do not hide generated files.
struct Git<'a> {
    dir: &'a Path,
    tree: &'a Path,
}

impl Git<'_> {
    async fn run<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<Vec<u8>> {
        let mut command = Command::new("git");
        command
            .arg("--git-dir")
            .arg(self.dir)
            .arg("--work-tree")
            .arg(self.tree)
            .args(["-c", "core.autocrlf=false", "-c", "core.fileMode=true"])
            .args(args)
            .current_dir(self.tree);
        run(&mut command, "git").await
    }

    /// Staged changes to `paths` against the original source, as a `-p1` patch.
    async fn diff(&self, paths: &[String]) -> Result<Vec<u8>> {
        let mut args: Vec<&OsStr> = [
            "diff",
            "--cached",
            "--no-ext-diff",
            "--no-color",
            "--binary",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "HEAD",
            "--",
        ]
        .map(OsStr::new)
        .to_vec();
        args.extend(paths.iter().map(OsStr::new));
        self.run(&args).await
    }

    async fn snapshot(&self) -> Result<()> {
        self.run(&["add", "--all", "--force", "--", "."]).await?;
        Ok(())
    }
}

async fn run(command: &mut Command, what: &str) -> Result<Vec<u8>> {
    let output = command
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|source| DeriveError::Io {
            context: format!("cannot run {what}"),
            source,
        })?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(DeriveError::Prepare(format!(
            "{what} failed:\n{}",
            tail(&output.stderr)
        )))
    }
}

fn io(context: &'static str) -> impl FnOnce(std::io::Error) -> DeriveError {
    move |source| DeriveError::Io {
        context: context.into(),
        source,
    }
}

/// Copies `from` to `to`, keeping symlinks and the executable bit and making
/// everything writable by the owner.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(&src)?, &dst)?;
        } else if kind.is_dir() {
            copy_tree(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
            let mode = std::fs::metadata(&src)?.permissions().mode() & 0o111 | 0o644;
            std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(mode))?;
        }
    }
    std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o755))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn runs_hook_and_exports_artifacts() {
        let config_dir = tempfile::tempdir().unwrap();
        let hook = config_dir.path().join("hook.sh");
        std::fs::write(
            &hook,
            "#!/bin/sh\nset -e\necho \"$FETCHRIX_VERSION\" > new.txt\necho changed > a.txt\n",
        )
        .unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();

        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), "original\n").unwrap();
        std::fs::write(src.path().join("b.txt"), "kept\n").unwrap();
        std::fs::write(src.path().join(".gitignore"), "new.txt\n").unwrap();
        std::fs::set_permissions(
            src.path().join("a.txt"),
            std::fs::Permissions::from_mode(0o444),
        )
        .unwrap();

        let prepare = Prepare {
            steps: vec![Step::Hook {
                run: vec!["./hook.sh".into()],
                sub_dir: None,
            }],
            artifacts: BTreeMap::from([
                (
                    "new.txt".into(),
                    Artifact::File {
                        path: "new.txt".into(),
                    },
                ),
                (
                    "fix.patch".into(),
                    Artifact::Patch {
                        paths: vec!["a.txt".into(), "new.txt".into()],
                    },
                ),
            ]),
        };
        let prepared = Preparer {
            nixpkgs: &Nixpkgs::new(None, config_dir.path()),
            config_dir: config_dir.path(),
            version: "1.2.3",
        }
        .prepare(&prepare, src.path())
        .await
        .expect("the hook succeeds");

        assert_eq!(
            std::fs::read_to_string(src.path().join("a.txt")).unwrap(),
            "original\n"
        );
        let artifacts: BTreeMap<_, _> = prepared
            .artifacts
            .iter()
            .map(|(k, v)| (k.as_str(), String::from_utf8_lossy(v).into_owned()))
            .collect();
        assert_eq!(artifacts["new.txt"], "1.2.3\n");
        assert_eq!(
            std::fs::read_to_string(prepared.artifact_dir.join("new.txt")).unwrap(),
            "1.2.3\n"
        );
        let patch = &artifacts["fix.patch"];
        assert!(patch.contains("--- a/a.txt\n+++ b/a.txt"), "{patch}");
        assert!(patch.contains("-original\n+changed"), "{patch}");
        assert!(patch.contains("+++ b/new.txt"), "{patch}");
        assert!(!patch.contains("b.txt"), "{patch}");
    }

    #[tokio::test]
    async fn reports_failing_hook() {
        let config_dir = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let prepare = Prepare {
            steps: vec![Step::Hook {
                run: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "echo boom >&2; exit 3".into(),
                ],
                sub_dir: None,
            }],
            artifacts: BTreeMap::new(),
        };
        let Err(DeriveError::Prepare(message)) = (Preparer {
            nixpkgs: &Nixpkgs::new(None, config_dir.path()),
            config_dir: config_dir.path(),
            version: "1",
        })
        .prepare(&prepare, src.path())
        .await
        else {
            panic!("the hook fails");
        };
        assert!(message.contains("boom"), "{message}");
    }
}
