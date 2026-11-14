//! Hashes and lock files derived from a fetched source.

pub mod prepare;

use std::{collections::BTreeMap, path::Path};

use miette::Diagnostic;
use nix_compat::nixhash::NixHash;
use thiserror::Error;

use crate::{
    config::{DeriveKind, DeriveSpec, Strategy},
    fetch::{FetchError, Fetcher},
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
    /// Directory that relative flake references in `attr` resolve against.
    pub flake_dir: &'a Path,
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
                self.fod(attr, src).await.map(Output::Hash)
            }
            Strategy::Lockfile => self.lockfile(spec, src).await,
        }
    }

    /// Builds `attr` with `src` and an empty output hash, and reads the real
    /// hash from the mismatch error.
    async fn fod(&self, attr: &str, src: &Path) -> Result<NixHash> {
        let apply = format!(
            "drv: (drv.overrideAttrs (old: {{ src = builtins.path {{ path = /. + {path}; name = \"source\"; }}; }} \
             // (if old ? hash then {{ hash = \"\"; }} else {{ outputHash = \"\"; outputHashAlgo = \"sha256\"; }}))).drvPath",
            path = nix_string(&src.to_string_lossy()),
        );
        let eval = self
            .nix(&["eval", "--impure", "--raw", attr, "--apply", &apply])
            .await?;
        if !eval.status.success() {
            return Err(DeriveError::Nix(format!(
                "cannot evaluate `{attr}`:\n{}",
                tail(&eval.stderr)
            )));
        }
        let drv = format!("{}^out", String::from_utf8_lossy(&eval.stdout).trim());
        let build = self.nix(&["build", "--no-link", &drv]).await?;
        if build.status.success() {
            return Err(DeriveError::Nix(format!(
                "`{attr}` built with an empty hash; it must be a fixed-output derivation"
            )));
        }
        let stderr = String::from_utf8_lossy(&build.stderr);
        let got = stderr
            .lines()
            .find_map(|l| l.trim().strip_prefix("got:"))
            .map(str::trim)
            .ok_or_else(|| {
                DeriveError::Nix(format!("cannot build `{attr}`:\n{}", tail(&build.stderr)))
            })?;
        NixHash::from_sri(got)
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

    async fn nix(&self, args: &[&str]) -> Result<std::process::Output> {
        tokio::process::Command::new("nix")
            .args(args)
            .current_dir(self.flake_dir)
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|source| DeriveError::Io {
                context: "cannot run nix".into(),
                source,
            })
    }
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
    fn escapes_nix_strings() {
        assert_eq!(nix_string(r#"/a"b\${c}"#), r#""/a\"b\\\${c}""#);
    }
}
