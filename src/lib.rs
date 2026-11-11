pub mod cli;
pub mod config;
pub mod derive;
pub mod error;
pub mod fetch;
pub mod lock;
pub mod source;
pub mod ui;

pub use error::{Error, Result};

use std::{collections::BTreeMap, sync::Arc};

use cli::{Cli, Command, Selection};
use config::Config;
use lock::Lock;

/// Runs the command selected on the command line.
///
/// # Errors
///
/// Returns an error if the command fails.
pub async fn run(cli: &Cli) -> Result<()> {
    let config = Config::load(&cli.config)?;
    let lock = Lock::load(&cli.lock_path())?;
    match &cli.command {
        Command::Check(selection) => {
            let selected = select(&config, selection)?;
            check(cli, &config, &lock, &selected).await
        }
        Command::Update(_) => Err(Error::NotImplemented("update")),
        Command::Verify(_) => Err(Error::NotImplemented("verify")),
    }
}

async fn check(cli: &Cli, config: &Config, lock: &Lock, selected: &[&str]) -> Result<()> {
    let client = Arc::new(source::Client::new(cli.github_token.clone())?);
    let mut tasks = tokio::task::JoinSet::new();
    for &name in selected {
        let client = Arc::clone(&client);
        let spec = config.packages[name].version.clone();
        let name = name.to_owned();
        tasks.spawn(async move { (name, client.latest(&spec).await) });
    }
    let mut results = BTreeMap::new();
    while let Some(joined) = tasks.join_next().await {
        let (name, result) = joined.expect("version queries do not panic");
        results.insert(name, result);
    }

    let mut failed = 0;
    for (name, result) in results {
        match (result, lock.packages.get(&name)) {
            (Ok(new), Some(old)) if old.version == new => println!("{name}: {new}"),
            (Ok(new), Some(old)) => println!("{name}: {} -> {new}", old.version),
            (Ok(new), None) => println!("{name}: {new} (new)"),
            (Err(e), _) => {
                failed += 1;
                eprintln!("{:?}", miette::Report::new(e).wrap_err(name));
            }
        }
    }
    if failed == 0 {
        Ok(())
    } else {
        Err(Error::Failed(failed))
    }
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
