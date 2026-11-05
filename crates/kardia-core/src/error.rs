use std::error::Error as StdError;
use std::fmt;
use std::sync::Arc;

use crate::key::Key;

/// A cheaply clonable, type-erased error.
///
/// Every `std::error::Error` converts into it, so plugin code can use `?` on
/// any fallible call. It deliberately does not implement `std::error::Error`
/// itself (that would conflict with the blanket `From`); use [`Error::as_std`]
/// where a standard error is required.
#[derive(Clone)]
pub struct Error(Arc<dyn StdError + Send + Sync>);

impl Error {
    pub fn new<E: StdError + Send + Sync + 'static>(error: E) -> Self {
        Self(Arc::new(error))
    }

    pub fn msg(message: impl fmt::Display) -> Self {
        Self::new(Message(message.to_string()))
    }

    pub fn downcast_ref<E: StdError + 'static>(&self) -> Option<&E> {
        self.0.downcast_ref()
    }

    pub fn is<E: StdError + 'static>(&self) -> bool {
        self.0.is::<E>()
    }

    /// `true` for the non-failure outcome of an iteration boundary.
    pub fn is_interrupted(&self) -> bool {
        self.is::<Interrupted>()
    }

    pub fn as_std(&self) -> &(dyn StdError + Send + Sync + 'static) {
        &*self.0
    }

    /// Whether two handles refer to the same error value.
    pub fn ptr_eq(&self, other: &Error) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub(crate) fn from_panic(payload: Box<dyn std::any::Any + Send>) -> Self {
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".into());
        Self::new(Panicked(message))
    }
}

impl<E: StdError + Send + Sync + 'static> From<E> for Error {
    fn from(error: E) -> Self {
        Self::new(error)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct Message(String);

#[derive(Debug, Clone, thiserror::Error)]
#[error("panicked: {0}")]
pub struct Panicked(pub String);

/// Returned by an iteration boundary whose effect (or fiber load) has been
/// superseded. Propagating it with `?` is not a failure: what was collected
/// so far is disposed normally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("interrupted at an iteration boundary")]
pub struct Interrupted;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KardiaError {
    #[error("cannot create effect on inactive context")]
    InactiveEffect,
    #[error("service \"{key}\" has been registered at <{by}>")]
    AlreadyProvided { key: Key, by: String },
    #[error("cannot set property \"{key}\" without provide")]
    NotProvided { key: Key },
    #[error("cannot set property \"{key}\" in multiple fibers")]
    ForeignBinding { key: Key },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AccessError {
    #[error("cannot get property \"{key}\" without inject")]
    Undeclared { key: Key },
    #[error("cannot get required service \"{key}\" in inactive context")]
    Inactive { key: Key },
    #[error("service \"{key}\" is not of type {expected}")]
    TypeMismatch { key: Key, expected: &'static str },
}

/// Several independent failures, e.g. of the listeners of a parallel event.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct AggregateError {
    pub message: String,
    pub errors: Vec<Error>,
}
