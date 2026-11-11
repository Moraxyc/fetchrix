//! Upstream version sources.

mod git;
mod version;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use miette::Diagnostic;
use reqwest::{Url, header};
use serde::{Deserialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::sync::Semaphore;

use crate::config::VersionSpec;

/// Concurrent requests allowed per host.
const PER_HOST: usize = 4;

#[derive(Debug, Error, Diagnostic)]
pub enum SourceError {
    #[error("cannot create HTTP client")]
    #[diagnostic(code(fetchrix::source::client))]
    Client(#[source] reqwest::Error),
    #[error("invalid URL `{url}`: {message}")]
    #[diagnostic(code(fetchrix::source::url))]
    Url { url: String, message: String },
    #[error("request to {url} failed")]
    #[diagnostic(code(fetchrix::source::http))]
    Http {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("GitHub API rate limit exceeded")]
    #[diagnostic(
        code(fetchrix::source::rate_limit),
        help("set GITHUB_TOKEN or pass --github-token")
    )]
    GithubRateLimit,
    #[error("unexpected response: {0}")]
    #[diagnostic(code(fetchrix::source::decode))]
    Decode(String),
    #[error("{0}")]
    #[diagnostic(code(fetchrix::source::no_match))]
    NoMatch(String),
}

type Result<T, E = SourceError> = std::result::Result<T, E>;

pub struct Client {
    http: reqwest::Client,
    hosts: Mutex<HashMap<String, Arc<Semaphore>>>,
    github_token: Option<String>,
}

impl Client {
    /// Creates a client; `github_token` authenticates GitHub API requests.
    ///
    /// # Errors
    ///
    /// Returns an error if the TLS backend cannot be initialized.
    pub fn new(github_token: Option<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("fetchrix/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(SourceError::Client)?;
        Ok(Self {
            http,
            hosts: Mutex::default(),
            github_token,
        })
    }

    /// Queries the latest upstream version described by `spec`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or no version is found.
    pub async fn latest(&self, spec: &VersionSpec) -> Result<String> {
        match endpoint(spec) {
            Some(url) => extract(spec, &self.get(&url).await?),
            None => extract(spec, &[]),
        }
    }

    async fn get(&self, url: &str) -> Result<Vec<u8>> {
        let parsed = Url::parse(url).map_err(|e| SourceError::Url {
            url: url.to_owned(),
            message: e.to_string(),
        })?;
        let host = parsed.host_str().unwrap_or_default().to_owned();
        let semaphore = self
            .hosts
            .lock()
            .expect("no code panics while holding the hosts lock")
            .entry(host.clone())
            .or_insert_with(|| Arc::new(Semaphore::new(PER_HOST)))
            .clone();
        let _permit = semaphore
            .acquire()
            .await
            .expect("host semaphores are never closed");

        let mut request = self.http.get(parsed);
        if host == "api.github.com" {
            request = request.header(header::ACCEPT, "application/vnd.github+json");
            if let Some(token) = &self.github_token {
                request = request.bearer_auth(token);
            }
        }
        let http = |source| SourceError::Http {
            url: url.to_owned(),
            source,
        };
        let response = request.send().await.map_err(http)?;
        let status = response.status();
        if host == "api.github.com"
            && self.github_token.is_none()
            && matches!(status.as_u16(), 403 | 429)
        {
            return Err(SourceError::GithubRateLimit);
        }
        let response = response.error_for_status().map_err(http)?;
        Ok(response.bytes().await.map_err(http)?.to_vec())
    }
}

fn endpoint(spec: &VersionSpec) -> Option<String> {
    Some(match spec {
        VersionSpec::GithubRelease { repo, .. } => {
            format!("https://api.github.com/repos/{repo}/releases/latest")
        }
        VersionSpec::GithubTag { repo, .. } => git::refs_url(&format!("https://github.com/{repo}")),
        VersionSpec::Git { url, .. } => git::refs_url(url),
        VersionSpec::Pypi { name } => format!("https://pypi.org/pypi/{name}/json"),
        VersionSpec::Crates { name } => format!("https://crates.io/api/v1/crates/{name}"),
        VersionSpec::Npm { name } => format!("https://registry.npmjs.org/{name}/latest"),
        VersionSpec::Regex { url, .. } => url.clone(),
        VersionSpec::Manual { .. } => return None,
    })
}

fn extract(spec: &VersionSpec, body: &[u8]) -> Result<String> {
    match spec {
        VersionSpec::GithubRelease { strip_prefix, .. } => {
            #[derive(Deserialize)]
            struct Release {
                tag_name: String,
            }
            let tag = json::<Release>(body)?.tag_name;
            strip(&tag, strip_prefix.as_deref())
                .map(str::to_owned)
                .ok_or_else(|| {
                    SourceError::NoMatch(format!("release tag `{tag}` lacks the prefix"))
                })
        }
        VersionSpec::GithubTag { strip_prefix, .. } => {
            let refs = git::parse_refs(body)?;
            let tags = refs
                .iter()
                .filter_map(|r| r.name.strip_prefix("refs/tags/"))
                .filter(|t| !t.ends_with("^{}"))
                .filter_map(|t| strip(t, strip_prefix.as_deref()))
                .filter(|t| t.starts_with(|c: char| c.is_ascii_digit()));
            version::max(tags)
                .map(str::to_owned)
                .ok_or_else(|| SourceError::NoMatch("no matching tag".into()))
        }
        VersionSpec::Git { branch, .. } => {
            let target = branch
                .as_ref()
                .map_or_else(|| "HEAD".to_owned(), |b| format!("refs/heads/{b}"));
            git::parse_refs(body)?
                .into_iter()
                .find(|r| r.name == target)
                .map(|r| r.oid)
                .ok_or_else(|| SourceError::NoMatch(format!("ref `{target}` not found")))
        }
        VersionSpec::Pypi { .. } => {
            #[derive(Deserialize)]
            struct Response {
                info: Info,
            }
            #[derive(Deserialize)]
            struct Info {
                version: String,
            }
            Ok(json::<Response>(body)?.info.version)
        }
        VersionSpec::Crates { .. } => {
            #[derive(Deserialize)]
            struct Response {
                #[serde(rename = "crate")]
                krate: Crate,
            }
            #[derive(Deserialize)]
            struct Crate {
                max_stable_version: Option<String>,
                max_version: String,
            }
            let c = json::<Response>(body)?.krate;
            Ok(c.max_stable_version.unwrap_or(c.max_version))
        }
        VersionSpec::Npm { .. } => {
            #[derive(Deserialize)]
            struct Response {
                version: String,
            }
            Ok(json::<Response>(body)?.version)
        }
        VersionSpec::Regex { regex, .. } => {
            let text = String::from_utf8_lossy(body);
            let regex = regex.as_regex();
            let matches = regex
                .captures_iter(&text)
                .filter_map(|c| c.get(1).or_else(|| c.get(0)).map(|m| m.as_str()));
            version::max(matches).map(str::to_owned).ok_or_else(|| {
                SourceError::NoMatch(format!("`{}` matched nothing", regex.as_str()))
            })
        }
        VersionSpec::Manual { version } => Ok(version.clone()),
    }
}

fn strip<'a>(tag: &'a str, prefix: Option<&str>) -> Option<&'a str> {
    match prefix {
        Some(p) => tag.strip_prefix(p),
        None => Some(tag),
    }
}

