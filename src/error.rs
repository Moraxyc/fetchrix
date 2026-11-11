use miette::Diagnostic;
use thiserror::Error;

use crate::{
    auth::AuthError, config::ConfigError, derive::DeriveError, fetch::FetchError, http::HttpError,
    lock::LockError, source::SourceError, verify::VerifyError,
};

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
    #[error(transparent)]
    #[diagnostic(transparent)]
    Fetch(#[from] FetchError),
    #[error(transparent)]
    #[diagnostic(transparent)]
    Derive(#[from] DeriveError),
    #[error(transparent)]
    #[diagnostic(transparent)]
    Http(#[from] HttpError),
    #[error(transparent)]
    #[diagnostic(transparent)]
    Auth(#[from] AuthError),
    #[error(transparent)]
    #[diagnostic(transparent)]
    Verify(#[from] VerifyError),
    #[error("{0} package(s) failed")]
    #[diagnostic(code(fetchrix::failed))]
    Failed(usize),
    #[error("unknown package `{0}`")]
    #[diagnostic(code(fetchrix::unknown_package))]
    UnknownPackage(String),
}
