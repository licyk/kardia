//! Module graph and hot-reload reconciliation on top of `registry.replace`.
//!
//! Rust has no module registry, so the host owns one: a [`ModuleGraph`]
//! loads module code from a [`ModuleSource`]. The [`Hmr`] service reloads
//! changed modules and replaces the plugins they export in place; with
//! `watch.enabled` (and the `fs` service) it does so as files change.
//!
//! Besides custom sources, three backends load guest modules from files:
//! [`WasmSource`] (WebAssembly on wasmtime, sandboxed), [`DylibSource`]
//! (dynamic libraries, for development) and [`ProcessSource`] (executables,
//! one child process per instance; [`ProcessGuest`] configures one, with
//! arguments, a [`Spawner`], timeouts and a frame limit). All speak the guest
//! ABI below and run each guest plugin through a bridge plugin that
//! instantiates the guest once per fiber, off the async workers, and drops
//! the instance when the fiber is disposed. A host can run guests over a
//! transport of its own ([`GuestCode`], [`GuestInstance`]), and give a
//! plugin's metadata itself instead of reading it from the guest
//! ([`guest_plugin`]). The `kardia-guest` crate writes such guests in Rust,
//! for every backend.
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
//! **Names.** Each export and dylib symbol may also be named `kardia_*`
//! (`kardia_meta`, `kardia_new`, …). The host looks each one up as
//! `kardia_*` first and falls back to `cordis_*`, so a guest may export
//! either set, or both (the SDKs' guests export the same functions under
//! both, and the host then calls the `kardia_*` one). WASM host functions are
//! offered under both import modules, `kardia` and `cordis`, and a module may
//! import from either or both.
//!
//! Host functions (WASM imports from module `cordis` or `kardia`; for a dylib, function
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
//!
//! ## Process transport
//!
//! A process guest exchanges one JSON object per line over its stdin and
//! stdout (stdout is reserved for this; a guest logs through `log` or
//! stderr, whose last 16 KiB the exit report keeps).
//!
//! **Version 1.** Each export arrives as `{ export, input }`, with `export`
//! one of `meta`, `apply`, `call`, `dispose` and `input` the message (`null`
//! if empty). While it runs, the guest may send `{ call: "log", level, text }`
//! (no reply), or `{ call: "provide", key, value }`, `{ call: "view", key }`,
//! `{ call: "invoke", key, method, args }`, each answered with `{ result }`
//! or `{ error }`; `{ output }` sets the result text of the export, and
//! `{ done: status }` ends it. After `dispose` the process is killed; a
//! guest exits when its stdin closes.
//!
//! **`kardia-guest/2`.** The host's first line offers it as an export:
//! `{ export: "hello", input: { protocol: "kardia-guest/2" } }`. A version 2
//! guest answers `{ ready: "kardia-guest/2" }`; a version 1 guest answers
//! `{ done: 1 }`, as it does to any export it does not know, and is served
//! version 1. Under version 2, every export carries an `id`, and every
//! message about it (host calls, their replies, `output`, `done`) carries the
//! same `id`. The host may also send `{ cancel: id }` (the guest may stop the
//! export early; its result is dropped) and `{ ping: n }`, which the guest
//! answers with `{ pong: n }` even while an export runs.
//!
//! An export that outlives its deadline kills the process, as does a line
//! that is not JSON or longer than the frame limit; a process that ends
//! fails its fiber ([`GuestExited`]).

mod graph;
#[cfg(any(feature = "wasm", feature = "dylib", feature = "process"))]
mod guest;
mod hmr;
mod watch;

#[cfg(feature = "dylib")]
mod dylib;
#[cfg(feature = "process")]
mod process;
#[cfg(feature = "wasm")]
mod wasm;

#[cfg(feature = "dylib")]
pub use dylib::DylibSource;
pub use graph::{Exports, Imports, ModuleCode, ModuleGraph, ModuleId, ModuleSource};
#[cfg(any(feature = "wasm", feature = "dylib", feature = "process"))]
pub use guest::{
    CallControl, Export, GuestCode, GuestHost, GuestInstance, GuestMeta, GuestService, guest_module, guest_plugin,
};
pub use hmr::{Hmr, HmrConfig, HmrExt, ImportFailure, ReconcileResult};
#[cfg(feature = "process")]
pub use process::{
    ExitCause, GuestChild, GuestCommand, GuestExit, GuestExited, GuestHandle, GuestProcess, GuestStarted, ProcessGuest,
    ProcessSource, Spawner, StdSpawner,
};
#[cfg(feature = "wasm")]
pub use wasm::{WasmLimits, WasmSource};
pub use watch::WatchConfig;
