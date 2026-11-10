//! The host side of a guest instance: what its host calls act on.

use std::sync::{Arc, Weak};

use kardia_core::{Context, Error, Level, Result};
use serde_json::Value;

use crate::guest::{GuestMeta, GuestService, Slot};

/// The host side of one guest instance: the fiber it runs in, and the host
/// calls of the guest ABI (`log`, `provide`, `view`, `invoke`).
pub struct GuestHost {
    /// `None` while a module's metadata is read.
    ctx: Option<Context>,
    slot: Weak<Slot>,
    /// The plugin's metadata: it provides only what this declares.
    meta: Arc<GuestMeta>,
    /// The input of the current export, then the result of the last host
    /// call (the FFI backends' message).
    #[cfg(any(feature = "wasm", feature = "dylib"))]
    pub(crate) message: Vec<u8>,
    /// What the guest wrote: the result of the current export.
    #[cfg(any(feature = "wasm", feature = "dylib"))]
    pub(crate) output: Vec<u8>,
}

impl GuestHost {
    pub(crate) fn new(ctx: Option<Context>, slot: Weak<Slot>, meta: Arc<GuestMeta>) -> Self {
        Self {
            ctx,
            slot,
            meta,
            #[cfg(any(feature = "wasm", feature = "dylib"))]
            message: Vec::new(),
            #[cfg(any(feature = "wasm", feature = "dylib"))]
            output: Vec::new(),
        }
    }

    /// The context of the fiber running the guest; `None` while a module's
    /// metadata is read.
    pub fn context(&self) -> Option<&Context> {
        self.ctx.as_ref()
    }

    /// Logs as the plugin.
    pub fn log(&self, level: Level, text: &str) {
        if let Some(ctx) = &self.ctx {
            ctx.logger().log(level, text);
        }
    }

    /// Provides the service `key` as a [`GuestService`] whose JSON view is
    /// `value`; its calls reach this instance. A key the plugin's metadata
    /// does not declare in `provide` is refused, and nothing is provided.
    pub fn provide(&self, key: &str, value: Value) -> Result<()> {
        let ctx = self.ctx()?;
        if !self.meta.provide.iter().any(|declared| declared == key) {
            return Err(Error::msg(format!("cannot provide \"{key}\": the plugin does not declare it in provide")));
        }
        ctx.provide_serialized_as(key, GuestService::new(key, value, self.slot.clone()))?;
        Ok(())
    }

    /// The JSON view of a service, with the checked access of the plugin's
    /// declared `inject`.
    pub fn view(&self, key: &str) -> Result<Value> {
        Ok(self.ctx()?.view(key)?)
    }

    /// Calls a [`GuestService`].
    pub fn invoke(&self, key: &str, method: &str, args: Value) -> Result<Value> {
        self.ctx()?.get_as::<GuestService>(key)?.call(method, args)
    }

    #[cfg(feature = "process")]
    pub(crate) fn logger(&self) -> Option<kardia_core::Logger> {
        self.ctx.as_ref().map(Context::logger)
    }

    fn ctx(&self) -> Result<&Context> {
        self.ctx.as_ref().ok_or_else(|| Error::msg("host calls are unavailable while reading module metadata"))
    }
}

/// The FFI backends' side: bytes in, the status convention out.
#[cfg(any(feature = "wasm", feature = "dylib"))]
impl GuestHost {
    pub(crate) fn begin(&mut self, input: Vec<u8>) {
        self.message = input;
        self.output.clear();
    }

    /// The result of the export that returned `status`.
    pub(crate) fn finish(&mut self, status: i32) -> Result<Vec<u8>> {
        let output = std::mem::take(&mut self.output);
        match status {
            0 => Ok(output),
            _ if output.is_empty() => Err(Error::msg(format!("guest failed with status {status}"))),
            _ => Err(Error::msg(String::from_utf8_lossy(&output).into_owned())),
        }
    }

    pub(crate) fn write(&mut self, bytes: &[u8]) {
        self.output = bytes.to_vec();
    }

    pub(crate) fn log_raw(&self, level_: i32, text: &[u8]) {
        self.log(level(level_.into()), &String::from_utf8_lossy(text));
    }

    pub(crate) fn provide_raw(&mut self, key: &[u8], value: &[u8]) -> i32 {
        let result = (|| {
            self.provide(utf8(key)?, serde_json::from_slice(value)?)?;
            Ok(Vec::new())
        })();
        self.reply(result)
    }

    pub(crate) fn view_raw(&mut self, key: &[u8]) -> i32 {
        let result = (|| Ok(serde_json::to_vec(&self.view(utf8(key)?)?)?))();
        self.reply(result)
    }

    pub(crate) fn invoke_raw(&mut self, key: &[u8], method: &[u8], args: &[u8]) -> i32 {
        let result =
            (|| Ok(serde_json::to_vec(&self.invoke(utf8(key)?, utf8(method)?, serde_json::from_slice(args)?)?)?))();
        self.reply(result)
    }

    /// Stores the result as the message: returns its length, or `-1 - len`
    /// for an error message.
    fn reply(&mut self, result: Result<Vec<u8>>) -> i32 {
        match result {
            Ok(bytes) => {
                self.message = bytes;
                self.message.len() as i32
            }
            Err(error) => {
                self.message = error.to_string().into_bytes();
                -1 - self.message.len() as i32
            }
        }
    }
}

/// A log level of the ABI (`0` error … `3` debug); anything else is info.
pub(crate) fn level(level: i64) -> Level {
    u8::try_from(level).ok().and_then(|level| Level::try_from(level).ok()).unwrap_or(Level::Info)
}

#[cfg(any(feature = "wasm", feature = "dylib"))]
fn utf8(bytes: &[u8]) -> Result<&str> {
    Ok(std::str::from_utf8(bytes)?)
}
