pub mod cli;
pub mod config;
pub mod derive;
pub mod error;
pub mod fetch;
pub mod lock;
pub mod source;
pub mod ui;

pub use error::{Error, Result};

use cli::{Cli, Command, Selection};
use config::Config;
use lock::Lock;

/// Runs the command selected on the command line.
///
/// # Errors
///
/// Returns an error if the command fails.
pub fn run(cli: &Cli) -> Result<()> {
    let config = Config::load(&cli.config)?;
    let _lock = Lock::load(&cli.lock_path())?;
    let (name, selection) = match &cli.command {
        Command::Update(s) => ("update", s),
        Command::Check(s) => ("check", s),
        Command::Verify(s) => ("verify", s),
    };
    select(&config, selection)?;
    Err(Error::NotImplemented(name))
}

fn select<'a>(config: &'a Config, selection: &'a Selection) -> Result<Vec<&'a str>> {
    if selection.packages.is_empty() {
        return Ok(config.packages.keys().map(String::as_str).collect());
    }
    selection
        .packages
        .iter()
        .map(|name| {
            if config.packages.contains_key(name) {
                Ok(name.as_str())
            } else {
                Err(Error::UnknownPackage(name.clone()))
            }
        })
        .collect()
}