fn json<T: DeserializeOwned>(body: &[u8]) -> Result<T> {
    serde_json::from_slice(body).map_err(|e| SourceError::Decode(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GitHubRepo, Pattern};

    const REFS: &[u8] = include_bytes!("fixtures/ripgrep-refs.bin");

    fn repo() -> GitHubRepo {
        "BurntSushi/ripgrep".parse().unwrap()
    }

    #[test]
    fn extracts_json_sources() {
        let cases = [
            (
                VersionSpec::GithubRelease {
                    repo: repo(),
                    strip_prefix: None,
                },
                &include_bytes!("fixtures/github-release.json")[..],
                "15.2.0",
            ),
            (
                VersionSpec::Pypi {
                    name: "requests".into(),
                },
                include_bytes!("fixtures/pypi.json"),
                "2.34.2",
            ),
            (
                VersionSpec::Crates {
                    name: "serde".into(),
                },
                include_bytes!("fixtures/crates.json"),
                "1.0.229",
            ),
            (
                VersionSpec::Npm {
                    name: "typescript".into(),
                },
                include_bytes!("fixtures/npm.json"),
                "7.0.2",
            ),
        ];
        for (spec, body, want) in cases {
            assert_eq!(extract(&spec, body).unwrap(), want, "{spec:?}");
        }
    }

    #[test]
    fn picks_highest_tag() {
        let spec = VersionSpec::GithubTag {
            repo: repo(),
            strip_prefix: None,
        };
        assert_eq!(extract(&spec, REFS).unwrap(), "15.2.0");
        let spec = VersionSpec::GithubTag {
            repo: repo(),
            strip_prefix: Some("0.1.".into()),
        };
        assert_eq!(extract(&spec, REFS).unwrap(), "18");
    }

    #[test]
    fn resolves_branch_commit() {
        let head = "178cc027e283fd965cb72d0a7395b219eada170c";
        for branch in [None, Some("master".into())] {
            let spec = VersionSpec::Git {
                url: "https://github.com/BurntSushi/ripgrep".into(),
                branch,
            };
            assert_eq!(extract(&spec, REFS).unwrap(), head);
        }
        let spec = VersionSpec::Git {
            url: String::new(),
            branch: Some("missing".into()),
        };
        assert!(matches!(extract(&spec, REFS), Err(SourceError::NoMatch(_))));
    }

    #[test]
    fn regex_takes_highest_capture() {
        let spec = VersionSpec::Regex {
            url: String::new(),
            regex: Pattern::try_from(r"foo-([\d.]+)\.tar".to_owned()).unwrap(),
        };
        let page = b"foo-1.9.tar foo-1.10.tar foo-1.2.tar";
        assert_eq!(extract(&spec, page).unwrap(), "1.10");
    }

    #[tokio::test]
    #[ignore = "network"]
    async fn queries_live_sources() {
        let client = Client::new(std::env::var("GITHUB_TOKEN").ok()).unwrap();
        for spec in [
            VersionSpec::GithubTag {
                repo: repo(),
                strip_prefix: None,
            },
            VersionSpec::Pypi {
                name: "requests".into(),
            },
            VersionSpec::Crates {
                name: "serde".into(),
            },
            VersionSpec::Npm {
                name: "typescript".into(),
            },
        ] {
            assert!(!client.latest(&spec).await.unwrap().is_empty(), "{spec:?}");
        }
    }
}
