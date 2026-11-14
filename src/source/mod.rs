//! Upstream version sources.

mod git;
mod version;

use miette::Diagnostic;
use serde::{Deserialize, de::DeserializeOwned};
use thiserror::Error;

use crate::{
    config::{ManifestFormat, Pattern, VersionSpec},
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
    #[error("the fetched source has no commit date for an unstable version")]
    #[diagnostic(
        code(fetchrix::source::no_date),
        help("use a fetcher that runs nix-prefetch-git, such as `type = \"git\"`")
    )]
    NoDate,
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

/// Formats an unstable version as nixpkgs does: `<stable>-unstable-<date>`.
///
/// # Errors
///
/// Returns an error if `date` is unknown.
pub fn unstable(stable: &str, date: Option<&str>) -> Result<String> {
    let date = date.ok_or(SourceError::NoDate)?;
    Ok(format!("{stable}-unstable-{date}"))
}

fn endpoint(spec: &VersionSpec) -> Option<String> {
    Some(match spec {
        VersionSpec::GithubRelease { repo, .. } => {
            format!("https://api.github.com/repos/{repo}/releases/latest")
        }
        VersionSpec::GithubTag { repo, .. } => git::refs_url(&format!("https://github.com/{repo}")),
        VersionSpec::Git { url, .. } | VersionSpec::GitTag { url, .. } => git::refs_url(url),
        VersionSpec::Pypi { name } => format!("https://pypi.org/pypi/{name}/json"),
        VersionSpec::Crates { name } => format!("https://crates.io/api/v1/crates/{name}"),
        VersionSpec::Npm { name } => format!("https://registry.npmjs.org/{name}/latest"),
        VersionSpec::Regex { url, .. } | VersionSpec::Manifest { url, .. } => url.clone(),
        VersionSpec::Manual { .. } => return None,
    })
}

fn extract(spec: &VersionSpec, body: &[u8]) -> Result<String> {
    match spec {
        VersionSpec::GithubRelease {
            strip_prefix,
            filter,
            ..
        } => {
            #[derive(Deserialize)]
            struct Release {
                tag_name: String,
            }
            let tag = json::<Release>(body)?.tag_name;
            if let Some(pat) = filter
                && !pat.as_regex().is_match(&tag)
            {
                return Err(SourceError::NoMatch(format!(
                    "release tag `{tag}` does not match filter"
                )));
            }
            strip(&tag, strip_prefix.as_deref())
                .map(str::to_owned)
                .ok_or_else(|| {
                    SourceError::NoMatch(format!("release tag `{tag}` lacks the prefix"))
                })
        }
        VersionSpec::GithubTag {
            strip_prefix,
            filter,
            ..
        }
        | VersionSpec::GitTag {
            strip_prefix,
            filter,
            ..
        } => latest_tag(body, strip_prefix.as_deref(), filter.as_ref()),
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
        VersionSpec::Manifest {
            format,
            field,
            strip_prefix,
            ..
        } => manifest(*format, field, strip_prefix.as_deref(), body),
        VersionSpec::Manual { version } => Ok(version.clone()),
    }
}

fn latest_tag(body: &[u8], strip_prefix: Option<&str>, filter: Option<&Pattern>) -> Result<String> {
    let refs = git::parse_refs(body)?;
    let tags = refs
        .iter()
        .filter_map(|r| r.name.strip_prefix("refs/tags/"))
        .filter(|t| !t.ends_with("^{}"))
        .filter(|t| filter.is_none_or(|pat| pat.as_regex().is_match(t)))
        .filter_map(|t| strip(t, strip_prefix))
        .filter(|t| t.starts_with(|c: char| c.is_ascii_digit()));
    version::max(tags)
        .map(str::to_owned)
        .ok_or_else(|| SourceError::NoMatch("no matching tag".into()))
}

