//! File-backed entry trees (YAML / JSON) with two-way synchronisation.
//!
//! File edits are reconciled into the tree; runtime changes of the tree
//! (API edits, plugin self-updates and self-disposals) are recorded in a
//! journal and written back to whoever owns them: the file, or a patch in
//! the include's own configuration. On a conflict the file wins.

mod include;
pub mod journal;
pub mod patch;
mod tree;
pub mod yaml;

pub use include::{ConfigFileError, ConfigIssue, Include, IncludeConfig, IncludePlugin, Stage};
pub use tree::{Flat, flatten};
