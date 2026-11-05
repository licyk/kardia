//! Guest plugins in Rust for the WASM, dylib and process backends of `kardia-hmr`.
//!
//! Implement [`Guest`] and [`export!`] it from a `cdylib` crate. The same
//! source builds as a WASM module (`--target wasm32-unknown-unknown`) or as
//! a dynamic library for the host platform. For the process backend, a
//! binary's `main` calls [`serve::<G>()`](serve) instead; its stdout carries
//! the protocol, so it logs through [`Host::log`] or stderr.
//!
//! ```no_run
//! use kardia_guest::{Guest, Host, Level, Map, Meta, Request, Result, Value, json};
//!
//! struct Counter {
//!     count: u64,
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
//!         Ok(Counter { count: 0 })
//!     }
//!
//!     fn call(&mut self, _: &Host, request: Request) -> Result<Value> {
//!         match request.method.as_str() {
//!             "increment" => {
//!                 self.count += 1;
//!                 Ok(json!(self.count))
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

pub use error::{Error, Result};
pub use guest::{Guest, Request};
pub use host::Level;
pub use meta::{Meta, PluginMeta};
pub use serde_json::{Map, Value, json};

pub use ffi::Host;
#[cfg(not(target_family = "wasm"))]
pub use ffi::serve;

#[doc(hidden)]
pub mod __private {
    pub use crate::ffi::*;
}
