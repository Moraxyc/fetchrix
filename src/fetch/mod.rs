//! Source downloads and their Nix hashes.

mod archive;
mod nar;

use std::path::{Path, PathBuf};

use miette::Diagnostic;
use nix_compat::nixhash::{NixHash, Sha256Digester};
use serde::Deserialize;
use thiserror::Error;

use crate::{
    config::FetchSpec,
    http::{Client, HttpError},
    lock::LockedSource,
};

#[derive(Debug, Error, Diagnostic)]
pub enum FetchError {
    #[error(transparent)]
    #[diagnostic(transparent)]
    Http(#[from] HttpError),
    #[error("{context}")]
    #[diagnostic(code(fetchrix::fetch::io))]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    #[diagnostic(code(fetchrix::fetch::archive))]
    Archive(String),
    #[error("nix-prefetch-git failed: {0}")]
    #[diagnostic(code(fetchrix::fetch::prefetch))]
    Prefetch(String),
}

type Result<T, E = FetchError> = std::result::Result<T, E>;

fn io(context: impl Into<String>) -> impl FnOnce(std::io::Error) -> FetchError {
    let context = context.into();
    move |source| FetchError::Io { context, source }
}

pub struct Fetcher<'a> {
    pub client: &'a Client,
    /// Flake reference that provides `nix-prefetch-git`.
    pub nixpkgs: &'a str,
    /// Directory whose flake inputs resolve `nixpkgs`.
    pub flake_dir: &'a Path,
}

impl Fetcher<'_> {
    /// Downloads the source described by `spec` at `version` and hashes it.
    ///
    /// # Errors
    ///
    /// Returns an error if the download, unpacking, or prefetch fails.
    pub async fn fetch(&self, spec: &FetchSpec, version: &str) -> Result<LockedSource> {
        match spec {
            FetchSpec::Url { url } => {
                let url = url.render(version);
                let hash = self.flat_hash(&url).await?;
                Ok(LockedSource::Url {
                    url,
                    hash: hash.into(),
                })
            }
            FetchSpec::Tarball { url } => {
                let url = url.render(version);
                let hash = self.unpacked_hash(&url).await?;
                Ok(LockedSource::Tarball {
                    url,
                    hash: hash.into(),
                })
            }
            FetchSpec::Github {
                repo,
                rev,
                submodules,
                leave_dot_git,
            } => {
                let rev = rev.render(version);
                let hash = if *submodules || *leave_dot_git {
                    let url = format!("https://github.com/{repo}.git");
                    self.prefetch_git(&url, &rev, *submodules, *leave_dot_git, &[])
                        .await?
                } else {
                    let url = format!("https://github.com/{repo}/archive/{rev}.tar.gz");
                    self.unpacked_hash(&url).await?
                };
                Ok(LockedSource::Github {
                    owner: repo.owner.clone(),
                    repo: repo.name.clone(),
                    rev,
                    hash: hash.into(),
                    fetch_submodules: *submodules,
                    leave_dot_git: *leave_dot_git,
                })
            }
            FetchSpec::Git {
                url,
                rev,
                submodules,
                leave_dot_git,
                sparse_checkout,
            } => {
                let rev = rev.render(version);
                let hash = self
                    .prefetch_git(url, &rev, *submodules, *leave_dot_git, sparse_checkout)
                    .await?;
                Ok(LockedSource::Git {
                    url: url.clone(),
                    rev,
                    hash: hash.into(),
                    fetch_submodules: *submodules,
                    leave_dot_git: *leave_dot_git,
                    sparse_checkout: sparse_checkout.clone(),
                })
            }
        }
    }

    async fn flat_hash(&self, url: &str) -> Result<NixHash> {
        let body = self.client.get(url).await?;
        let mut digester = Sha256Digester::new();
        digester.update(&body);
        Ok(digester.finalize().into())
    }

    async fn unpacked_hash(&self, url: &str) -> Result<NixHash> {
        let work = tempfile::tempdir().map_err(io("cannot create a temporary directory"))?;
        let archive_path = work.path().join("archive");
        let mut file = tokio::fs::File::create(&archive_path)
            .await
            .map_err(io("cannot create a temporary file"))?;
        self.client.download(url, &mut file).await?;
        drop(file);
        let out = work.path().join("out");
        tokio::task::spawn_blocking(move || {
            let root = archive::unpack(&archive_path, &out)?;
            nar::hash(&root).map_err(io(format!("cannot hash {}", root.display())))
        })
        .await
        .expect("unpacking does not panic")
    }

    async fn prefetch_git(
        &self,
        url: &str,
        rev: &str,
        submodules: bool,
        leave_dot_git: bool,
        sparse_checkout: &[String],
    ) -> Result<NixHash> {
        #[derive(Deserialize)]
        struct Output {
            hash: String,
        }
        let mut command = tokio::process::Command::new("nix");
        command
            .arg("run")
            .arg("--inputs-from")
            .arg(self.flake_dir)
            .arg(format!("{}#nix-prefetch-git", self.nixpkgs))
            .args(["--", "--quiet", "--url", url, "--rev", rev]);
        if submodules {
            command.arg("--fetch-submodules");
        }
        if leave_dot_git {
            command.arg("--leave-dotGit");
        }
        if !sparse_checkout.is_empty() {
            command
                .arg("--sparse-checkout")
                .arg(sparse_checkout.join("\n"));
        }
        let output = command
            .kill_on_drop(true)
            .output()
            .await
            .map_err(io("cannot run nix"))?;
        if !output.status.success() {
            return Err(FetchError::Prefetch(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        let parsed: Output = serde_json::from_slice(&output.stdout)
            .map_err(|e| FetchError::Prefetch(format!("unexpected output: {e}")))?;
        NixHash::from_sri(&parsed.hash)
            .map_err(|e| FetchError::Prefetch(format!("unexpected hash: {e}")))
    }
}

/// The directory a lock file's `nix` commands resolve flake inputs from.
#[must_use]
pub fn flake_dir(config: &Path) -> PathBuf {
    let dir = config.parent().unwrap_or(Path::new(""));
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    std::path::absolute(dir).unwrap_or_else(|_| dir.to_owned())
}
