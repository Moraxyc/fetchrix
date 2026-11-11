use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
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
