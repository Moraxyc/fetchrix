pub mod auth;
pub mod cli;
pub mod commit;
pub mod config;
pub mod derive;
pub mod error;
pub mod fetch;
pub mod http;
pub mod lock;
pub mod nixpkgs;
pub mod source;
pub mod ui;
pub mod verify;

pub use error::{Error, Result};

use std::{collections::BTreeMap, path::Path, sync::Arc};

use auth::Credentials;
use cli::{Cli, Command, Selection, Update};
use config::{Config, Package, VersionSpec};
use http::Client;
use lock::{Derived, DerivedHash, DerivedLockfile, Lock, LockedPackage};
use ui::{
    Progress, Stage, Ui,
    report::{Entry, Report, Status},
};
use verify::VerifyError;

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
            check(cli, selection.json, &config, &lock, &selected).await
        }
        Command::Update(args) => {
            let selected = select(&config, &args.selection)?;
            update(cli, args, config, lock, selected).await
        }
        Command::Verify(selection) => {
            let selected = select(&config, selection)?;
            verify(cli, selection.json, config, &lock, selected).await
        }
    }
}

async fn verify(
    cli: &Cli,
    json: bool,
    config: Arc<Config>,
    lock: &Lock,
    selected: Vec<String>,
) -> Result<()> {
    let client = Arc::new(Client::new(Credentials::load(&cli.access_token)?)?);
    let flake_dir = Arc::new(fetch::flake_dir(&cli.config));
    let out_dir = Arc::new(cli.out_dir.clone());
    let ui = Ui::new("verify", &selected);
    let mut tasks = tokio::task::JoinSet::new();
    for name in selected {
        let client = Arc::clone(&client);
        let config = Arc::clone(&config);
        let flake_dir = Arc::clone(&flake_dir);
        let out_dir = Arc::clone(&out_dir);
        let locked = lock.packages.get(&name).cloned();
        let progress = ui.package(&name);
        tasks.spawn(async move {
            let result = verify_package(
                &client, &config, &flake_dir, &out_dir, &progress, &name, locked,
            )
            .await;
            progress.done(result.is_ok());
            (name, result)
        });
    }
    let results = join(tasks).await;
    ui.finish();

    let packages = results
        .into_iter()
        .map(|(name, result)| {
            let status = match result {
                Ok(version) => Status::Verified { version },
                Err(e) => fail(&name, e),
            };
            Entry { name, status }
        })
        .collect();
    finish(
        &Report {
            command: "verify",
            packages,
        },
        json,
    )
}

