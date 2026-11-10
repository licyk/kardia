//! The log levels and the host calls a guest makes through `Host`.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

use crate::{Error, Host, Result};

/// The id of the running `kardia-guest/2` export (`0`: none), and the last id
/// its host cancelled; set by the process transport.
pub(crate) static RUNNING: AtomicU64 = AtomicU64::new(0);
pub(crate) static CANCELLED: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
}

impl Host {
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

    /// Calls a service provided by a guest.
    pub fn invoke(&self, key: &str, method: &str, args: &Value) -> Result<Value> {
        let status = self.raw_invoke(key.as_bytes(), method.as_bytes(), &serde_json::to_vec(args)?);
        Ok(serde_json::from_slice(&self.reply(status)?)?)
    }

    /// Whether the host cancelled the running export, so a long call may stop
    /// early. Only a process guest served under `kardia-guest/2` is told;
    /// elsewhere this is always `false`.
    pub fn cancelled(&self) -> bool {
        let running = RUNNING.load(Ordering::Acquire);
        running != 0 && CANCELLED.load(Ordering::Acquire) == running
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
