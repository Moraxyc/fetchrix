pub mod cli;
pub mod config;
pub mod derive;
pub mod error;
pub mod fetch;
pub mod http;
pub mod lock;
pub mod source;
pub mod ui;

pub use error::{Error, Result};

use std::{collections::BTreeMap, path::Path, sync::Arc};

use cli::{Cli, Command, Selection};
use config::{Config, Package};
use http::Client;
use lock::{Lock, LockedPackage};

/// Runs the command selected on the command line.
///
/// # Errors
///
/// Returns an error if the command fails.
pub async fn run(cli: &Cli) -> Result<()> {
    let config = Arc::new(Config::load(&cli.config)?);
    let lock = Lock::load(&cli.lock_path())?;
    match &cli.command {
        Command::Check(selection) => {
            let selected = select(&config, selection)?;
            check(cli, &config, &lock, &selected).await
        }
        Command::Update(selection) => {
            let selected = select(&config, selection)?;
            update(cli, config, lock, selected, selection.packages.is_empty()).await
        }
        Command::Verify(_) => Err(Error::NotImplemented("verify")),
    }
}

async fn check(cli: &Cli, config: &Config, lock: &Lock, selected: &[String]) -> Result<()> {
    let client = Arc::new(Client::new(cli.github_token.clone())?);
    let mut tasks = tokio::task::JoinSet::new();
    for name in selected {
        let client = Arc::clone(&client);
        let spec = config.packages[name].version.clone();
        let name = name.clone();
        tasks.spawn(async move { (name, source::latest(&client, &spec).await) });
    }
    let results = join(tasks).await;

    let mut failed = 0;
    for (name, result) in results {
        match result {
            Ok(new) => report(
                &name,
                lock.packages.get(&name).map(|p| p.version.as_str()),
                &new,
            ),
            Err(e) => failed += fail(&name, e.into()),
        }
    }
    finish(failed)
}

enum Outcome {
    Unchanged,
    Updated(LockedPackage),
}

async fn update(
    cli: &Cli,
    config: Arc<Config>,
    mut lock: Lock,
    selected: Vec<String>,
    all: bool,
) -> Result<()> {
    let client = Arc::new(Client::new(cli.github_token.clone())?);
    let flake_dir = Arc::new(fetch::flake_dir(&cli.config));
    let mut tasks = tokio::task::JoinSet::new();
    for name in selected {
        let client = Arc::clone(&client);
        let config = Arc::clone(&config);
        let flake_dir = Arc::clone(&flake_dir);
        let old = lock.packages.get(&name).cloned();
        tasks.spawn(async move {
            let result = update_package(&client, &config, &flake_dir, &name, old).await;
            (name, result)
        });
    }
    let results = join(tasks).await;

    let original = lock.clone();
    let mut failed = 0;
    for (name, result) in results {
        match result {
            Ok(Outcome::Unchanged) => println!("{name}: {}", lock.packages[&name].version),
            Ok(Outcome::Updated(new)) => {
                report(
                    &name,
                    lock.packages.get(&name).map(|p| p.version.as_str()),
                    &new.version,
                );
                lock.packages.insert(name, new);
            }
            Err(e) => failed += fail(&name, e),
        }
    }
    if all {
        lock.packages
            .retain(|name, _| config.packages.contains_key(name));
    }
    if lock != original {
        lock.save(&cli.lock_path())?;
    }
    finish(failed)
}

async fn update_package(
    client: &Client,
    config: &Config,
    flake_dir: &Path,
    name: &str,
    old: Option<LockedPackage>,
) -> Result<Outcome> {
    let package: &Package = &config.packages[name];
    let version = source::latest(client, &package.version).await?;
    let fingerprint = package.fingerprint();
    if old.is_some_and(|old| old.version == version && old.fingerprint == fingerprint) {
        return Ok(Outcome::Unchanged);
    }
    if !package.derive.is_empty() {
        return Err(Error::NotImplemented("derive"));
    }
    let fetcher = fetch::Fetcher {
        client,
        nixpkgs: config.nix.nixpkgs.as_deref().unwrap_or("nixpkgs"),
        flake_dir,
    };
    let src = fetcher.fetch(&package.fetch, &version).await?;
    Ok(Outcome::Updated(LockedPackage {
        version,
        src,
        derived: BTreeMap::new(),
        fingerprint,
    }))
}

async fn join<T: 'static>(mut tasks: tokio::task::JoinSet<(String, T)>) -> BTreeMap<String, T> {
    let mut results = BTreeMap::new();
    while let Some(joined) = tasks.join_next().await {
        let (name, result) = joined.expect("package tasks do not panic");
        results.insert(name, result);
    }
    results
}

fn report(name: &str, old: Option<&str>, new: &str) {
    match old {
        Some(old) if old == new => println!("{name}: {new}"),
        Some(old) => println!("{name}: {old} -> {new}"),
        None => println!("{name}: {new} (new)"),
    }
}

fn fail(name: &str, error: Error) -> usize {
    eprintln!("{:?}", miette::Report::new(error).wrap_err(name.to_owned()));
    1
}

fn finish(failed: usize) -> Result<()> {
    if failed == 0 {
        Ok(())
    } else {
        Err(Error::Failed(failed))
    }
}

fn select(config: &Config, selection: &Selection) -> Result<Vec<String>> {
    if selection.packages.is_empty() {
        return Ok(config.packages.keys().cloned().collect());
    }
    selection
        .packages
        .iter()
        .map(|name| {
            if config.packages.contains_key(name) {
                Ok(name.clone())
            } else {
                Err(Error::UnknownPackage(name.clone()))
            }
        })
        .collect()
}
