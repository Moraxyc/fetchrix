//! Upstream version sources.

mod git;
mod version;

use miette::Diagnostic;
use serde::{Deserialize, de::DeserializeOwned};
use thiserror::Error;

use crate::{
    config::VersionSpec,
    http::{Client, HttpError},
};

#[derive(Debug, Error, Diagnostic)]
pub enum SourceError {
    #[error(transparent)]
    #[diagnostic(transparent)]
    Http(#[from] HttpError),
    #[error("unexpected response: {0}")]
    #[diagnostic(code(fetchrix::source::decode))]
    Decode(String),
    #[error("{0}")]
    #[diagnostic(code(fetchrix::source::no_match))]
    NoMatch(String),
}

type Result<T, E = SourceError> = std::result::Result<T, E>;

/// Queries the latest upstream version described by `spec`.
///
/// # Errors
///
/// Returns an error if the request fails or no version is found.
pub async fn latest(client: &Client, spec: &VersionSpec) -> Result<String> {
    let version = match endpoint(spec) {
        Some(url) => extract(spec, &client.get(&url).await?)?,
        None => extract(spec, &[])?,
    };
    Ok(version)
}

fn endpoint(spec: &VersionSpec) -> Option<String> {
    Some(match spec {
        VersionSpec::GithubRelease { repo, .. } => {
            format!("https://api.github.com/repos/{repo}/releases/latest")
        }
        VersionSpec::GithubTag { repo, .. } => git::refs_url(&format!("https://github.com/{repo}")),
        VersionSpec::Git { url, .. } => git::refs_url(url),
        VersionSpec::GitTag { url, .. } => git::refs_url(url),
        VersionSpec::Pypi { name } => format!("https://pypi.org/pypi/{name}/json"),
        VersionSpec::Crates { name } => format!("https://crates.io/api/v1/crates/{name}"),
        VersionSpec::Npm { name } => format!("https://registry.npmjs.org/{name}/latest"),
        VersionSpec::Regex { url, .. } => url.clone(),
        VersionSpec::Manual { .. } => return None,
    })
}

fn extract(spec: &VersionSpec, body: &[u8]) -> Result<String> {
    match spec {
        VersionSpec::GithubRelease {
            strip_prefix, filter, ..
        } => {
            #[derive(Deserialize)]
            struct Release {
                tag_name: String,
            }
            let tag = json::<Release>(body)?.tag_name;
            if let Some(pat) = filter {
                if !pat.as_regex().is_match(&tag) {
                    return Err(SourceError::NoMatch(format!(
                        "release tag `{tag}` does not match filter"
                    )));
                }
            }
            strip(&tag, strip_prefix.as_deref())
                .map(str::to_owned)
                .ok_or_else(|| {
                    SourceError::NoMatch(format!("release tag `{tag}` lacks the prefix"))
                })
        }
        VersionSpec::GithubTag {
            strip_prefix, filter, ..
        } => {
            let refs = git::parse_refs(body)?;
            let tags = refs
                .iter()
                .filter_map(|r| r.name.strip_prefix("refs/tags/"))
                .filter(|t| !t.ends_with("^{}"))
                .filter(|t| {
                    filter
                        .as_ref()
                        .map_or(true, |pat| pat.as_regex().is_match(t))
                })
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
        VersionSpec::GitTag {
            strip_prefix, filter, ..
        } => {
            let refs = git::parse_refs(body)?;
            let tags = refs
                .iter()
                .filter_map(|r| r.name.strip_prefix("refs/tags/"))
                .filter(|t| !t.ends_with("^{}"))
                .filter(|t| {
                    filter
                        .as_ref()
                        .map_or(true, |pat| pat.as_regex().is_match(t))
                })
                .filter_map(|t| strip(t, strip_prefix.as_deref()))
                .filter(|t| t.starts_with(|c: char| c.is_ascii_digit()));
            version::max(tags)
                .map(str::to_owned)
                .ok_or_else(|| SourceError::NoMatch("no matching tag".into()))
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
                    filter: None,
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
            filter: None,
        };
        assert_eq!(extract(&spec, REFS).unwrap(), "15.2.0");
        let spec = VersionSpec::GithubTag {
            repo: repo(),
            strip_prefix: Some("0.1.".into()),
            filter: None,
        };
        assert_eq!(extract(&spec, REFS).unwrap(), "18");
    }

    #[test]
    fn filter_excludes_non_matching_tags() {
        let filter = Some(Pattern::try_from(r"^\d+\.\d+\.\d+$".to_owned()).unwrap());
        let spec = VersionSpec::GithubTag {
            repo: repo(),
            strip_prefix: None,
            filter,
        };
        assert_eq!(extract(&spec, REFS).unwrap(), "15.2.0");
    }

    #[test]
    fn git_tag_picks_highest() {
        let spec = VersionSpec::GitTag {
            url: "https://github.com/BurntSushi/ripgrep".into(),
            strip_prefix: None,
            filter: None,
        };
        assert_eq!(extract(&spec, REFS).unwrap(), "15.2.0");
        let spec = VersionSpec::GitTag {
            url: "https://github.com/BurntSushi/ripgrep".into(),
            strip_prefix: Some("0.1.".into()),
            filter: None,
        };
        assert_eq!(extract(&spec, REFS).unwrap(), "18");
    }

    #[test]
    fn release_filter_rejects_non_matching() {
        let filter = Some(Pattern::try_from(r"^v".to_owned()).unwrap());
        let spec = VersionSpec::GithubRelease {
            repo: repo(),
            strip_prefix: None,
            filter,
        };
        // The fixture tag is "15.2.0" (no leading v), so filter should reject it.
        assert!(matches!(extract(&spec, include_bytes!("fixtures/github-release.json")), Err(SourceError::NoMatch(_))));
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
        let client = Client::new(crate::auth::Credentials::load(&[]).unwrap()).unwrap();
        for spec in [
            VersionSpec::GithubTag {
                repo: repo(),
                strip_prefix: None,
                filter: None,
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
            assert!(
                !latest(&client, &spec).await.unwrap().is_empty(),
                "{spec:?}"
            );
        }
    }
}
