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
    let attrs: BTreeSet<_> = locked.derived.keys().chain(actual.derived.keys()).collect();
    for attr in attrs {
        match (locked.derived.get(attr), actual.derived.get(attr)) {
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
            version: "1.0.0".into(),
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
            fingerprint: "f".into(),
        }
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
