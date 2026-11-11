use miette::Diagnostic;
use thiserror::Error;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Error, Diagnostic)]
pub enum Error {
    #[error("`{0}` is not implemented yet")]
    #[diagnostic(code(fetchrix::not_implemented))]
    NotImplemented(&'static str),
}
