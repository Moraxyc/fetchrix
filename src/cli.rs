use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(version, about)]
pub struct Cli {
    /// Configuration file; the lock file is written next to it with a `.lock` extension.
    #[arg(short, long, global = true, default_value = "fetchrix.toml")]
    pub config: PathBuf,
    /// Token for GitHub API requests.
    #[arg(long, global = true, env = "GITHUB_TOKEN", hide_env_values = true)]
    pub github_token: Option<String>,
    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    #[must_use]
    pub fn lock_path(&self) -> PathBuf {
        self.config.with_extension("lock")
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Check versions, fetch sources, compute derived hashes, and write the lock file.
    Update(Selection),
    /// Check upstream versions without downloading or writing the lock file.
    Check(Selection),
    /// Verify hashes recorded in the lock file.
    Verify(Selection),
}

#[derive(Debug, Args)]
pub struct Selection {
    /// Packages to process; all packages when empty.
    pub packages: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::CommandFactory;

    #[test]
    fn cli_is_consistent() {
        Cli::command().debug_assert();
    }
}
