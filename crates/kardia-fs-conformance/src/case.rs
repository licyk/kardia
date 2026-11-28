//! The cases the suite runs, and the outcome of one.

use kardia_core::Result;
use kardia_fs::{FsCode, FsError};

/// What one case found.
#[derive(Debug)]
pub enum Outcome {
    Passed,
    /// The provider does not declare the capability the case needs.
    Skipped,
    Failed(String),
}

#[derive(Clone, Copy)]
pub(crate) enum Case {
    WriteThenRead,
    MissingIsNotFound,
    ExclusiveRejects,
    ConditionalWrite,
    RangedRead,
    StatReports,
    MkdirRecursive,
    ReaddirLists,
    RenameMoves,
    RemoveRefuses,
    CopyFile,
}

pub(crate) fn is_code(error: &kardia_core::Error, code: FsCode) -> bool {
    error.downcast_ref::<FsError>().is_some_and(|error| error.code == code)
}

pub(crate) fn check(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition { Ok(()) } else { Err(kardia_core::Error::msg(message.into())) }
}
