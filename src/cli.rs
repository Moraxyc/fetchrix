use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::auth::HostToken;

#[derive(Debug, Parser)]
#[command(version, about)]
pub struct Cli {
    /// Configuration file.
    #[arg(short, long, global = true, default_value = "fetchrix.toml")]
    pub config: PathBuf,
    /// Output directory holding `lock.json` and extracted lock files, one subdirectory per package.
    #[arg(short, long, global = true, default_value = "_sources")]
    pub out_dir: PathBuf,
    /// Bearer token for requests to HOST, overriding environment tokens and netrc; repeatable.
    #[arg(long, global = true, value_name = "HOST=TOKEN")]
    pub access_token: Vec<HostToken>,
    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    #[must_use]
    pub fn lock_path(&self) -> PathBuf {
        self.out_dir.join(crate::lock::FILE)
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Check versions, fetch sources, compute derived hashes, and write the lock file.
    Update(Update),
    /// Check upstream versions without downloading or writing the lock file.
    Check(Selection),
    /// Verify hashes recorded in the lock file.
    Verify(Selection),
}

#[derive(Debug, Args)]
pub struct Update {
    #[command(flatten)]
    pub selection: Selection,
    /// Commit the output directory once, listing every version change in the message.
    #[arg(long)]
    pub commit: bool,
}

#[derive(Debug, Args)]
pub struct Selection {
    /// Packages to process; all packages when empty.
    pub packages: Vec<String>,
    /// Print the report as JSON on stdout.
    #[arg(long)]
    pub json: bool,
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
