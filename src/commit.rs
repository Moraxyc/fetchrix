//! Git commit of the output directory.

use std::path::Path;

use miette::Diagnostic;
use thiserror::Error;
use tokio::process::Command;

#[derive(Debug, Error, Diagnostic)]
pub enum CommitError {
    #[error("cannot run git")]
    #[diagnostic(code(fetchrix::commit::spawn))]
    Spawn(#[source] std::io::Error),
    #[error("`git {command}` failed:\n{stderr}")]
    #[diagnostic(code(fetchrix::commit::git))]
    Git { command: String, stderr: String },
}

/// Stages every change under `dir` and commits only those paths.
///
/// # Errors
///
/// Returns an error if a git command fails.
pub async fn commit(dir: &Path, message: &str) -> Result<(), CommitError> {
    let dir = dir.as_os_str();
    git(&["add".as_ref(), "--all".as_ref(), "--".as_ref(), dir]).await?;
    git(&[
        "commit".as_ref(),
        "--quiet".as_ref(),
        "--message".as_ref(),
        message.as_ref(),
        "--".as_ref(),
        dir,
    ])
    .await
}

async fn git(args: &[&std::ffi::OsStr]) -> Result<(), CommitError> {
    let output = Command::new("git")
        .args(args)
        .output()
        .await
        .map_err(CommitError::Spawn)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(CommitError::Git {
            command: args[0].to_string_lossy().into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}
