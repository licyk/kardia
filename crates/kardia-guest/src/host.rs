//! The log levels and the host calls a guest makes through `Host`.

use serde_json::Value;

use crate::{Error, Host, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
}

impl Host {
    /// A host the guest may keep past the export it got it in: under
    /// `kardia-guest/3`, its calls reach the host from any thread at any
    /// time while the instance runs. The WASM and dylib backends, and a
    /// process guest under version 1, have no channel outside an export:
    /// a call through a detached host there fails.
    pub fn detached(&self) -> Host {
        self.detach()
    }

    /// Whether this host reaches the host outside of an export (see
    /// [`detached`](Host::detached)).
    pub fn is_detached(&self) -> bool {
        self.reaches_outside()
    }

    pub fn log(&self, level: Level, text: &str) {
        self.raw_log(level as i32, text.as_bytes());
    }

    /// Provides the service `key`, whose JSON view is `value`; calls to it
    /// arrive at [`Guest::call`](crate::Guest::call). It is withdrawn when the fiber is disposed.
    /// The key must be declared in [`PluginMeta::provide`](crate::PluginMeta::provide): a
    /// kardia-hmr 0.2.3 host refuses any other, and provides nothing.
    pub fn provide(&self, key: &str, value: &Value) -> Result<()> {
        let status = self.raw_provide(key.as_bytes(), &serde_json::to_vec(value)?);
        self.reply(status).map(drop)
    }

    /// The JSON view of a service declared in [`PluginMeta::inject`](crate::PluginMeta::inject) (or
    /// provided by this plugin or its ancestors).
    pub fn view(&self, key: &str) -> Result<Value> {
        let status = self.raw_view(key.as_bytes());
        Ok(serde_json::from_slice(&self.reply(status)?)?)
    }

    /// Calls a service provided by a guest, or an action the host registered
    /// under `key`. Through a detached host under `kardia-guest/3`, this
    /// reaches the host from any thread, between calls too.
    pub fn invoke(&self, key: &str, method: &str, args: &Value) -> Result<Value> {
        let status = self.raw_invoke(key.as_bytes(), method.as_bytes(), &serde_json::to_vec(args)?);
        Ok(serde_json::from_slice(&self.reply(status)?)?)
    }

    /// Whether the host cancelled the export this host belongs to, so a
    /// long call may stop early. Only a process guest served under
    /// `kardia-guest/3` is told; elsewhere this is always `false`.
    pub fn cancelled(&self) -> bool {
        self.was_cancelled()
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
