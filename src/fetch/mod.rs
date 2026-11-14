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
    nixpkgs::Nixpkgs,
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

/// A hash, the path it covers, the temporary directory holding that path,
/// and the commit and its date when available.
struct Local {
    hash: NixHash,
    path: PathBuf,
    work: Option<TempDir>,
    commit: Option<String>,
    date: Option<String>,
}

/// A fetched source; temporary copies are removed on drop.
pub struct Fetched {
    pub source: LockedSource,
    /// Commit SHA, populated for git sources and `git archive` tarballs such
    /// as GitHub's.
    pub commit: Option<String>,
    /// Commit date (`YYYY-MM-DD`), populated with `commit`. Dates from
    /// `nix-prefetch-git` use the committer's time zone; tarball dates are UTC.
    pub date: Option<String>,
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
    /// Provides `nix-prefetch-git`.
    pub nixpkgs: &'a Nixpkgs,
}

/// The ref Nix fetchers resolve: `tag` becomes `refs/tags/<tag>`.
fn git_ref(rev: Option<&str>, tag: Option<&str>) -> String {
    match (rev, tag) {
        (_, Some(tag)) => format!("refs/tags/{tag}"),
        (Some(rev), None) => rev.to_owned(),
        (None, None) => unreachable!("configuration validation requires `rev` or `tag`"),
    }
}

impl Fetcher<'_> {
    /// Downloads the source described by `spec` at `version` and hashes it.
    ///
    /// # Errors
    ///
    /// Returns an error if the download, unpacking, or prefetch fails.
    pub async fn fetch(&self, spec: &FetchSpec, version: &str) -> Result<Fetched> {
        let (source, local) = match spec {
            FetchSpec::Url { url } => {
                let url = url.render(version);
                let local = self.download_file(&url).await?;
                let hash = local.hash.clone().into();
                (LockedSource::Url { url, hash }, local)
            }
            FetchSpec::Tarball { url } => {
                let url = url.render(version);
                let local = self.download_tarball(&url).await?;
                let hash = local.hash.clone().into();
                (LockedSource::Tarball { url, hash }, local)
            }
            FetchSpec::Github {
                repo,
                rev,
                tag,
                submodules,
                leave_dot_git,
            } => {
                let rev = rev.as_ref().map(|t| t.render(version));
                let tag = tag.as_ref().map(|t| t.render(version));
                let git_ref = git_ref(rev.as_deref(), tag.as_deref());
                let local = if *submodules || *leave_dot_git {
                    let url = format!("https://github.com/{repo}.git");
                    self.prefetch_git(&url, &git_ref, *submodules, *leave_dot_git, &[])
                        .await?
                } else {
                    self.download_tarball(&format!(
                        "https://github.com/{repo}/archive/{git_ref}.tar.gz"
                    ))
                    .await?
                };
                let source = LockedSource::Github {
                    owner: repo.owner.clone(),
                    repo: repo.name.clone(),
                    rev,
                    tag,
                    hash: local.hash.clone().into(),
                    fetch_submodules: *submodules,
                    leave_dot_git: *leave_dot_git,
                };
                (source, local)
            }
            FetchSpec::Git {
                url,
                rev,
                tag,
                submodules,
                leave_dot_git,
                sparse_checkout,
            } => {
                let rev = rev.as_ref().map(|t| t.render(version));
                let tag = tag.as_ref().map(|t| t.render(version));
                let git_ref = git_ref(rev.as_deref(), tag.as_deref());
                let local = self
                    .prefetch_git(url, &git_ref, *submodules, *leave_dot_git, sparse_checkout)
                    .await?;
                let source = LockedSource::Git {
                    url: url.clone(),
                    rev,
                    tag,
                    hash: local.hash.clone().into(),
                    fetch_submodules: *submodules,
                    leave_dot_git: *leave_dot_git,
                    sparse_checkout: sparse_checkout.clone(),
                };
                (source, local)
            }
        };
        Ok(Fetched {
            source,
            commit: local.commit,
            date: local.date,
            path: local.path,
            _work: local.work,
        })
    }

    /// Hashes `url` at `rev` as `fetchgit` does with its default arguments.
    ///
    /// # Errors
    ///
    /// Returns an error if `nix-prefetch-git` fails.
    pub async fn fetchgit_hash(&self, url: &str, rev: &str) -> Result<NixHash> {
        Ok(self.prefetch_git(url, rev, true, false, &[]).await?.hash)
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
        Ok(Local {
            hash,
            path,
            work: Some(work),
            commit: None,
            date: None,
        })
    }

    async fn download_tarball(&self, url: &str) -> Result<Local> {
        let (work, archive_path) = self.download(url).await?;
        let out = work.path().join("unpacked");
        let (hash, unpacked) = tokio::task::spawn_blocking(move || {
            let unpacked = archive::unpack(&archive_path, &out)?;
            let hash = nar::hash(&unpacked.root)
                .map_err(io(format!("cannot hash {}", unpacked.root.display())))?;
            Ok::<_, FetchError>((hash, unpacked))
        })
        .await
        .expect("unpacking does not panic")?;
        Ok(Local {
            hash,
            path: unpacked.root,
            work: Some(work),
            commit: unpacked.commit,
            date: unpacked.date,
        })
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
            rev: String,
            date: Option<String>,
        }
        let mut command = tokio::process::Command::new("nix");
        command
            .arg("run")
            .args(self.nixpkgs.installable("nix-prefetch-git"))
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
        // `date` from nix-prefetch-git is an ISO 8601 timestamp; take YYYY-MM-DD.
        let date = parsed.date.and_then(|d| d.get(..10).map(str::to_owned));
        Ok(Local {
            hash,
            path: parsed.path,
            work: None,
            commit: Some(parsed.rev),
            date,
        })
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
