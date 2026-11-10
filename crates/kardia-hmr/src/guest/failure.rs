//! The typed error of a guest instance that failed, as opposed to a guest
//! reporting an error of its own.

use std::fmt;

use kardia_core::Error;

/// How a guest instance failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum GuestFailureKind {
    /// A WASM guest trapped, or a host function failed while it ran.
    Trap,
    /// A WASM export used up its fuel.
    OutOfFuel,
    /// An export outlived its deadline: a WASM guest was interrupted, a
    /// process guest killed.
    Timeout,
    /// A WASM guest was refused growing its memory.
    OutOfMemory,
    /// The guest process ended on its own.
    Exited,
    /// The guest process was killed: it broke the protocol, sent a message
    /// over the frame limit, or its host killed it.
    Killed,
    /// A thread running the guest panicked.
    Panicked,
}

/// The error of a guest instance that failed (it trapped, ran out of fuel,
/// time or memory, its process ended or was killed, or a thread running it
/// panicked), as opposed to a guest reporting an error of its own, which
/// leaves the instance running. Its text is the error's message.
///
/// It reaches the caller as a [`kardia_core::Error`]: find it with
/// [`GuestFailure::of`] (or `error.downcast_ref::<GuestFailure>()`).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct GuestFailure {
    pub kind: GuestFailureKind,
    pub message: String,
}

impl GuestFailure {
    pub fn new(kind: GuestFailureKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }

    /// The failure `error` carries, if the instance failed.
    pub fn of(error: &Error) -> Option<&GuestFailure> {
        error.downcast_ref()
    }
}

impl fmt::Display for GuestFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for GuestFailure {}
