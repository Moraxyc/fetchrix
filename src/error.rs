use miette::Diagnostic;
use thiserror::Error;

use crate::{config::ConfigError, lock::LockError, source::SourceError};

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Error, Diagnostic)]
pub enum Error {
    #[error(transparent)]
    #[diagnostic(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    #[diagnostic(transparent)]
    Lock(#[from] LockError),
    #[error(transparent)]
    #[diagnostic(transparent)]
    Source(#[from] SourceError),
    #[error("{0} package(s) failed")]
    #[diagnostic(code(fetchrix::failed))]
    Failed(usize),
    #[error("unknown package `{0}`")]
    #[diagnostic(code(fetchrix::unknown_package))]
    UnknownPackage(String),
    #[error("`{0}` is not implemented yet")]
    #[diagnostic(code(fetchrix::not_implemented))]
    NotImplemented(&'static str),
}
