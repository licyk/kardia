//! Services provided by guest plugins.

use std::future::Future;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use kardia_core::{Error, Result};
use serde::{Serialize, Serializer};
use serde_json::{Value, json};

#[cfg(not(feature = "process"))]
use crate::guest::bridge::disposed;
#[cfg(feature = "process")]
use crate::guest::bridge::gone;
use crate::guest::bridge::{blocking, run};
use crate::guest::{CallControl, Export, Slot};
#[cfg(feature = "process")]
use crate::process::GuestHandle;

/// A service provided by a guest plugin. Its JSON view is the value the
/// guest provided it with; its calls call into the guest instance.
pub struct GuestService {
    key: String,
    value: Value,
    instance: Weak<Slot>,
    /// Kept to tell why the guest is gone once its instance is.
    #[cfg(feature = "process")]
    handle: Option<GuestHandle>,
}

impl GuestService {
    pub(crate) fn new(key: &str, value: Value, instance: Weak<Slot>) -> Self {
        Self {
            key: key.into(),
            value,
            #[cfg(feature = "process")]
            handle: instance.upgrade().and_then(|slot| slot.handle()),
            instance,
        }
    }

    pub fn value(&self) -> &Value {
        &self.value
    }

    /// Calls the guest's `cordis_call` with `{ key, method, args }`, blocking
    /// until it answers or the transport's default timeout passes.
    pub fn call(&self, method: &str, args: Value) -> Result<Value> {
        self.call_with(method, args, &CallControl::default())
    }

    /// Like [`GuestService::call`], failing after `timeout`: a transport that
    /// can stop its guest then stops it, and its later calls fail too.
    pub fn call_timeout(&self, method: &str, args: Value, timeout: Duration) -> Result<Value> {
        self.call_with(method, args, &CallControl::new(Some(Instant::now() + timeout)))
    }

    /// Calls the guest off the async workers, like [`GuestService::call`],
    /// or [`GuestService::call_timeout`] with a `timeout`. Dropping the future
    /// cancels the call: the guest is told, and its result is dropped.
    pub fn call_async(
        &self,
        method: &str,
        args: Value,
        timeout: Option<Duration>,
    ) -> impl Future<Output = Result<Value>> + Send + 'static {
        let (instance, key, method, gone) = (self.instance.clone(), self.key.clone(), method.to_string(), self.gone());
        let input = request(&key, &method, args);
        let control = CallControl::new(timeout.map(|timeout| Instant::now() + timeout));
        async move {
            let (input, slot) = (input?, instance.upgrade().ok_or_else(gone)?);
            let cancel = CancelOnDrop(Some(control.clone()));
            let output = blocking(move || run(&slot, Export::Call, input, &control)).await;
            cancel.disarm();
            decode(&key, &method, &output?)
        }
    }

    /// Whether the instance can still answer: it is not disposed, and its
    /// process, if any, has not exited.
    pub fn is_available(&self) -> bool {
        self.instance.upgrade().is_some_and(|slot| slot.is_available())
    }

    /// The process running the guest, for a transport that has one.
    #[cfg(feature = "process")]
    pub fn handle(&self) -> Option<GuestHandle> {
        self.handle.clone()
    }

    fn call_with(&self, method: &str, args: Value, control: &CallControl) -> Result<Value> {
        let slot: Arc<Slot> = self.instance.upgrade().ok_or_else(self.gone())?;
        let output = run(&slot, Export::Call, request(&self.key, method, args)?, control)?;
        decode(&self.key, method, &output)
    }
}

impl GuestService {
    /// Why the guest cannot be called once its instance is gone.
    #[cfg(feature = "process")]
    fn gone(&self) -> impl FnOnce() -> Error + Send + 'static {
        let handle = self.handle.clone();
        move || gone(handle.as_ref())
    }

    #[cfg(not(feature = "process"))]
    fn gone(&self) -> impl FnOnce() -> Error + Send + 'static {
        disposed
    }
}

impl Serialize for GuestService {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.value.serialize(serializer)
    }
}

fn request(key: &str, method: &str, args: Value) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&json!({ "key": key, "method": method, "args": args }))?)
}

fn decode(key: &str, method: &str, output: &[u8]) -> Result<Value> {
    serde_json::from_slice(output).map_err(|error| Error::msg(format!("invalid result of {key}.{method}: {error}")))
}

/// Cancels the call when the caller drops its future before the result.
struct CancelOnDrop(Option<CallControl>);

impl CancelOnDrop {
    fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(control) = &self.0 {
            control.cancel();
        }
    }
}
