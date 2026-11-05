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

mod ffi;

use std::fmt;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
pub use serde_json::{Map, Value, json};

pub use ffi::Host;
#[cfg(not(target_family = "wasm"))]
pub use ffi::serve;

/// A guest plugin (or a module exporting a value, see [`Meta::value`]).
/// Each fiber running the plugin gets its own instance.
pub trait Guest: Sized + 'static {
    /// The plugin's configuration; `null` decodes as `{}` when it fails as is.
    type Config: DeserializeOwned;

    /// What the module depends on and exports.
    fn meta() -> Meta;

    /// Starts an instance with its configuration and the values of the
    /// module's dependencies (by resolved path).
    fn apply(host: &Host, config: Self::Config, imports: Map<String, Value>) -> Result<Self>;

    /// Answers a call to a service the instance provided.
    fn call(&mut self, host: &Host, request: Request) -> Result<Value> {
        let _ = host;
        Err(Error::msg(format!("{} has no method {}", request.key, request.method)))
    }

    /// Runs when the fiber is disposed, before the instance is dropped.
    fn dispose(self, host: &Host) {
        let _ = host;
    }
}

/// The metadata of a module.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Meta {
    /// Module paths, relative to this module.
    pub deps: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin: Option<PluginMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PluginMeta {
    pub name: Option<String>,
    /// Services the plugin reads; it starts once they are available.
    pub inject: Vec<String>,
    /// Services the plugin provides.
    pub provide: Vec<String>,
}

impl Meta {
    /// A module exporting a plugin.
    pub fn plugin(name: impl Into<String>) -> Self {
        Self { plugin: Some(PluginMeta { name: Some(name.into()), ..Default::default() }), ..Default::default() }
    }

    /// A module exporting a value, which dependent modules receive as an import.
    pub fn value(value: Value) -> Self {
        Self { value: Some(value), ..Default::default() }
    }

    pub fn dep(mut self, path: impl Into<String>) -> Self {
        self.deps.push(path.into());
        self
    }

    pub fn inject(mut self, key: impl Into<String>) -> Self {
        self.plugin.get_or_insert_default().inject.push(key.into());
        self
    }

    pub fn provide(mut self, key: impl Into<String>) -> Self {
        self.plugin.get_or_insert_default().provide.push(key.into());
        self
    }
}

/// A call to a service provided by the guest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub key: String,
    pub method: String,
    pub args: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
}

/// An error message, reported to the host. Any displayable error converts
/// into it with `?`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(String);

impl Error {
    pub fn msg(message: impl fmt::Display) -> Self {
        Self(message.to_string())
    }

    pub fn message(&self) -> &str {
        &self.0
    }
}

impl<E: fmt::Display> From<E> for Error {
    fn from(error: E) -> Self {
        Self::msg(error)
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Host {
    pub fn log(&self, level: Level, text: &str) {
        self.raw_log(level as i32, text.as_bytes());
    }

    /// Provides the service `key`, whose JSON view is `value`; calls to it
    /// arrive at [`Guest::call`]. It is withdrawn when the fiber is disposed.
    pub fn provide(&self, key: &str, value: &Value) -> Result<()> {
        let status = self.raw_provide(key.as_bytes(), &serde_json::to_vec(value)?);
        self.reply(status).map(drop)
    }

    /// The JSON view of a service declared in [`PluginMeta::inject`] (or
    /// provided by this plugin or its ancestors).
    pub fn view(&self, key: &str) -> Result<Value> {
        let status = self.raw_view(key.as_bytes());
        Ok(serde_json::from_slice(&self.reply(status)?)?)
    }

    /// Calls a service provided by a guest.
    pub fn invoke(&self, key: &str, method: &str, args: &Value) -> Result<Value> {
        let status = self.raw_invoke(key.as_bytes(), method.as_bytes(), &serde_json::to_vec(args)?);
        Ok(serde_json::from_slice(&self.reply(status)?)?)
    }

    /// Reads the message a host call left: its result, or its error.
    fn reply(&self, status: i32) -> Result<Vec<u8>> {
        if status >= 0 {
            return Ok(self.raw_read(status as usize));
        }
        let message = self.raw_read((-1 - status) as usize);
        Err(Error::msg(String::from_utf8_lossy(&message)))
    }
}

/// Exports `$guest` with the guest ABI of the target: WASM exports on
/// `wasm32`, C symbols for a dynamic library elsewhere.
#[macro_export]
macro_rules! export {
    ($guest:ty) => {
        #[cfg(target_family = "wasm")]
        const _: () = {
            #[unsafe(no_mangle)]
            pub extern "C" fn cordis_meta() -> i32 {
                $crate::__private::wasm_meta::<$guest>()
            }

            #[unsafe(no_mangle)]
            pub extern "C" fn cordis_apply(len: usize) -> i32 {
                $crate::__private::wasm_apply::<$guest>(len)
            }

            #[unsafe(no_mangle)]
            pub extern "C" fn cordis_call(len: usize) -> i32 {
                $crate::__private::wasm_call::<$guest>(len)
            }

            #[unsafe(no_mangle)]
            pub extern "C" fn cordis_dispose() {
                $crate::__private::wasm_dispose::<$guest>()
            }
        };

        #[cfg(not(target_family = "wasm"))]
        const _: () = {
            use $crate::__private::{HostApi, State};

            #[unsafe(no_mangle)]
            pub extern "C" fn cordis_new() -> State {
                $crate::__private::dylib_new::<$guest>()
            }

            /// # Safety
            /// Called by the host only.
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn cordis_meta(_: State, host: *const HostApi) -> i32 {
                unsafe { $crate::__private::dylib_meta::<$guest>(host) }
            }

            /// # Safety
            /// Called by the host only.
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn cordis_apply(state: State, host: *const HostApi, len: usize) -> i32 {
                unsafe { $crate::__private::dylib_apply::<$guest>(state, host, len) }
            }

            /// # Safety
            /// Called by the host only.
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn cordis_call(state: State, host: *const HostApi, len: usize) -> i32 {
                unsafe { $crate::__private::dylib_call::<$guest>(state, host, len) }
            }

            /// # Safety
            /// Called by the host only.
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn cordis_dispose(state: State, host: *const HostApi) {
                unsafe { $crate::__private::dylib_dispose::<$guest>(state, host) }
            }
        };
    };
}

#[doc(hidden)]
pub mod __private {
    pub use crate::ffi::*;
}
