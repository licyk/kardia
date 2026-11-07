use std::future::Future;

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::Value;

use crate::protocol::BridgeError;

/// What the host exposes to clients under a name: a JSON value they can
/// read, and methods they can call.
///
/// A closure `Fn(String, Value) -> impl Future<Output = Result<Value, BridgeError>>`
/// is an endpoint with methods and a `null` value; a [`Value`] is an endpoint
/// with a value and no methods.
///
/// `args` is client input: decode it into a typed struct first thing, and
/// answer a malformed one with [`BridgeError::invalid`], so raw JSON goes no
/// further than the endpoint.
pub trait Endpoint: Send + Sync + 'static {
    /// Calls `method` with `args`. The future runs on the bridge's runtime.
    fn call(&self, method: String, args: Value) -> BoxFuture<'static, Result<Value, BridgeError>>;

    /// The JSON value clients see for this service. Read once, when it is exposed.
    fn value(&self) -> Value {
        Value::Null
    }
}

impl<F, Fut> Endpoint for F
where
    F: Fn(String, Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Value, BridgeError>> + Send + 'static,
{
    fn call(&self, method: String, args: Value) -> BoxFuture<'static, Result<Value, BridgeError>> {
        self(method, args).boxed()
    }
}

impl Endpoint for Value {
    fn call(&self, method: String, _: Value) -> BoxFuture<'static, Result<Value, BridgeError>> {
        futures::future::ready(Err(BridgeError::invalid(format!("unknown method {method}")))).boxed()
    }

    fn value(&self) -> Value {
        self.clone()
    }
}