/// Recomputes the hashes of the locked version and compares them, and the
/// extracted lock files on disk, with the lock entry.
async fn verify_package(
    client: &Client,
    config: &Config,
    flake_dir: &Path,
    out_dir: &Path,
    progress: &Progress,
    name: &str,
    locked: Option<LockedPackage>,
) -> Result<String> {
    let locked = locked.ok_or(VerifyError::NotLocked)?;
    let fingerprint = config.packages[name].fingerprint();
    if locked.fingerprint != fingerprint {
        return Err(VerifyError::Stale.into());
    }
    let (actual, files) = resolve(
        client,
        config,
        flake_dir,
        progress,
        name,
        locked.raw_version.clone(),
        |_| Ok(locked.version.clone()),
        fingerprint,
    )
    .await?;
    let mut diffs = verify::compare(&locked, &actual);
    for (file, content) in files {
        let path = out_dir.join(name).join(&file);
        match tokio::fs::read(&path).await {
            Ok(on_disk) if on_disk == content => {}
            Ok(_) => diffs.push(format!("{}: differs from the source", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                diffs.push(format!("{}: missing", path.display()));
            }
            Err(source) => {
                return Err(lock::LockError::Io {
                    path: path.display().to_string(),
                    source,
                }
                .into());
            }
        }
    }
    if diffs.is_empty() {
        Ok(locked.version)
    } else {
        Err(VerifyError::Mismatch(diffs).into())
    }
}

async fn check(
    cli: &Cli,
    json: bool,
    config: &Config,
    lock: &Lock,
    selected: &[String],
) -> Result<()> {
    let client = Arc::new(Client::new(Credentials::load(&cli.access_token)?)?);
    let ui = Ui::new("check", selected);
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

    let packages = results
        .into_iter()
        .map(|(name, result)| {
            let status = match result {
                Ok(new) => Status::changed(
                    lock.packages.get(&name).map(|p| p.raw_version.as_str()),
                    new,
                ),
                Err(e) => fail(&name, e.into()),
            };
            Entry { name, status }
        })
        .collect();
    finish(
        &Report {
            command: "check",
            packages,
        },
        json,
    )
}

enum Outcome {
    Unchanged,
    Updated(Box<LockedPackage>, Lockfiles),
}

/// Files written for one package, by path relative to `<out>/<package>`.
type Lockfiles = Vec<(String, Vec<u8>)>;

async fn update(
    cli: &Cli,
    args: &Update,
    config: Arc<Config>,
    mut lock: Lock,
    selected: Vec<String>,
) -> Result<()> {
    let client = Arc::new(Client::new(Credentials::load(&cli.access_token)?)?);
    let flake_dir = Arc::new(fetch::flake_dir(&cli.config));
    let ui = Ui::new("update", &selected);
    let mut tasks = tokio::task::JoinSet::new();
    for name in selected {
        let client = Arc::clone(&client);
        let config = Arc::clone(&config);
        let flake_dir = Arc::clone(&flake_dir);
        let old = lock.packages.get(&name).cloned();
        let progress = ui.package(&name);
        tasks.spawn(async move {
            let result = update_package(&client, &config, &flake_dir, &progress, &name, old).await;
            progress.done(result.is_ok());
            (name, result)
        });
    }
    let results = join(tasks).await;
    ui.finish();

    let original = lock.clone();
    let mut packages = Vec::new();
    let mut lockfiles = BTreeMap::new();
    for (name, result) in results {
        let status = match result {
            Ok(Outcome::Unchanged) => Status::Unchanged {
                version: lock.packages[&name].version.clone(),
            },
            Ok(Outcome::Updated(new, files)) => {
                lockfiles.insert(name.clone(), files);
                let old = lock.packages.insert(name.clone(), *new);
                Status::Changed {
                    old: old.map(|p| p.version),
                    new: lock.packages[&name].version.clone(),
                }
            }
            Err(e) => fail(&name, e),
        };
        packages.push(Entry { name, status });
    }
    if args.selection.packages.is_empty() {
        let removed: Vec<_> = lock
            .packages
            .keys()
            .filter(|name| !config.packages.contains_key(*name))
            .cloned()
            .collect();
        for name in removed {
            let old = lock
                .packages
                .remove(&name)
                .expect("removed names are locked");
            lockfiles.insert(name.clone(), Vec::new());
            packages.push(Entry {
                name,
                status: Status::Removed { old: old.version },
            });
        }
        packages.sort_by(|a, b| a.name.cmp(&b.name));
    }
    for (name, files) in lockfiles {
        write_lockfiles(&cli.out_dir.join(name), &files)?;
    }
    if lock != original {
        std::fs::create_dir_all(&cli.out_dir).map_err(|source| lock::LockError::Io {
            path: cli.out_dir.display().to_string(),
            source,
        })?;
        lock.save(&cli.lock_path())?;
    }
    let report = Report {
        command: "update",
        packages,
    };
    if args.commit
        && let Some(message) = report.commit_message()
    {
        commit::commit(&cli.out_dir, &message).await?;
    }
    finish(&report, args.selection.json)
}

async fn update_package(
    client: &Client,
    config: &Config,
    flake_dir: &Path,
    progress: &Progress,
    name: &str,
    old: Option<LockedPackage>,
) -> Result<Outcome> {
    let package: &Package = &config.packages[name];
    progress.stage(Stage::Check);
    let raw = source::latest(client, &package.version).await?;
    let stable = match &package.version {
        VersionSpec::Git {
            unstable: Some(unstable),
            ..
        } => Some(match &unstable.stable {
            Some(spec) => package.transform(&source::latest(client, spec).await?),
            None => "0".to_owned(),
        }),
        _ => None,
    };
    let fingerprint = package.fingerprint();
    if old.is_some_and(|old| {
        old.raw_version == raw
            && old.fingerprint == fingerprint
            && old.pname == package.pname
            && match &stable {
                Some(s) => old.version.starts_with(&format!("{s}-unstable-")),
                None => old.version == package.transform(&raw),
            }
    }) {
        return Ok(Outcome::Unchanged);
    }
    let (new, files) = resolve(
        client,
        config,
        flake_dir,
        progress,
        name,
        raw.clone(),
        |date| match &stable {
            Some(stable) => Ok(source::unstable(stable, date)?),
            None => Ok(package.transform(&raw)),
        },
        fingerprint,
    )
    .await?;
    Ok(Outcome::Updated(Box::new(new), files))
}

/// Fetches upstream version `raw` of package `name` and computes its derived
/// outputs; `version` turns the commit date of the source into the version.
#[expect(
    clippy::too_many_arguments,
    reason = "private helper shared by update and verify"
)]
async fn resolve(
    client: &Client,
    config: &Config,
    flake_dir: &Path,
    progress: &Progress,
    name: &str,
    raw: String,
    version: impl FnOnce(Option<&str>) -> Result<String>,
    fingerprint: String,
) -> Result<(LockedPackage, Lockfiles)> {
    let package: &Package = &config.packages[name];
    let nixpkgs = nixpkgs::Nixpkgs::new(config.nix.nixpkgs.as_deref(), flake_dir);
    let fetcher = fetch::Fetcher {
        client,
        nixpkgs: &nixpkgs,
    };
    progress.stage(Stage::Fetch);
    let source = fetcher.fetch(&package.fetch, &raw).await?;
    let version = version(source.date.as_deref())?;
    if !package.derive.is_empty() || package.prepare.is_some() {
        progress.stage(Stage::Derive);
    }

    let mut files = Vec::new();
    let mut artifacts = BTreeMap::new();
    let prepared = match &package.prepare {
        Some(prepare) => {
            let preparer = derive::prepare::Preparer {
                nixpkgs: &nixpkgs,
                config_dir: flake_dir,
                version: &version,
            };
            let copy = preparer.prepare(prepare, &source.path).await?;
            for (artifact, content) in &copy.artifacts {
                artifacts.insert(artifact.clone(), format!("{name}/{artifact}"));
                files.push((artifact.clone(), content.clone()));
            }
            Some(copy)
        }
        None => None,
    };
    let tree = prepared.as_ref().map_or(&source.path, |p| &p.path);

    let deriver = derive::Deriver {
        fetcher: &fetcher,
        flake_dir,
    };
    let mut outputs = BTreeMap::new();
    for (attr, spec) in &package.derive {
        let entry = match deriver.derive(spec, tree).await? {
            derive::Output::Hash(hash) => Derived::Hash(DerivedHash { hash: hash.into() }),
            derive::Output::Lockfile {
                files: extracted,
                output_hashes,
            } => {
                let (first, _) = extracted.first().expect("the lock file comes first");
                let lockfile = format!("{name}/{}/{first}", lock::SRC_DIR);
                files.extend(
                    extracted
                        .into_iter()
                        .map(|(path, content)| (format!("{}/{path}", lock::SRC_DIR), content)),
                );
                Derived::Lockfile(DerivedLockfile {
                    lockfile,
                    output_hashes: output_hashes
                        .into_iter()
                        .map(|(k, v)| (k, v.into()))
                        .collect(),
                })
            }
        };
        outputs.insert(attr.clone(), entry);
    }
    Ok((
        LockedPackage {
            pname: package.pname.clone(),
            raw_version: raw,
            version,
            date: source.date,
            src: source.source,
            derived: outputs,
            artifacts,
            fingerprint,
        },
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
    for (file, content) in files {
        let path = dir.join(file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        lock::write_atomic(&path, content)?;
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

/// Renders the diagnostic on stderr and records its message chain.
fn fail(name: &str, error: Error) -> Status {
    let mut message = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(e) = source {
        message = format!("{message}: {e}");
        source = e.source();
    }
    eprintln!("{:?}", miette::Report::new(error).wrap_err(name.to_owned()));
    Status::Failed { error: message }
}

fn finish(report: &Report, json: bool) -> Result<()> {
    ui::print(report, json)?;
    let failed = report.failures();
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
