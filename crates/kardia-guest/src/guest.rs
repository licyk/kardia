//! The `Guest` trait a plugin implements, and the requests it answers.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{Error, Host, Meta, Result};

/// A guest plugin (or a module exporting a value, see [`Meta::value`]).
/// Each fiber running the plugin gets its own instance.
///
/// [`call`](Guest::call) and [`event`](Guest::event) take `&self` because
/// they may run at the same time on a process guest under
/// `kardia-guest/3`: calls of one instance overlap, and an event reaches
/// the instance while a call waits for its host. State they share goes
/// behind a lock or an atomic; `apply` and `dispose` run alone.
pub trait Guest: Sized + Send + Sync + 'static {
    /// The plugin's configuration; `null` decodes as `{}` when it fails as is.
    type Config: DeserializeOwned;

    /// What the module depends on and exports.
    fn meta() -> Meta;

    /// Starts an instance with its configuration and the values of the
    /// module's dependencies (by resolved path).
    fn apply(host: &Host, config: Self::Config, imports: Map<String, Value>) -> Result<Self>;

    /// Answers a call to a service the instance provided. Calls may
    /// overlap on a process guest under `kardia-guest/3`.
    fn call(&self, host: &Host, request: Request) -> Result<Value> {
        let _ = host;
        Err(Error::msg(format!("{} has no method {}", request.key, request.method)))
    }

    /// Receives an event the host sent outside of any call, while the
    /// instance runs; the host named it with `event`. A guest that receives
    /// none ignores it. Only a process guest under `kardia-guest/3` gets
    /// events; the WASM and dylib backends have no channel to a running
    /// guest, so a host's `emit` fails there.
    fn event(&self, host: &Host, name: &str, payload: Value) {
        let _ = (host, name, payload);
    }

    /// Runs when the fiber is disposed, before the instance is dropped.
    fn dispose(self, host: &Host) {
        let _ = host;
    }
}

/// A call to a service provided by the guest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub key: String,
    pub method: String,
    pub args: Value,
}
