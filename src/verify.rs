//! Comparison of a lock entry against freshly computed hashes.

use std::collections::{BTreeMap, BTreeSet};

use miette::Diagnostic;
use thiserror::Error;

use crate::lock::{Derived, Hash, LockedPackage};

#[derive(Debug, Error, Diagnostic)]
pub enum VerifyError {
    #[error("not in the lock file")]
    #[diagnostic(code(fetchrix::verify::not_locked), help("run `fetchrix update`"))]
    NotLocked,
    #[error("the lock entry is stale: its configuration changed")]
    #[diagnostic(code(fetchrix::verify::stale), help("run `fetchrix update`"))]
    Stale,
    #[error("recorded hashes do not match:\n{}", .0.join("\n"))]
    #[diagnostic(code(fetchrix::verify::mismatch))]
    Mismatch(Vec<String>),
}

/// Describes every difference between the `locked` entry and the `actual`
/// one computed for the same version.
#[must_use]
pub fn compare(locked: &LockedPackage, actual: &LockedPackage) -> Vec<String> {
    let mut diffs = Vec::new();
    if locked.src != actual.src {
        diffs.push(format!(
            "src: locked {}, got {}",
            locked.src.hash(),
            actual.src.hash()
        ));
    }
    if locked.artifacts != actual.artifacts {
        diffs.push("artifacts: the declared artifacts changed".to_owned());
    }
    let attrs: BTreeSet<_> = locked.derived.keys().chain(actual.derived.keys()).collect();
    for attr in attrs {
        match (locked.derived.get(attr), actual.derived.get(attr)) {
            (Some(Derived::GradleDeps(a)), Some(Derived::GradleDeps(b))) => {
                if a != b {
                    diffs.push(format!("{attr}: Gradle dependency data changed"));
                }
            }
            (Some(Derived::Hash(a)), Some(Derived::Hash(b))) => {
                if a != b {
                    diffs.push(format!("{attr}: locked {}, got {}", a.hash, b.hash));
                }
            }
            (Some(Derived::Lockfile(a)), Some(Derived::Lockfile(b))) => {
                if a.lockfile != b.lockfile {
                    diffs.push(format!(
                        "{attr}: locked lock file {}, got {}",
                        a.lockfile, b.lockfile
                    ));
                }
                compare_hashes(attr, &a.output_hashes, &b.output_hashes, &mut diffs);
            }
            (a, b) => diffs.push(format!(
                "{attr}: locked {}, got {}",
                describe(a),
                describe(b)
            )),
        }
    }
    diffs
}

pub(crate) fn gradle_integrity(locked: &LockedPackage, dir: &std::path::Path) -> Vec<String> {
    let mut diffs = Vec::new();
    for entry in locked.derived.values() {
        if let Derived::GradleDeps(entry) = entry {
            let path = dir.join(&entry.data);
            match std::fs::read(&path) {
                Ok(bytes) => {
                    let hash: Hash = nix_compat::nixhash::NixHash::from(
                        nix_compat::nixhash::Sha256::digest_bytes(&bytes),
                    )
                    .into();
                    if hash != entry.hash {
                        diffs.push(format!("{}: content hash mismatch", path.display()));
                    }
                }
                Err(e) => diffs.push(format!("{}: {e}", path.display())),
            }
        }
    }
    diffs
}

fn compare_hashes(
    attr: &str,
    locked: &BTreeMap<String, Hash>,
    actual: &BTreeMap<String, Hash>,
    diffs: &mut Vec<String>,
) {
    let keys: BTreeSet<_> = locked.keys().chain(actual.keys()).collect();
    for key in keys {
        let (a, b) = (locked.get(key), actual.get(key));
        if a != b {
            let show = |h: Option<&Hash>| h.map_or_else(|| "nothing".to_owned(), Hash::to_string);
            diffs.push(format!(
                "{attr}.outputHashes.{key}: locked {}, got {}",
                show(a),
                show(b)
            ));
        }
    }
}

fn describe(derived: Option<&Derived>) -> &'static str {
    match derived {
        Some(Derived::GradleDeps(_)) => "Gradle dependency data",
        Some(Derived::Hash(_)) => "a hash",
        Some(Derived::Lockfile(_)) => "a lock file",
        None => "nothing",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::{DerivedHash, DerivedLockfile, LockedSource};
    use nix_compat::nixhash::{NixHash, Sha256};

    fn hash(seed: &str) -> Hash {
        NixHash::from(Sha256::digest_bytes(seed)).into()
    }

    fn package(src: &str, vendor: &str, git: &str) -> LockedPackage {
        LockedPackage {
            pname: None,
            version: "1.0.0".into(),
            raw_version: "1.0.0".into(),
            date: None,
            src: LockedSource::Url {
                url: "https://example.org/foo.tar.gz".into(),
                hash: hash(src),
            },
            derived: BTreeMap::from([
                (
                    "vendorHash".into(),
                    Derived::Hash(DerivedHash { hash: hash(vendor) }),
                ),
                (
                    "cargoLock".into(),
                    Derived::Lockfile(DerivedLockfile {
                        lockfile: "foo/src/Cargo.lock".into(),
                        output_hashes: BTreeMap::from([("dep-0.1.0".into(), hash(git))]),
                    }),
                ),
            ]),
            artifacts: BTreeMap::new(),
            fingerprint: "f".into(),
        }
    }

    #[test]
    fn gradle_data_roundtrip_compare_and_integrity() {
        let dir = tempfile::tempdir().unwrap();
        let mut locked = package("a", "b", "c");
        locked.derived.insert(
            "gradleDeps".into(),
            Derived::GradleDeps(crate::lock::DerivedGradleDeps {
                data: "deps.json".into(),
                hash: hash("{}"),
            }),
        );
        let json = serde_json::to_vec(&locked).unwrap();
        let actual = serde_json::from_slice(&json).unwrap();
        assert_eq!(locked, actual);
        assert_eq!(compare(&locked, &actual), Vec::<String>::new());
        assert_eq!(gradle_integrity(&locked, dir.path()).len(), 1);
        std::fs::write(dir.path().join("deps.json"), "{}").unwrap();
        assert_eq!(gradle_integrity(&locked, dir.path()), Vec::<String>::new());
        std::fs::write(dir.path().join("deps.json"), "changed").unwrap();
        assert!(gradle_integrity(&locked, dir.path())[0].contains("hash mismatch"));
        let mut changed = actual;
        if let Derived::GradleDeps(entry) = changed.derived.get_mut("gradleDeps").unwrap() {
            entry.hash = hash("changed");
        }
        assert_eq!(
            compare(&locked, &changed),
            ["gradleDeps: Gradle dependency data changed"]
        );
    }

    #[test]
    fn equal_entries_match() {
        assert_eq!(
            compare(&package("a", "b", "c"), &package("a", "b", "c")),
            Vec::<String>::new()
        );
    }

    #[test]
    fn reports_each_mismatch() {
        let diffs = compare(&package("a", "b", "c"), &package("x", "y", "z"));
        let fields: Vec<_> = diffs
            .iter()
            .map(|d| d.split_once(':').expect("diffs name a field").0)
            .collect();
        assert_eq!(
            fields,
            ["src", "cargoLock.outputHashes.dep-0.1.0", "vendorHash"]
        );
    }

    #[test]
    fn reports_missing_derived() {
        let locked = package("a", "b", "c");
        let mut actual = locked.clone();
        actual.derived.remove("vendorHash");
        assert_eq!(
            compare(&locked, &actual),
            ["vendorHash: locked a hash, got nothing"]
        );
    }
}
