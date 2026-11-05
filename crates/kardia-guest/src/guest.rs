//! The `Guest` trait a plugin implements, and the requests it answers.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{Error, Host, Meta, Result};

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

/// A call to a service provided by the guest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub key: String,
    pub method: String,
    pub args: Value,
}
