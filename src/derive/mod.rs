//! Hashes and lock files derived from a fetched source.

use std::path::Path;

use miette::Diagnostic;
use nix_compat::nixhash::NixHash;
use thiserror::Error;

use crate::config::{DeriveKind, DeriveSpec, Strategy};

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
    #[error("{0} is not supported yet")]
    #[diagnostic(code(fetchrix::derive::unsupported))]
    Unsupported(&'static str),
}

type Result<T, E = DeriveError> = std::result::Result<T, E>;

pub enum Output {
    Hash(NixHash),
    Lockfile {
        name: &'static str,
        content: Vec<u8>,
    },
}

pub struct Deriver<'a> {
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
            Strategy::Lockfile => lockfile(spec, src).await,
            Strategy::Native => Err(DeriveError::Unsupported("`strategy = \"native\"`")),
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

async fn lockfile(spec: &DeriveSpec, src: &Path) -> Result<Output> {
    let name = match spec.kind {
        DeriveKind::CargoVendor => "Cargo.lock",
        DeriveKind::NpmDeps => "package-lock.json",
        DeriveKind::GoVendor => return Err(DeriveError::Unsupported("`lockfile` for Go")),
    };
    let path = src
        .join(spec.sub_dir.as_deref().unwrap_or_default())
        .join(name);
    let content = tokio::fs::read(&path)
        .await
        .map_err(|source| DeriveError::Io {
            context: format!("cannot read {name} from the source"),
            source,
        })?;
    Ok(Output::Lockfile { name, content })
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
    fn escapes_nix_strings() {
        assert_eq!(nix_string(r#"/a"b\${c}"#), r#""/a\"b\\\${c}""#);
    }
}
