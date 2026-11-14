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
    #[error("invalid package name `{0}`")]
    #[diagnostic(
        code(fetchrix::config::name),
        help(
            "use ASCII letters, digits, `.`, `_`, and `-`, not starting with `.` and not `lock.json`"
        )
    )]
    InvalidName(String),
    #[error("package `{0}`: set exactly one of `rev` and `tag`")]
    #[diagnostic(code(fetchrix::config::fetch))]
    InvalidFetch(String),
    #[error("package `{package}`, prepare: {message}")]
    #[diagnostic(code(fetchrix::config::prepare))]
    InvalidPrepare { package: String, message: String },
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
    /// Nix `pname`; the package name when absent.
    pub pname: Option<String>,
    pub version: VersionSpec,
    /// Rewrites applied in order to the upstream version, or to the stable
    /// part of an unstable one; the fetch still uses the upstream version.
    #[serde(default)]
    pub transform: Vec<Transform>,
    pub fetch: FetchSpec,
    /// Changes to a copy of the fetched source before derive runs on it.
    pub prepare: Option<Prepare>,
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
        // Without `prepare` the tuple keeps the fingerprints of older entries.
        let canonical = match &self.prepare {
            Some(prepare) => serde_json::to_vec(&(&self.fetch, &self.derive, prepare)),
            None => serde_json::to_vec(&(&self.fetch, &self.derive)),
        }
        .expect("fetch, derive, and prepare settings always serialize");
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
        /// Only consider releases whose tag matches this regex.
        filter: Option<Pattern>,
    },
    GithubTag {
        repo: GitHubRepo,
        strip_prefix: Option<String>,
        /// Only consider tags that match this regex.
        filter: Option<Pattern>,
    },
    Git {
        url: String,
        branch: Option<String>,
        /// Version the commit as `<stable>-unstable-<date>` instead of its SHA.
        unstable: Option<Unstable>,
    },
    GitTag {
        url: String,
        strip_prefix: Option<String>,
        /// Only consider tags that match this regex.
        filter: Option<Pattern>,
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
    /// Fetches a structured update manifest (JSON or YAML) from a fixed URL and
    /// extracts the version from a named top-level field.
    Manifest {
        url: String,
        format: DataFormat,
        /// Top-level field name containing the version string; defaults to `"version"`.
        #[serde(default = "default_version_field")]
        field: String,
        strip_prefix: Option<String>,
    },
    Manual {
        version: String,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DataFormat {
    #[default]
    Json,
    Yaml,
}

fn default_version_field() -> String {
    "version".to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum Transform {
    /// Removes a leading `v` or `V`.
    StripV,
    StripPrefix(String),
    StripSuffix(String),
}

impl Transform {
    #[must_use]
    pub fn apply<'a>(&self, version: &'a str) -> &'a str {
        match self {
            Self::StripV => version.strip_prefix(['v', 'V']),
            Self::StripPrefix(p) => version.strip_prefix(p.as_str()),
            Self::StripSuffix(s) => version.strip_suffix(s.as_str()),
        }
        .unwrap_or(version)
    }
}

impl Package {
    /// Applies every `transform` to `version`.
    #[must_use]
    pub fn transform(&self, version: &str) -> String {
        self.transform
            .iter()
            .fold(version, |v, t| t.apply(v))
            .to_owned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unstable {
    /// Source of the `<stable>` part; `0` when absent.
    pub stable: Option<Box<VersionSpec>>,
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
        #[serde(skip_serializing_if = "Option::is_none")]
        rev: Option<Template>,
        /// Fetches `refs/tags/<tag>`; exclusive with `rev`.
        #[serde(skip_serializing_if = "Option::is_none")]
        tag: Option<Template>,
        #[serde(default)]
        submodules: bool,
        #[serde(default)]
        leave_dot_git: bool,
    },
    Git {
        url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        rev: Option<Template>,
        /// Fetches `refs/tags/<tag>`; exclusive with `rev`.
        #[serde(skip_serializing_if = "Option::is_none")]
        tag: Option<Template>,
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
pub struct Prepare {
    /// Run in order in a writable copy of the source.
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Files written to `<out>/<package>/<name>`, keyed by `name`.
    #[serde(default)]
    pub artifacts: BTreeMap<String, Artifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Step {
    /// Generates or completes `package-lock.json` without installing.
    NpmLock {
        sub_dir: Option<String>,
        #[serde(default)]
        args: Vec<String>,
    },
    /// Generates `composer.lock`, or refreshes the content hash of an
    /// existing one, without installing.
    ComposerLock {
        sub_dir: Option<String>,
        #[serde(default)]
        args: Vec<String>,
    },
    /// Downloads Go module dependencies and writes `go.sum`.
    GoModDownload {
        sub_dir: Option<String>,
        #[serde(default)]
        args: Vec<String>,
    },
    /// Generates `pnpm-lock.yaml` without installing.
    PnpmLock {
        sub_dir: Option<String>,
        #[serde(default)]
        args: Vec<String>,
    },
    /// Generates `Cargo.lock` without building.
    CargoGenerateLock {
        sub_dir: Option<String>,
        #[serde(default)]
        args: Vec<String>,
    },
    /// Runs a local program; `run[0]` is a path relative to the configuration
    /// file.
    Hook {
        run: Vec<String>,
        sub_dir: Option<String>,
    },
}

impl Step {
    #[must_use]
    pub fn sub_dir(&self) -> Option<&str> {
        match self {
            Self::NpmLock { sub_dir, .. }
            | Self::ComposerLock { sub_dir, .. }
            | Self::GoModDownload { sub_dir, .. }
            | Self::PnpmLock { sub_dir, .. }
            | Self::CargoGenerateLock { sub_dir, .. }
            | Self::Hook { sub_dir, .. } => sub_dir.as_deref(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Artifact {
    /// A file of the prepared source.
    File { path: String },
    /// The changes made by `steps`, as a `-p1` patch, limited to `paths`
    /// when given.
    Patch {
        #[serde(default)]
        paths: Vec<String>,
    },
    /// A JSON or YAML file of the prepared source as JSON, limited to
    /// `fields` when given.
    Json {
        path: String,
        #[serde(default)]
        format: DataFormat,
        #[serde(default)]
        fields: Vec<JsonPointer>,
    },
}

/// An RFC 6901 JSON pointer such as `/dependencies/foo`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct JsonPointer(String);

impl JsonPointer {
    /// The unescaped object keys along the pointer.
    pub fn keys(&self) -> impl Iterator<Item = String> {
        self.0[1..]
            .split('/')
            .map(|key| key.replace("~1", "/").replace("~0", "~"))
    }
}

impl fmt::Display for JsonPointer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<JsonPointer> for String {
    fn from(p: JsonPointer) -> Self {
        p.0
    }
}

impl TryFrom<String> for JsonPointer {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        let escapes_valid = s
            .match_indices('~')
            .all(|(i, _)| matches!(s.as_bytes().get(i + 1), Some(b'0' | b'1')));
        if !s.starts_with('/') || !escapes_valid {
            return Err(format!(
                "`{s}` is not a JSON pointer such as `/dependencies`; escape `~` as `~0` and `/` as `~1`"
            ));
        }
        Ok(Self(s))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeriveSpec {
    /// Ecosystem of the derived output; unused by `fod`.
    pub kind: Option<DeriveKind>,
    pub strategy: Strategy,
    pub sub_dir: Option<String>,
    /// A flake installable, or an attribute path in `file` when set.
    pub attr: Option<String>,
    /// Nix file for `fod`, relative to the configuration, or a lookup path
    /// such as `<nixpkgs>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Attribute path of the FOD in the package that `attr`, a function of
    /// `{ src, version, artifacts }`, returns.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeriveKind {
    NpmDeps,
    CargoVendor,
    GoModules,
    PnpmDeps,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Strategy {
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
            let valid_name = !package.starts_with('.')
                && package != crate::lock::FILE
                && package
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
            if !valid_name {
                return Err(ConfigError::InvalidName(package.clone()));
            }
            if let FetchSpec::Github { rev, tag, .. } | FetchSpec::Git { rev, tag, .. } =
                &spec.fetch
                && rev.is_some() == tag.is_some()
            {
                return Err(ConfigError::InvalidFetch(package.clone()));
            }
            if let Some(prepare) = &spec.prepare {
                validate_prepare(package, prepare)?;
            }
            for (derive, d) in &spec.derive {
                let message = match (d.strategy, &d.attr, d.kind) {
                    (Strategy::Fod, None, _) => Some("`strategy = \"fod\"` requires `attr`"),
                    (Strategy::Lockfile, Some(_), _) => {
                        Some("`attr` is only used with `strategy = \"fod\"`")
                    }
                    (Strategy::Lockfile, ..) if d.file.is_some() => {
                        Some("`file` is only used with `strategy = \"fod\"`")
                    }
                    (Strategy::Lockfile, ..) if d.select.is_some() => {
                        Some("`select` is only used with `strategy = \"fod\"`")
                    }
                    _ if !d
                        .select
                        .as_deref()
                        .is_none_or(|s| s.split('.').all(|segment| !segment.is_empty())) =>
                    {
                        Some("`select` must be an attribute path such as `goModules`")
                    }
                    (Strategy::Lockfile, _, None) => {
                        Some("`strategy = \"lockfile\"` requires `kind`")
                    }
                    _ if !d.sub_dir.as_deref().is_none_or(is_relative_dir) => Some(
                        "`sub_dir` must be a relative path without `.`, `..`, or empty segments",
                    ),
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

fn validate_prepare(package: &str, prepare: &Prepare) -> Result<(), ConfigError> {
    let invalid = |message| ConfigError::InvalidPrepare {
        package: package.to_owned(),
        message,
    };
    for step in &prepare.steps {
        if !step.sub_dir().is_none_or(is_relative_dir) {
            return Err(invalid(
                "step `sub_dir` must be a relative path without `.`, `..`, or empty segments"
                    .into(),
            ));
        }
        if let Step::Hook { run, .. } = step
            && !run.first().is_some_and(|program| program.contains('/'))
        {
            return Err(invalid(
                "hook `run` must start with a path such as `./scripts/prepare.sh`".into(),
            ));
        }
    }
    for (name, artifact) in &prepare.artifacts {
        if !is_file_name(name) || name == crate::lock::SRC_DIR {
            return Err(invalid(format!(
                "invalid artifact name `{name}`; use a file name other than `{}`",
                crate::lock::SRC_DIR
            )));
        }
        let paths = match artifact {
            Artifact::File { path } | Artifact::Json { path, .. } => std::slice::from_ref(path),
            Artifact::Patch { paths } => paths.as_slice(),
        };
        if !paths.iter().all(|p| is_relative_dir(p)) {
            return Err(invalid(format!(
                "artifact `{name}`: paths must be relative without `.`, `..`, or empty segments"
            )));
        }
    }
    Ok(())
}

/// Whether `name` is usable as a file name in the output directory.
fn is_file_name(name: &str) -> bool {
    !name.starts_with('.')
        && !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Whether `dir` stays inside the tree it is joined to; extracted lock files
/// are written under the same relative path.
fn is_relative_dir(dir: &str) -> bool {
    dir.split('/')
        .all(|segment| !matches!(segment, "" | "." | ".."))
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
            fetch   = { type = "github", repo = "owner/foo", tag = "v{version}" }

            [packages.foo.derive.vendorHash]
            strategy = "fod"
            attr = ".#foo.goModules"
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
                strip_prefix: Some("v".into()),
                filter: None,
            }
        );
        let FetchSpec::Github {
            repo: r,
            tag: Some(tag),
            rev: None,
            ..
        } = &foo.fetch
        else {
            panic!("expected github fetch by tag");
        };
        assert_eq!(r, &repo);
        assert_eq!(tag.render("1.2.3"), "v1.2.3");
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
    fn parses_unstable_git() {
        let config = parse(
            r#"
            [packages.foo]
            version = { source = "git", url = "https://example.org/foo.git", unstable = { stable = { source = "manual", version = "1.0" } } }
            fetch = { type = "git", url = "https://example.org/foo.git", rev = "{version}" }
            "#,
        )
        .unwrap();
        let VersionSpec::Git {
            unstable: Some(u), ..
        } = &config.packages["foo"].version
        else {
            panic!("expected unstable git source");
        };
        assert_eq!(
            u.stable.as_deref(),
            Some(&VersionSpec::Manual {
                version: "1.0".into()
            })
        );
    }

    #[test]
    fn applies_transforms_in_order() {
        let config = parse(
            r#"
            [packages.foo]
            version = { source = "manual", version = "Vrelease-1.0-final" }
            transform = ["strip-v", { strip-prefix = "release-" }, { strip-suffix = "-final" }, "strip-v"]
            fetch = { type = "url", url = "https://example.org/{version}" }
            "#,
        )
        .unwrap();
        let foo = &config.packages["foo"];
        assert_eq!(foo.transform("Vrelease-1.0-final"), "1.0");
        assert_eq!(foo.transform("2.0"), "2.0");
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

    #[test]
    fn file_is_only_for_fod() {
        let text = |strategy| {
            format!(
                r#"
                [packages.foo]
                version = {{ source = "manual", version = "1" }}
                fetch = {{ type = "url", url = "https://example.org" }}
                [packages.foo.derive.vendorHash]
                kind = "go-modules"
                strategy = "{strategy}"
                file = "default.nix"
                {}
                "#,
                if strategy == "fod" {
                    r#"attr = "foo.goModules""#
                } else {
                    ""
                }
            )
        };
        let config = parse(&text("fod")).unwrap();
        assert_eq!(
            config.packages["foo"].derive["vendorHash"].file.as_deref(),
            Some("default.nix")
        );
        let err = parse(&text("lockfile")).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }), "{err:?}");
    }

    #[test]
    fn fingerprint_ignores_absent_file() {
        let spec = DeriveSpec {
            kind: None,
            strategy: Strategy::Fod,
            sub_dir: None,
            attr: Some(".#foo".into()),
            file: None,
            select: None,
        };
        assert_eq!(
            serde_json::to_string(&spec).unwrap(),
            r#"{"kind":null,"strategy":"fod","sub_dir":null,"attr":".#foo"}"#
        );
    }

    #[test]
    fn select_is_an_attr_path_for_fod() {
        let text = |strategy, select| {
            format!(
                r#"
                [packages.foo]
                version = {{ source = "manual", version = "1" }}
                fetch = {{ type = "url", url = "https://example.org" }}
                [packages.foo.derive.vendorHash]
                kind = "go-modules"
                strategy = "{strategy}"
                select = "{select}"
                {}
                "#,
                if strategy == "fod" {
                    r#"attr = "foo""#
                } else {
                    ""
                }
            )
        };
        let config = parse(&text("fod", "passthru.goModules")).unwrap();
        assert_eq!(
            config.packages["foo"].derive["vendorHash"]
                .select
                .as_deref(),
            Some("passthru.goModules")
        );
        for (strategy, select) in [("fod", ""), ("fod", "a..b"), ("lockfile", "goModules")] {
            let err = parse(&text(strategy, select)).unwrap_err();
            assert!(matches!(err, ConfigError::Invalid { .. }), "{err:?}");
        }
    }

    #[test]
    fn requires_exactly_one_of_rev_and_tag() {
        for refs in ["", r#", rev = "a", tag = "b""#] {
            let err = parse(&format!(
                r#"
                [packages.foo]
                version = {{ source = "manual", version = "1" }}
                fetch = {{ type = "git", url = "https://example.org/foo.git"{refs} }}
                "#
            ))
            .unwrap_err();
            assert!(matches!(err, ConfigError::InvalidFetch(_)), "{err:?}");
        }
    }

    #[test]
    fn parses_prepare() {
        let config = parse(
            r#"
            [packages.foo]
            version = { source = "manual", version = "1" }
            fetch = { type = "url", url = "https://example.org" }
            [packages.foo.prepare]
            steps = [
              { type = "npm-lock", sub_dir = "web", args = ["--legacy-peer-deps"] },
              { type = "composer-lock" },
              { type = "hook", run = ["./fix.sh", "x"] },
            ]
            artifacts."fix.patch" = { type = "patch", paths = ["web/package.json"] }
            artifacts."composer.lock" = { type = "file", path = "composer.lock" }
            artifacts."pubspec.lock.json" = { type = "json", path = "pubspec.lock", format = "yaml" }
            artifacts."deps.json" = { type = "json", path = "package.json", fields = ["/a~1b~0c/d"] }
            "#,
        )
        .unwrap();
        let prepare = config.packages["foo"].prepare.as_ref().unwrap();
        assert_eq!(prepare.steps.len(), 3);
        assert_eq!(prepare.steps[0].sub_dir(), Some("web"));
        assert_eq!(prepare.artifacts.len(), 4);
        assert!(matches!(
            prepare.artifacts["pubspec.lock.json"],
            Artifact::Json {
                format: DataFormat::Yaml,
                ..
            }
        ));
        let Artifact::Json { format, fields, .. } = &prepare.artifacts["deps.json"] else {
            panic!("`deps.json` is a json artifact");
        };
        assert_eq!(*format, DataFormat::Json);
        assert_eq!(fields[0].keys().collect::<Vec<_>>(), ["a/b~c", "d"]);
    }

    #[test]
    fn rejects_invalid_json_pointer() {
        for field in ["", "version", "/a~2", "/a~"] {
            let err = parse(&format!(
                r#"
                [packages.foo]
                version = {{ source = "manual", version = "1" }}
                fetch = {{ type = "url", url = "https://example.org" }}
                [packages.foo.prepare]
                artifacts."a.json" = {{ type = "json", path = "a.json", fields = ["{field}"] }}
                "#
            ))
            .unwrap_err();
            assert!(matches!(err, ConfigError::Parse { .. }), "{field}: {err:?}");
        }
    }

    #[test]
    fn rejects_invalid_prepare() {
        for prepare in [
            r#"steps = [{ type = "hook", run = ["bash"] }]"#,
            r#"steps = [{ type = "npm-lock", sub_dir = "../x" }]"#,
            r#"artifacts.src = { type = "file", path = "a" }"#,
            r#"artifacts."a/b" = { type = "file", path = "a" }"#,
            r#"artifacts.p = { type = "patch", paths = ["/etc"] }"#,
            r#"artifacts."a.json" = { type = "json", path = "../a.json" }"#,
        ] {
            let err = parse(&format!(
                r#"
                [packages.foo]
                version = {{ source = "manual", version = "1" }}
                fetch = {{ type = "url", url = "https://example.org" }}
                [packages.foo.prepare]
                {prepare}
                "#
            ))
            .unwrap_err();
            assert!(matches!(err, ConfigError::InvalidPrepare { .. }), "{err:?}");
        }
    }

    #[test]
    fn fingerprint_ignores_absent_prepare() {
        let text = r#"
            [packages.foo]
            version = { source = "manual", version = "1" }
            fetch = { type = "url", url = "https://example.org" }
        "#;
        let plain = parse(text).unwrap().packages["foo"].fingerprint();
        let canonical = serde_json::to_vec(&(
            &FetchSpec::Url {
                url: Template("https://example.org".into()),
            },
            &BTreeMap::<String, DeriveSpec>::new(),
        ))
        .unwrap();
        let expected = nix_compat::nixhash::NixHash::from(
            nix_compat::nixhash::Sha256::digest_bytes(canonical),
        )
        .to_sri_string();
        assert_eq!(plain, expected);
        let prepared = parse(&format!("{text}\n[packages.foo.prepare]\n"))
            .unwrap()
            .packages["foo"]
            .fingerprint();
        assert_ne!(plain, prepared);
    }

    #[test]
    fn sub_dir_stays_inside_source() {
        assert!(is_relative_dir("cmd/foo"));
        for dir in ["", "/abs", "../up", "a/../b", "a//b", "a/", "./a"] {
            assert!(!is_relative_dir(dir), "{dir}");
        }
    }
}
