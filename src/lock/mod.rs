//! Lock file model and atomic writes.

use std::{collections::BTreeMap, fmt, io::Write, path::Path};

use miette::{Diagnostic, NamedSource, SourceOffset};
use nix_compat::nixhash::NixHash;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SCHEMA: u32 = 1;
/// Name of the lock file inside the output directory.
pub const FILE: &str = "lock.json";
/// Directory inside `<out>/<package>/` that mirrors files copied from the
/// source; siblings are left for files fetchrix generates.
pub const SRC_DIR: &str = "src";

#[derive(Debug, Error, Diagnostic)]
pub enum LockError {
    #[error("cannot access {path}")]
    #[diagnostic(code(fetchrix::lock::io))]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid lock file")]
    #[diagnostic(code(fetchrix::lock::parse))]
    Parse {
        #[source_code]
        src: NamedSource<String>,
        #[label("{message}")]
        offset: SourceOffset,
        message: String,
    },
    #[error("unsupported lock schema {found}, expected {SCHEMA}")]
    #[diagnostic(code(fetchrix::lock::schema))]
    Schema { found: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lock {
    pub schema: u32,
    pub packages: BTreeMap<String, LockedPackage>,
}

impl Default for Lock {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            packages: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedPackage {
    pub version: String,
    pub src: LockedSource,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub derived: BTreeMap<String, Derived>,
    pub fingerprint: String,
}

/// Field names match the arguments of the corresponding Nix fetchers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "fetcher",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum LockedSource {
    Url {
        url: String,
        hash: Hash,
    },
    Tarball {
        url: String,
        hash: Hash,
    },
    Github {
        owner: String,
        repo: String,
        rev: String,
        hash: Hash,
        #[serde(default, skip_serializing_if = "is_false")]
        fetch_submodules: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        leave_dot_git: bool,
    },
    Git {
        url: String,
        rev: String,
        hash: Hash,
        #[serde(default, skip_serializing_if = "is_false")]
        fetch_submodules: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        leave_dot_git: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        sparse_checkout: Vec<String>,
    },
}

impl LockedSource {
    #[must_use]
    pub fn hash(&self) -> &Hash {
        match self {
            Self::Url { hash, .. }
            | Self::Tarball { hash, .. }
            | Self::Github { hash, .. }
            | Self::Git { hash, .. } => hash,
        }
    }
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
fn is_false(b: &bool) -> bool {
    !b
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Derived {
    Hash(DerivedHash),
    Lockfile(DerivedLockfile),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DerivedHash {
    pub hash: Hash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DerivedLockfile {
    /// `<package>/src/<path in the source>`, relative to the directory of the
    /// lock file. Its file name selects the consumer; companion files sit
    /// beside it.
    pub lockfile: String,
    /// `importCargoLock` hashes of git dependencies, keyed by `name-version`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub output_hashes: BTreeMap<String, Hash>,
}

/// A hash written in SRI form, such as `sha256-<base64>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Hash(NixHash);

impl From<NixHash> for Hash {
    fn from(h: NixHash) -> Self {
        Self(h)
    }
}

impl TryFrom<String> for Hash {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        NixHash::from_sri(&s)
            .map(Self)
            .map_err(|e| format!("`{s}`: {e}"))
    }
}

impl From<Hash> for String {
    fn from(h: Hash) -> Self {
        h.0.to_sri_string()
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.write_sri_str(f)
    }
}

impl Lock {
    /// Reads the lock file at `path`; a missing file yields an empty lock.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read, is malformed, or uses
    /// another schema.
    pub fn load(path: &Path) -> Result<Self, LockError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&path.display().to_string(), text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(LockError::Io {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    /// Parses lock file text; `name` labels diagnostics.
    ///
    /// # Errors
    ///
    /// Returns an error if the text is malformed or uses another schema.
    pub fn parse(name: &str, text: String) -> Result<Self, LockError> {
        #[derive(Deserialize)]
        struct Header {
            schema: u32,
        }
        let parse_error = |e: &serde_json::Error, text: String| LockError::Parse {
            offset: SourceOffset::from_location(&text, e.line(), e.column()),
            message: e.to_string(),
            src: NamedSource::new(name, text),
        };
        let header: Header =
            serde_json::from_str(&text).map_err(|e| parse_error(&e, text.clone()))?;
        if header.schema != SCHEMA {
            return Err(LockError::Schema {
                found: header.schema,
            });
        }
        serde_json::from_str(&text).map_err(|e| parse_error(&e, text))
    }

    /// Atomically replaces the lock file at `path`.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    ///
    /// # Panics
    ///
    /// Never; lock types always serialize to JSON.
    pub fn save(&self, path: &Path) -> Result<(), LockError> {
        let mut text = serde_json::to_string_pretty(self).expect("lock types always serialize");
        text.push('\n');
        write_atomic(path, text.as_bytes())
    }
}

/// Replaces `path` with `content` so readers never see a partial file.
///
/// # Errors
///
/// Returns an error if the file cannot be written.
pub fn write_atomic(path: &Path, content: &[u8]) -> Result<(), LockError> {
    let io = |source| LockError::Io {
        path: path.display().to_string(),
        source,
    };
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(io)?;
    tmp.write_all(content).map_err(io)?;
    tmp.as_file().sync_all().map_err(io)?;
    tmp.persist(path).map_err(|e| io(e.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";

    #[test]
    fn parses_nix_lib_fixture() {
        let text = include_str!("../../nix/tests/_sources/lock.json");
        let lock = Lock::parse(FILE, text.to_owned()).expect("the fixture is valid");
        let Derived::Lockfile(cargo) = &lock.packages["foo"].derived["cargoLock"] else {
            panic!("cargoLock is an extracted lock file");
        };
        assert_eq!(cargo.output_hashes.len(), 1);
        let Derived::Lockfile(npm) = &lock.packages["web"].derived["npmDeps"] else {
            panic!("npmDeps is an extracted lock file");
        };
        assert_eq!(npm.lockfile, "web/src/frontend/package-lock.json");
    }

    fn sample() -> String {
        format!(
            r#"{{
  "schema": 1,
  "packages": {{
    "foo": {{
      "version": "1.2.3",
      "src": {{
        "fetcher": "github",
        "owner": "owner",
        "repo": "foo",
        "rev": "v1.2.3",
        "hash": "{HASH}",
        "fetchSubmodules": true
      }},
      "derived": {{
        "vendorHash": {{
          "hash": "{HASH}"
        }}
      }},
      "fingerprint": "abc"
    }}
  }}
}}
"#
        )
    }

    #[test]
    fn round_trips_through_file() {
        let lock = Lock::parse("test", sample()).unwrap();
        let LockedSource::Github {
            fetch_submodules,
            leave_dot_git,
            ..
        } = &lock.packages["foo"].src
        else {
            panic!("expected github source");
        };
        assert!(fetch_submodules);
        assert!(!leave_dot_git);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fetchrix.lock");
        lock.save(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), sample());
        assert_eq!(Lock::load(&path).unwrap(), lock);
    }

    #[test]
    fn missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            Lock::load(&dir.path().join("none")).unwrap(),
            Lock::default()
        );
    }

    #[test]
    fn rejects_other_schema() {
        let err = Lock::parse("test", r#"{"schema": 2, "future": true}"#.into()).unwrap_err();
        assert!(matches!(err, LockError::Schema { found: 2 }), "{err:?}");
    }
}
