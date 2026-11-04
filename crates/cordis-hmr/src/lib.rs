//! Module graph and hot-reload reconciliation on top of `registry.replace`.
//!
//! Rust has no module registry, so the host owns one: a [`ModuleGraph`]
//! loads module code from a [`ModuleSource`] (an in-memory table, WASM
//! components, dynamic libraries, ...). The [`Hmr`] service reloads changed
//! modules and replaces the plugins they export in place.

mod graph;
mod hmr;

pub use graph::{Exports, Imports, ModuleCode, ModuleGraph, ModuleId, ModuleSource};
pub use hmr::{Hmr, HmrExt, ImportFailure, ReconcileResult};
