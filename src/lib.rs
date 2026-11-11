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
use lock::{Derived, DerivedHash, DerivedLockfile, Lock, LockedPackage};
use ui::{Progress, Stage, Ui};

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
    let ui = Ui::new(selected);
    let mut tasks = tokio::task::JoinSet::new();
    for name in selected {
        let client = Arc::clone(&client);
        let spec = config.packages[name].version.clone();
        let progress = ui.package(name);
        let name = name.clone();
        tasks.spawn(async move {
            progress.stage(Stage::Check);
            let result = source::latest(&client, &spec).await;
            progress.done(result.is_ok());
            (name, result)
        });
    }
    let results = join(tasks).await;
    ui.finish();

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
    Updated(Box<LockedPackage>, Lockfiles),
}

/// Extracted lock files of one package, by file name.
type Lockfiles = Vec<(&'static str, Vec<u8>)>;

async fn update(
    cli: &Cli,
    config: Arc<Config>,
    mut lock: Lock,
    selected: Vec<String>,
    all: bool,
) -> Result<()> {
    let client = Arc::new(Client::new(cli.github_token.clone())?);
    let flake_dir = Arc::new(fetch::flake_dir(&cli.config));
    let lockfiles_dir = Arc::new(cli.lockfiles_dir());
    let ui = Ui::new(&selected);
    let mut tasks = tokio::task::JoinSet::new();
    for name in selected {
        let client = Arc::clone(&client);
        let config = Arc::clone(&config);
        let flake_dir = Arc::clone(&flake_dir);
        let lockfiles_dir = Arc::clone(&lockfiles_dir);
        let old = lock.packages.get(&name).cloned();
        let progress = ui.package(&name);
        tasks.spawn(async move {
            let dirs = Dirs {
                flake: &flake_dir,
                lockfiles: &lockfiles_dir,
            };
            let result = update_package(&client, &config, dirs, &progress, &name, old).await;
            progress.done(result.is_ok());
            (name, result)
        });
    }
    let results = join(tasks).await;
    ui.finish();

    let original = lock.clone();
    let mut failed = 0;
    let mut lockfiles = BTreeMap::new();
    for (name, result) in results {
        match result {
            Ok(Outcome::Unchanged) => println!("{name}: {}", lock.packages[&name].version),
            Ok(Outcome::Updated(new, files)) => {
                report(
                    &name,
                    lock.packages.get(&name).map(|p| p.version.as_str()),
                    &new.version,
                );
                lockfiles.insert(name.clone(), files);
                lock.packages.insert(name, *new);
            }
            Err(e) => failed += fail(&name, e),
        }
    }
    if all {
        let removed: Vec<_> = lock
            .packages
            .keys()
            .filter(|name| !config.packages.contains_key(*name))
            .cloned()
            .collect();
        for name in removed {
            lock.packages.remove(&name);
            lockfiles.insert(name, Vec::new());
        }
    }
    for (name, files) in lockfiles {
        write_lockfiles(&lockfiles_dir.join(name), &files)?;
    }
    if lock != original {
        lock.save(&cli.lock_path())?;
    }
    finish(failed)
}

#[derive(Clone, Copy)]
struct Dirs<'a> {
    flake: &'a Path,
    lockfiles: &'a Path,
}

async fn update_package(
    client: &Client,
    config: &Config,
    dirs: Dirs<'_>,
    progress: &Progress,
    name: &str,
    old: Option<LockedPackage>,
) -> Result<Outcome> {
    let package: &Package = &config.packages[name];
    progress.stage(Stage::Check);
    let version = source::latest(client, &package.version).await?;
    let fingerprint = package.fingerprint();
    if old.is_some_and(|old| old.version == version && old.fingerprint == fingerprint) {
        return Ok(Outcome::Unchanged);
    }
    let fetcher = fetch::Fetcher {
        client,
        nixpkgs: config.nix.nixpkgs.as_deref().unwrap_or("nixpkgs"),
        flake_dir: dirs.flake,
    };
    progress.stage(Stage::Fetch);
    let source = fetcher.fetch(&package.fetch, &version).await?;
    if !package.derive.is_empty() {
        progress.stage(Stage::Derive);
    }

    let deriver = derive::Deriver {
        flake_dir: dirs.flake,
    };
    let dir_name = dirs
        .lockfiles
        .file_name()
        .expect("the lock files directory is named after the config file")
        .to_string_lossy();
    let mut outputs = BTreeMap::new();
    let mut files = Vec::new();
    for (attr, spec) in &package.derive {
        let entry = match deriver.derive(spec, &source.path).await? {
            derive::Output::Hash(hash) => Derived::Hash(DerivedHash { hash: hash.into() }),
            derive::Output::Lockfile {
                name: file,
                content,
            } => {
                files.push((file, content));
                Derived::Lockfile(DerivedLockfile {
                    lockfile: format!("{dir_name}/{name}/{file}"),
                })
            }
        };
        outputs.insert(attr.clone(), entry);
    }
    Ok(Outcome::Updated(
        Box::new(LockedPackage {
            version,
            src: source.source,
            derived: outputs,
            fingerprint,
        }),
        files,
    ))
}

/// Replaces the lock files of one package; an empty list removes its directory.
fn write_lockfiles(dir: &Path, files: &Lockfiles) -> Result<()> {
    let io = |source| lock::LockError::Io {
        path: dir.display().to_string(),
        source,
    };
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(io(e).into()),
        _ => {}
    }
    if !files.is_empty() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    for (file, content) in files {
        lock::write_atomic(&dir.join(file), content)?;
    }
    Ok(())
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
