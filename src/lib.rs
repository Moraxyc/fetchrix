pub mod cli;
pub mod derive;
pub mod error;
pub mod fetch;
pub mod lock;
pub mod source;
pub mod ui;

pub use error::{Error, Result};

use cli::{Cli, Command};

/// Runs the command selected on the command line.
///
/// # Errors
///
/// Returns an error if the command fails.
pub fn run(cli: &Cli) -> Result<()> {
    match &cli.command {
        Command::Update(_) => Err(Error::NotImplemented("update")),
        Command::Check(_) => Err(Error::NotImplemented("check")),
        Command::Verify(_) => Err(Error::NotImplemented("verify")),
    }
}
