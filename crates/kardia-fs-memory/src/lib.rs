//! An in-memory file system provider (`mem:`), for tests and for a host that
//! keeps configuration in memory.
//!
//! Content lives in one tree per provider, so a mount at `mem:` serves every
//! location below it. Entries are versioned by a counter, which `if_match` and
//! `Stats::version` compare; every write is atomic because no reader can observe
//! a partially replaced buffer.

mod provider;
mod tree;

pub use provider::{MemoryFs, MemoryFsConfig, MemoryFsPlugin};
