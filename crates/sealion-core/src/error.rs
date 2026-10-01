//! Shared error type for SeaLion crates.

use thiserror::Error;

/// Top-level error for recoverable engine failures.
///
/// I/O and corruption errors carry context; logic bugs should `panic!` in
/// debug builds via `debug_assert!` rather than surfacing here.
#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(String),
    #[error("corrupt index data: {0}")]
    Corrupt(String),
    #[error("invalid argument: {0}")]
    Invalid(String),
    #[error("invalid query: {0}")]
    InvalidQuery(String),
    #[error("not yet implemented: {0}")]
    Unimplemented(String),
    #[error("internal error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}
