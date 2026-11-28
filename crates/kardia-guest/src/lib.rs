//! Guest plugins in Rust for the WASM, dylib and process backends of `kardia-hmr`.
//!
//! Implement [`Guest`] and [`export!`] it from a `cdylib` crate. The same
//! source builds as a WASM module (`--target wasm32-unknown-unknown`) or as
//! a dynamic library for the host platform. For the process backend, a
//! binary's `main` calls [`serve::<G>()`](serve) instead; its stdout carries
//! the protocol (version 1, or `kardia-guest/3` with hosts that offer it), so
//! it logs through [`Host::log`] or stderr. Under version 3, calls of the
//! instance run on threads of their own, so [`Guest::call`] and
//! [`Guest::event`] take `&self` and may overlap: state they share goes
//! behind a lock or an atomic. A host kept with [`Host::detached`] reaches
//! the host from any thread, between exports too.
//!
//! Unlike the rest of kardia, this crate is licensed under MIT OR Apache-2.0,
//! so a guest that links it chooses its own licence.
//!
//! ```no_run
//! use kardia_guest::{Guest, Host, Level, Map, Meta, Request, Result, Value, json};
//!
//! struct Counter {
//!     count: std::sync::atomic::AtomicU64,
//! }
//!
//! impl Guest for Counter {
//!     type Config = Value;
//!
//!     fn meta() -> Meta {
//!         Meta::plugin("counter").provide("counter")
//!     }
//!
//!     fn apply(host: &Host, config: Value, _: Map<String, Value>) -> Result<Self> {
//!         host.provide("counter", &json!({ "config": config }))?;
//!         host.log(Level::Info, "counting");
//!         Ok(Counter { count: std::sync::atomic::AtomicU64::new(0) })
//!     }
//!
//!     fn call(&self, _: &Host, request: Request) -> Result<Value> {
//!         match request.method.as_str() {
//!             "increment" => {
//!                 let count = self.count.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
//!                 Ok(json!(count))
//!             }
//!             method => Err(format!("unknown method {method}").into()),
//!         }
//!     }
//! }
//!
//! kardia_guest::export!(Counter);
//! ```

mod error;
mod export;
mod ffi;
mod guest;
mod host;
mod meta;
#[cfg(not(target_family = "wasm"))]
mod serve;

pub use error::{Error, Result};
pub use guest::{Guest, Request};
pub use host::Level;
pub use meta::{Meta, PluginMeta};
pub use serde_json::{Map, Value, json};

pub use ffi::Host;
#[cfg(not(target_family = "wasm"))]
pub use serve::serve;

#[doc(hidden)]
pub mod __private {
    pub use crate::ffi::*;
}
