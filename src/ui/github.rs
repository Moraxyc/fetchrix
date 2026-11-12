//! GitHub Actions workflow commands and step summary.

use std::{fmt::Write as _, io::Write as _, path::Path};

use super::{
    UiError,
    report::{Report, Status},
};

/// Whether the process runs as a GitHub Actions step.
#[must_use]
pub fn detected() -> bool {
    std::env::var_os("GITHUB_ACTIONS").is_some_and(|v| v == "true")
}

/// Emits one error annotation per failure and appends the summary table to
/// `$GITHUB_STEP_SUMMARY` when set.
///
/// # Errors
///
/// Returns an error if the summary file cannot be written.
pub fn publish(report: &Report) -> Result<(), UiError> {
    for entry in &report.packages {
        if let Status::Failed { error } = &entry.status {
            eprintln!(
                "::error title={}::{}",
                escape_property(&format!("fetchrix: {}", entry.name)),
                escape_data(error)
            );
        }
    }
    let Some(path) = std::env::var_os("GITHUB_STEP_SUMMARY") else {
        return Ok(());
    };
    let path = Path::new(&path);
    let io = |source| UiError::Io {
        path: path.display().to_string(),
        source,
    };
    std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .and_then(|mut file| file.write_all(summary(report).as_bytes()))
        .map_err(io)
}

fn summary(report: &Report) -> String {
    let mut out = format!(
        "### fetchrix {}\n\n| Package | Old | New | Status |\n|---|---|---|---|\n",
        report.command
    );
    for entry in &report.packages {
        let (old, new) = entry.status.versions();
        let cell = |v: Option<&str>| v.map_or_else(String::new, |v| v.replace('|', "\\|"));
        writeln!(
            out,
            "| {} | {} | {} | {} |",
            entry.name,
            cell(old),
            cell(new),
            entry.status.label()
        )
        .expect("writing to a String does not fail");
    }
    out.push('\n');
    out
}

fn escape_data(s: &str) -> String {
    s.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

fn escape_property(s: &str) -> String {
    escape_data(s).replace(':', "%3A").replace(',', "%2C")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::report::Entry;

    #[test]
    fn escapes_workflow_commands() {
        assert_eq!(escape_data("50%\na"), "50%25%0Aa");
        assert_eq!(escape_property("a: b, c"), "a%3A b%2C c");
    }

    #[test]
    fn renders_summary_table() {
        let report = Report {
            command: "update",
            packages: vec![
                Entry {
                    name: "a".into(),
                    status: Status::changed(Some("1"), "2".into()),
                },
                Entry {
                    name: "b".into(),
                    status: Status::Failed { error: "x".into() },
                },
            ],
        };
        assert_eq!(
            summary(&report),
            "### fetchrix update\n\n| Package | Old | New | Status |\n|---|---|---|---|\n\
             | a | 1 | 2 | changed |\n| b |  |  | failed |\n\n"
        );
    }
}
