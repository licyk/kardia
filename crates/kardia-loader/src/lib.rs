//! Declarative loader: entries, groups, realms and a plugin catalog.
//!
//! The loader keeps a tree of *entries* (plugin name + configuration) in
//! step with live fibers in both directions: configuration changes are
//! reconciled into the fibers, and a plugin that updates or disposes
//! itself is written back to its entry.
//!
//! The default feature `preserve-order` turns on `serde_json/preserve_order`,
//! which keeps option keys in insertion order and lets [`sort_keys`] order
//! write-backs. Cargo unifies it into every `serde_json::Map` of a build;
//! without it, maps (write-backs included) list their keys alphabetically.

mod catalog;
mod entry;
pub mod expr;
mod group;
mod loader;
mod locate;
mod options;
mod realm;

pub use catalog::{Catalog, ModuleNotFound, Resolver, builtins};
pub use entry::OUTER_TAG;
pub use group::{GROUP_TAG, group_plugin};
pub use loader::{EntryChange, EntryHandle, GroupHandle, Loader, MemoryTree, PatchNext, TreeBackend, TreeHandle};
pub use locate::{Location, value_at};
pub use options::{
    Disabled, EXPR_KEY, EntryOptions, EntryValidationError, IsolateLabel, SEP, diff_keys, expr_source, is_expr, merge,
    sort_keys, validate_options,
};
