//! Final per-package report of a command.

use std::fmt::{self, Write as _};

use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Report {
    pub command: &'static str,
    pub packages: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub name: String,
    #[serde(flatten)]
    pub status: Status,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum Status {
    Unchanged {
        version: String,
    },
    /// A new version, or a refreshed entry when `old` equals `new`; `old` is
    /// absent for a package not locked before.
    Changed {
        old: Option<String>,
        new: String,
    },
    Removed {
        old: String,
    },
    Verified {
        version: String,
    },
    Failed {
        error: String,
    },
}

impl Status {
    #[must_use]
    pub fn changed(old: Option<&str>, new: String) -> Self {
        match old {
            Some(old) if old == new => Self::Unchanged { version: new },
            old => Self::Changed {
                old: old.map(str::to_owned),
                new,
            },
        }
    }

    /// Old and new version columns.
    #[must_use]
    pub fn versions(&self) -> (Option<&str>, Option<&str>) {
        match self {
            Self::Unchanged { version } | Self::Verified { version } => {
                (Some(version), Some(version))
            }
            Self::Changed { old, new } => (old.as_deref(), Some(new)),
            Self::Removed { old } => (Some(old), None),
            Self::Failed { .. } => (None, None),
        }
    }

    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Unchanged { .. } => "unchanged",
            Self::Changed { .. } => "changed",
            Self::Removed { .. } => "removed",
            Self::Verified { .. } => "verified",
            Self::Failed { .. } => "failed",
        }
    }
}

impl Report {
    /// Entries that change the output directory.
    pub fn changes(&self) -> impl Iterator<Item = &Entry> {
        self.packages
            .iter()
            .filter(|e| matches!(e.status, Status::Changed { .. } | Status::Removed { .. }))
    }

    #[must_use]
    pub fn failures(&self) -> usize {
        self.packages
            .iter()
            .filter(|e| matches!(e.status, Status::Failed { .. }))
            .count()
    }

    /// Commit message listing every change, or `None` without changes.
    #[must_use]
    pub fn commit_message(&self) -> Option<String> {
        let changes: Vec<_> = self.changes().collect();
        match changes.as_slice() {
            [] => None,
            [one] => Some(format!("Update {one}")),
            many => {
                let mut message = format!("Update {} packages\n\n", many.len());
                for entry in many {
                    writeln!(message, "- {entry}").expect("writing to a String does not fail");
                }
                Some(message)
            }
        }
    }
}

/// One-line human summary; failures are rendered separately as diagnostics.
impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = &self.name;
        match &self.status {
            Status::Unchanged { version } => write!(f, "{name}: {version}"),
            Status::Changed { old: None, new } => write!(f, "{name}: {new} (new)"),
            Status::Changed {
                old: Some(old),
                new,
            } if old == new => write!(f, "{name}: {new} (refreshed)"),
            Status::Changed {
                old: Some(old),
                new,
            } => write!(f, "{name}: {old} -> {new}"),
            Status::Removed { old } => write!(f, "{name}: {old} (removed)"),
            Status::Verified { version } => write!(f, "{name}: {version} ok"),
            Status::Failed { error } => write!(f, "{name}: failed: {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, status: Status) -> Entry {
        Entry {
            name: name.into(),
            status,
        }
    }

    fn report(packages: Vec<Entry>) -> Report {
        Report {
            command: "update",
            packages,
        }
    }

    #[test]
    fn no_changes_no_commit() {
        let r = report(vec![
            entry(
                "a",
                Status::Unchanged {
                    version: "1".into(),
                },
            ),
            entry("b", Status::Failed { error: "x".into() }),
        ]);
        assert_eq!(r.commit_message(), None);
        assert_eq!(r.failures(), 1);
    }

    #[test]
    fn single_change_is_subject() {
        let r = report(vec![entry("a", Status::changed(Some("1"), "2".into()))]);
        assert_eq!(r.commit_message().as_deref(), Some("Update a: 1 -> 2"));
    }

    #[test]
    fn lists_every_change() {
        let r = report(vec![
            entry("a", Status::changed(Some("1"), "2".into())),
            entry("b", Status::changed(None, "3".into())),
            entry(
                "c",
                Status::Unchanged {
                    version: "1".into(),
                },
            ),
            entry(
                "d",
                Status::Changed {
                    old: Some("4".into()),
                    new: "4".into(),
                },
            ),
            entry("e", Status::Removed { old: "5".into() }),
        ]);
        assert_eq!(
            r.commit_message().as_deref(),
            Some(
                "Update 4 packages\n\n- a: 1 -> 2\n- b: 3 (new)\n- d: 4 (refreshed)\n- e: 5 (removed)\n"
            )
        );
    }

    #[test]
    fn json_shape() {
        let r = report(vec![entry("a", Status::changed(None, "2".into()))]);
        assert_eq!(
            serde_json::to_value(&r).expect("reports serialize"),
            serde_json::json!({
                "command": "update",
                "packages": [{ "name": "a", "status": "changed", "old": null, "new": "2" }],
            })
        );
    }
}
