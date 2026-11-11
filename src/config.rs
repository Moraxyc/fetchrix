//! Configuration file schema.

use std::{collections::BTreeMap, fmt, path::Path, str::FromStr};

use miette::{Diagnostic, NamedSource, SourceSpan};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, Diagnostic)]
pub enum ConfigError {
    #[error("cannot read {path}")]
    #[diagnostic(code(fetchrix::config::io))]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid configuration")]
    #[diagnostic(code(fetchrix::config::parse))]
    Parse {
        #[source_code]
        src: NamedSource<String>,
        #[label("{message}")]
        span: Option<SourceSpan>,
        message: String,
    },
    #[error("package `{package}`, derive `{derive}`: {message}")]
    #[diagnostic(code(fetchrix::config::invalid))]
    Invalid {
        package: String,
        derive: String,
        message: &'static str,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub nix: NixConfig,
    #[serde(default)]
    pub packages: BTreeMap<String, Package>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NixConfig {
    pub nixpkgs: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    pub version: VersionSpec,
    pub fetch: FetchSpec,
    #[serde(default)]
    pub derive: BTreeMap<String, DeriveSpec>,
}

impl Package {
    /// Hash of the settings that determine the locked source and derived hashes.
    ///
    /// # Panics
    ///
    /// Never; the settings always serialize to JSON.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let canonical = serde_json::to_vec(&(&self.fetch, &self.derive))
            .expect("fetch and derive settings always serialize");
        nix_compat::nixhash::NixHash::from(nix_compat::nixhash::Sha256::digest_bytes(canonical))
            .to_sri_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "source", rename_all = "kebab-case", deny_unknown_fields)]
pub enum VersionSpec {
    GithubRelease {
        repo: GitHubRepo,
        strip_prefix: Option<String>,
    },
    GithubTag {
        repo: GitHubRepo,
        strip_prefix: Option<String>,
    },
    Git {
        url: String,
        branch: Option<String>,
    },
    Pypi {
        name: String,
    },
    Crates {
        name: String,
    },
    Npm {
        name: String,
    },
    Regex {
        url: String,
        regex: Pattern,
    },
    Manual {
        version: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum FetchSpec {
    Url {
        url: Template,
    },
    Tarball {
        url: Template,
    },
    Github {
        repo: GitHubRepo,
        rev: Template,
        #[serde(default)]
        submodules: bool,
        #[serde(default)]
        leave_dot_git: bool,
    },
    Git {
        url: String,
        rev: Template,
        #[serde(default)]
        submodules: bool,
        #[serde(default)]
        leave_dot_git: bool,
        #[serde(default)]
        sparse_checkout: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeriveSpec {
    pub kind: DeriveKind,
    pub strategy: Strategy,
    pub toolchain: Option<String>,
    pub sub_dir: Option<String>,
    pub attr: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeriveKind {
    GoVendor,
    NpmDeps,
    CargoVendor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Strategy {
    Native,
    Lockfile,
    Fod,
}

/// A GitHub repository written as `owner/name`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct GitHubRepo {
    pub owner: String,
    pub name: String,
}

impl FromStr for GitHubRepo {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.split_once('/') {
            Some((owner, name)) if !owner.is_empty() && !name.is_empty() && !name.contains('/') => {
                Ok(Self {
                    owner: owner.to_owned(),
                    name: name.to_owned(),
                })
            }
            _ => Err(format!("expected `owner/name`, got `{s}`")),
        }
    }
}

impl TryFrom<String> for GitHubRepo {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<GitHubRepo> for String {
    fn from(repo: GitHubRepo) -> Self {
        repo.to_string()
    }
}

impl fmt::Display for GitHubRepo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

/// A regular expression; the first capture group, if any, is the version.
#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "String")]
pub struct Pattern(regex::Regex);

impl Pattern {
    #[must_use]
    pub fn as_regex(&self) -> &regex::Regex {
        &self.0
    }
}

impl PartialEq for Pattern {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_str() == other.0.as_str()
    }
}

impl Eq for Pattern {}

impl TryFrom<String> for Pattern {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        regex::Regex::new(&s).map(Self).map_err(|e| e.to_string())
    }
}

/// A string where `{version}` is replaced by the package version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Template(String);

impl From<Template> for String {
    fn from(t: Template) -> Self {
        t.0
    }
}

impl Template {
    const PLACEHOLDER: &str = "{version}";

    #[must_use]
    pub fn render(&self, version: &str) -> String {
        self.0.replace(Self::PLACEHOLDER, version)
    }
}

impl TryFrom<String> for Template {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        let rest = s.replace(Self::PLACEHOLDER, "");
        if rest.contains(['{', '}']) {
            return Err(format!(
                "`{s}`: only `{}` may appear in braces",
                Self::PLACEHOLDER
            ));
        }
        Ok(Self(s))
    }
}

impl Config {
    /// Reads and validates the configuration at `path`.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or is invalid.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Self::parse(&path.display().to_string(), text)
    }

    /// Parses and validates configuration text; `name` labels diagnostics.
    ///
    /// # Errors
    ///
    /// Returns an error if the text is not a valid configuration.
    pub fn parse(name: &str, text: String) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(&text).map_err(|e| ConfigError::Parse {
            span: e.span().map(Into::into),
            message: e.message().to_owned(),
            src: NamedSource::new(name, text),
        })?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        for (package, spec) in &self.packages {
            for (derive, d) in &spec.derive {
                let message = match (d.strategy, &d.attr, d.kind) {
                    (Strategy::Fod, None, _) => Some("`strategy = \"fod\"` requires `attr`"),
                    (Strategy::Native | Strategy::Lockfile, Some(_), _) => {
                        Some("`attr` is only used with `strategy = \"fod\"`")
                    }
                    (Strategy::Lockfile, _, DeriveKind::GoVendor) => {
                        Some("`go-vendor` does not support `strategy = \"lockfile\"`")
                    }
                    _ => None,
                };
                if let Some(message) = message {
                    return Err(ConfigError::Invalid {
                        package: package.clone(),
                        derive: derive.clone(),
                        message,
                    });
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Config, ConfigError> {
        Config::parse("test.toml", text.to_owned())
    }

    #[test]
    fn parses_full_package() {
        let config = parse(
            r#"
            [nix]
            nixpkgs = "nixpkgs"

            [packages.foo]
            version = { source = "github-release", repo = "owner/foo", strip_prefix = "v" }
            fetch   = { type = "github", repo = "owner/foo", rev = "v{version}" }

            [packages.foo.derive.vendorHash]
            kind = "go-vendor"
            strategy = "native"
            sub_dir = "cmd/foo"
            "#,
        )
        .unwrap();
        assert_eq!(config.nix.nixpkgs.as_deref(), Some("nixpkgs"));
        let foo = &config.packages["foo"];
        let repo: GitHubRepo = "owner/foo".parse().unwrap();
        assert_eq!(
            foo.version,
            VersionSpec::GithubRelease {
                repo: repo.clone(),
                strip_prefix: Some("v".into())
            }
        );
        let FetchSpec::Github { repo: r, rev, .. } = &foo.fetch else {
            panic!("expected github fetch");
        };
        assert_eq!(r, &repo);
        assert_eq!(rev.render("1.2.3"), "v1.2.3");
        assert_eq!(foo.derive["vendorHash"].kind, DeriveKind::GoVendor);
    }

    #[test]
    fn rejects_unknown_field_with_span() {
        let err = parse(
            r#"
            [packages.foo]
            version = { source = "manual", version = "1", typo = 1 }
            fetch = { type = "url", url = "https://example.org/{version}" }
            "#,
        )
        .unwrap_err();
        let ConfigError::Parse { span, message, .. } = err else {
            panic!("expected parse error, got {err:?}");
        };
        assert!(span.is_some());
        assert!(message.contains("typo"), "{message}");
    }

    #[test]
    fn rejects_bad_repo() {
        let err = parse(
            r#"
            [packages.foo]
            version = { source = "github-tag", repo = "foo" }
            fetch = { type = "url", url = "https://example.org" }
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn rejects_unknown_placeholder() {
        assert!(Template::try_from("v{ver}".to_owned()).is_err());
        assert!(Template::try_from("v{version}".to_owned()).is_ok());
    }

    #[test]
    fn fod_requires_attr() {
        let err = parse(
            r#"
            [packages.foo]
            version = { source = "manual", version = "1" }
            fetch = { type = "url", url = "https://example.org" }
            [packages.foo.derive.pnpmDeps]
            kind = "npm-deps"
            strategy = "fod"
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }), "{err:?}");
    }
}