fn manifest(
    format: ManifestFormat,
    field: &str,
    strip_prefix: Option<&str>,
    body: &[u8],
) -> Result<String> {
    let raw = match format {
        ManifestFormat::Json => {
            let v: serde_json::Value = json(body)?;
            v.get(field)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| {
                    SourceError::NoMatch(format!(
                        "field `{field}` not found or not a string in JSON manifest"
                    ))
                })?
        }
        ManifestFormat::Yaml => {
            let text = String::from_utf8_lossy(body);
            yaml_top_level(&text, field)
                .map(str::to_owned)
                .ok_or_else(|| {
                    SourceError::NoMatch(format!("field `{field}` not found in YAML manifest"))
                })?
        }
    };
    strip(&raw, strip_prefix)
        .map(str::to_owned)
        .ok_or_else(|| SourceError::NoMatch(format!("manifest version `{raw}` lacks the prefix")))
}

/// Extracts a top-level string value from a minimal YAML document.
///
/// Only handles top-level `key: value` lines where the value is an unquoted
/// or single-/double-quoted scalar. This is sufficient for update manifests
/// produced by Electron Builder and similar tools.
fn yaml_top_level<'a>(body: &'a str, field: &str) -> Option<&'a str> {
    for line in body.lines() {
        let line = line.trim();
        // Skip comments and blank lines.
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        // We only want top-level keys; nested keys are indented.
        let Some(rest) = line.strip_prefix(field) else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix(':') else {
            continue;
        };
        let value = rest.trim();
        // Strip surrounding quotes if present.
        let value = if (value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\''))
        {
            &value[1..value.len() - 1]
        } else {
            value
        };
        return Some(value);
    }
    None
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
        assert!(matches!(
            extract(&spec, include_bytes!("fixtures/github-release.json")),
            Err(SourceError::NoMatch(_))
        ));
    }

    #[test]
    fn resolves_branch_commit() {
        let head = "178cc027e283fd965cb72d0a7395b219eada170c";
        for branch in [None, Some("master".into())] {
            let spec = VersionSpec::Git {
                url: "https://github.com/BurntSushi/ripgrep".into(),
                branch,
                unstable: None,
            };
            assert_eq!(extract(&spec, REFS).unwrap(), head);
        }
        let spec = VersionSpec::Git {
            url: String::new(),
            branch: Some("missing".into()),
            unstable: None,
        };
        assert!(matches!(extract(&spec, REFS), Err(SourceError::NoMatch(_))));
    }

    #[test]
    fn formats_unstable_version() {
        assert_eq!(
            unstable("0", Some("2024-01-15")).unwrap(),
            "0-unstable-2024-01-15"
        );
        assert!(matches!(unstable("1.0", None), Err(SourceError::NoDate)));
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

    #[test]
    fn extracts_yaml_manifest() {
        let spec = VersionSpec::Manifest {
            url: String::new(),
            format: crate::config::ManifestFormat::Yaml,
            field: "version".into(),
            strip_prefix: None,
        };
        assert_eq!(
            extract(&spec, include_bytes!("fixtures/electron-latest.yml")).unwrap(),
            "33.4.5"
        );
    }

    #[test]
    fn extracts_yaml_manifest_with_strip() {
        let spec = VersionSpec::Manifest {
            url: String::new(),
            format: crate::config::ManifestFormat::Yaml,
            field: "version".into(),
            strip_prefix: Some("33.".into()),
        };
        assert_eq!(
            extract(&spec, include_bytes!("fixtures/electron-latest.yml")).unwrap(),
            "4.5"
        );
    }

    #[test]
    fn extracts_json_manifest() {
        let spec = VersionSpec::Manifest {
            url: String::new(),
            format: crate::config::ManifestFormat::Json,
            field: "version".into(),
            strip_prefix: None,
        };
        assert_eq!(
            extract(&spec, include_bytes!("fixtures/update-manifest.json")).unwrap(),
            "1.5.0"
        );
    }

    #[test]
    fn manifest_missing_field_errors() {
        let spec = VersionSpec::Manifest {
            url: String::new(),
            format: crate::config::ManifestFormat::Yaml,
            field: "nonexistent".into(),
            strip_prefix: None,
        };
        assert!(matches!(
            extract(&spec, include_bytes!("fixtures/electron-latest.yml")),
            Err(SourceError::NoMatch(_))
        ));
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
