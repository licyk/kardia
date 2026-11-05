//! Module graph and hot-reload reconciliation on top of `registry.replace`.
//!
//! Rust has no module registry, so the host owns one: a [`ModuleGraph`]
//! loads module code from a [`ModuleSource`]. The [`Hmr`] service reloads
//! changed modules and replaces the plugins they export in place; with
//! `watch.enabled` (and the `fs` service) it does so as files change.
//!
//! Besides custom sources, two backends load guest modules from files:
//! [`WasmSource`] (WebAssembly on wasmtime, sandboxed) and [`DylibSource`]
//! (dynamic libraries, for development). Both speak the guest ABI below and
//! run each guest plugin through a bridge plugin that instantiates the guest
//! once per fiber and drops the instance when the fiber is disposed. The
//! `cordis-guest` crate writes such guests in Rust, for either backend.
//!
//! # Guest ABI
//!
//! Data crosses the boundary as UTF-8 JSON in byte buffers. The host keeps
//! one *message* per instance: the input of the export being called, then
//! the result of the last host call. The guest copies it into its own memory
//! with `read`, after allocating the length it was given.
//!
//! Exports (status `0` is success; on failure the guest `write`s the error):
//!
//! | WASM | dylib | |
//! |---|---|---|
//! | `memory` | `cordis_new() -> state` | memory / per-instance guest state |
//! | `cordis_meta() -> i32` | `cordis_meta(state, host) -> i32` | `write`s `{ deps?, plugin?: { name?, inject?, provide? }, value? }` |
//! | `cordis_apply(len) -> i32` | `cordis_apply(state, host, len) -> i32` | message: `{ config, imports }` |
//! | `cordis_call(len) -> i32` | `cordis_call(state, host, len) -> i32` | message: `{ key, method, args }`; `write`s the result |
//! | `cordis_dispose()` (optional) | `cordis_dispose(state, host)` | last call; a dylib frees `state` |
//!
//! `deps` are module paths relative to the module; `imports` maps each
//! dependency that exports a `value` to that value. A module exporting
//! neither a plugin nor a value exports nothing.
//!
//! Host functions (WASM imports from module `cordis`; for a dylib, function
//! pointers in the `host` struct `{ host, read, write, log, provide, view,
//! invoke }`, each taking `host.host` first). Pointers are guest addresses,
//! lengths are byte counts:
//!
//! * `read(ptr)` copies the message to `ptr`.
//! * `write(ptr, len)` sets the result of the current export.
//! * `log(level, ptr, len)` logs as the plugin (`0` error … `3` debug).
//! * `provide(key, key_len, value, value_len) -> i32` provides the service
//!   `key` as a [`GuestService`] whose JSON view is `value`.
//! * `view(key, key_len) -> i32` reads the JSON view of a service, with the
//!   checked access of the plugin's declared `inject`.
//! * `invoke(key, key_len, method, method_len, args, args_len) -> i32` calls
//!   a [`GuestService`].
//!
//! Host functions returning `i32` set the message to their result and return
//! its length, or set it to an error message and return `-1 - len`.

mod graph;
#[cfg(any(feature = "wasm", feature = "dylib"))]
mod guest;
mod hmr;
mod watch;

#[cfg(feature = "dylib")]
mod dylib;
#[cfg(feature = "wasm")]
mod wasm;

#[cfg(feature = "dylib")]
pub use dylib::DylibSource;
pub use graph::{Exports, Imports, ModuleCode, ModuleGraph, ModuleId, ModuleSource};
#[cfg(any(feature = "wasm", feature = "dylib"))]
pub use guest::GuestService;
pub use hmr::{Hmr, HmrConfig, HmrExt, ImportFailure, ReconcileResult};
#[cfg(feature = "wasm")]
pub use wasm::WasmSource;
pub use watch::WatchConfig;
