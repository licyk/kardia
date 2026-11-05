//! The error reported to the host, and the `Result` alias.

use std::fmt;

/// An error message, reported to the host. Any displayable error converts
/// into it with `?`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(String);

impl Error {
    pub fn msg(message: impl fmt::Display) -> Self {
        Self(message.to_string())
    }

    pub fn message(&self) -> &str {
        &self.0
    }
}

impl<E: fmt::Display> From<E> for Error {
    fn from(error: E) -> Self {
        Self::msg(error)
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
