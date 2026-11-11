//! Source downloads and their Nix hashes.

mod archive;
mod nar;

use std::path::{Path, PathBuf};

use miette::Diagnostic;
use nix_compat::nixhash::{NixHash, Sha256Digester};
use serde::Deserialize;
use tempfile::TempDir;
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

/// A hash, the path it covers, and the temporary directory holding that path.
type Local = (NixHash, PathBuf, Option<TempDir>);

/// A fetched source; temporary copies are removed on drop.
pub struct Fetched {
    pub source: LockedSource,
    /// The unpacked tree, or the file for `url` sources.
    pub path: PathBuf,
    _work: Option<TempDir>,
}

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
    pub async fn fetch(&self, spec: &FetchSpec, version: &str) -> Result<Fetched> {
        let (source, path, work) = match spec {
            FetchSpec::Url { url } => {
                let url = url.render(version);
                let (hash, path, work) = self.download_file(&url).await?;
                let hash = hash.into();
                (LockedSource::Url { url, hash }, path, work)
            }
            FetchSpec::Tarball { url } => {
                let url = url.render(version);
                let (hash, path, work) = self.download_tarball(&url).await?;
                let hash = hash.into();
                (LockedSource::Tarball { url, hash }, path, work)
            }
            FetchSpec::Github {
                repo,
                rev,
                submodules,
                leave_dot_git,
            } => {
                let rev = rev.render(version);
                let (hash, path, work) = if *submodules || *leave_dot_git {
                    let url = format!("https://github.com/{repo}.git");
                    self.prefetch_git(&url, &rev, *submodules, *leave_dot_git, &[])
                        .await?
                } else {
                    let url = format!("https://github.com/{repo}/archive/{rev}.tar.gz");
                    self.download_tarball(&url).await?
                };
                let source = LockedSource::Github {
                    owner: repo.owner.clone(),
                    repo: repo.name.clone(),
                    rev,
                    hash: hash.into(),
                    fetch_submodules: *submodules,
                    leave_dot_git: *leave_dot_git,
                };
                (source, path, work)
            }
            FetchSpec::Git {
                url,
                rev,
                submodules,
                leave_dot_git,
                sparse_checkout,
            } => {
                let rev = rev.render(version);
                let (hash, path, work) = self
                    .prefetch_git(url, &rev, *submodules, *leave_dot_git, sparse_checkout)
                    .await?;
                let source = LockedSource::Git {
                    url: url.clone(),
                    rev,
                    hash: hash.into(),
                    fetch_submodules: *submodules,
                    leave_dot_git: *leave_dot_git,
                    sparse_checkout: sparse_checkout.clone(),
                };
                (source, path, work)
            }
        };
        Ok(Fetched {
            source,
            path,
            _work: work,
        })
    }

    async fn download(&self, url: &str) -> Result<(TempDir, PathBuf)> {
        let work = tempfile::tempdir().map_err(io("cannot create a temporary directory"))?;
        let path = work.path().join("download");
        let mut file = tokio::fs::File::create(&path)
            .await
            .map_err(io("cannot create a temporary file"))?;
        self.client.download(url, &mut file).await?;
        Ok((work, path))
    }

    async fn download_file(&self, url: &str) -> Result<Local> {
        let (work, path) = self.download(url).await?;
        let file = path.clone();
        let hash = tokio::task::spawn_blocking(move || {
            let mut digester = Sha256Digester::new();
            std::io::copy(&mut std::fs::File::open(&file)?, &mut digester)?;
            Ok::<_, std::io::Error>(digester.finalize().into())
        })
        .await
        .expect("hashing does not panic")
        .map_err(io("cannot hash the download"))?;
        Ok((hash, path, Some(work)))
    }

    async fn download_tarball(&self, url: &str) -> Result<Local> {
        let (work, archive_path) = self.download(url).await?;
        let out = work.path().join("unpacked");
        let (hash, root) = tokio::task::spawn_blocking(move || {
            let root = archive::unpack(&archive_path, &out)?;
            let hash = nar::hash(&root).map_err(io(format!("cannot hash {}", root.display())))?;
            Ok::<_, FetchError>((hash, root))
        })
        .await
        .expect("unpacking does not panic")?;
        Ok((hash, root, Some(work)))
    }

    async fn prefetch_git(
        &self,
        url: &str,
        rev: &str,
        submodules: bool,
        leave_dot_git: bool,
        sparse_checkout: &[String],
    ) -> Result<Local> {
        #[derive(Deserialize)]
        struct Output {
            hash: String,
            path: PathBuf,
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
        let hash = NixHash::from_sri(&parsed.hash)
            .map_err(|e| FetchError::Prefetch(format!("unexpected hash: {e}")))?;
        Ok((hash, parsed.path, None))
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
