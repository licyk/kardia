use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use kardia_core::{EffectHandle, Fiber};
use parking_lot::Mutex;
use serde::{Serialize, Serializer};
use serde_json::Value;
use tokio::sync::oneshot;

use crate::protocol::{BridgeError, HostMessage};

/// The host-to-client half of a session: delivers JSON messages to the
/// client, such as through a `tauri::ipc::Channel`. It must not block.
pub trait Outbox: Send + Sync + 'static {
    /// Delivers one message; `false` when the client is gone, which closes the session.
    fn send(&self, message: Value) -> bool;
}

type Reply = oneshot::Sender<Result<Value, BridgeError>>;

/// One client connection. Its fiber owns the services the client provides,
/// so they are withdrawn when the session ends.
pub(crate) struct Session {
    pub(crate) id: u64,
    pub(crate) client: String,
    outbox: Arc<dyn Outbox>,
    fiber: Fiber,
    timeout: Duration,
    closed: AtomicBool,
    next_request: AtomicU64,
    pending: Mutex<HashMap<u64, Reply>>,
    provided: Mutex<HashMap<String, EffectHandle>>,
}

impl Session {
    pub(crate) fn new(id: u64, client: String, outbox: Arc<dyn Outbox>, fiber: Fiber, timeout: Duration) -> Self {
        Self {
            id,
            client,
            outbox,
            fiber,
            timeout,
            closed: AtomicBool::new(false),
            next_request: AtomicU64::new(0),
            pending: Mutex::new(HashMap::new()),
            provided: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Delivers a message; `false` if the client is gone (the caller then closes the session).
    pub(crate) fn send(&self, message: &HostMessage<'_>) -> bool {
        !self.is_closed() && self.outbox.send(message.to_value())
    }

    /// Calls a method of a service the client provides, and waits for its answer.
    async fn request(&self, service: &str, method: &str, args: Value) -> Result<Value, BridgeError> {
        let id = self.next_request.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id, tx);
        if !self.send(&HostMessage::Request { id, service, method, args: &args }) {
            self.pending.lock().remove(&id);
            return Err(BridgeError::unavailable(format!("the client of {service} is gone")));
        }
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(BridgeError::unavailable(format!("the client of {service} is gone"))),
            Err(_) => {
                self.pending.lock().remove(&id);
                Err(BridgeError::timeout(format!("{service}.{method} did not answer within {:?}", self.timeout)))
            }
        }
    }

    /// Settles a request with the client's answer. Unknown or late answers are ignored.
    pub(crate) fn settle(&self, id: u64, result: Result<Value, BridgeError>) {
        if let Some(reply) = self.pending.lock().remove(&id) {
            let _ = reply.send(result);
        }
    }

    /// Provides a client service in the host app, as an effect of the session's fiber.
    pub(crate) fn provide(self: &Arc<Self>, name: String, value: Value) -> Result<(), BridgeError> {
        let mut provided = self.provided.lock();
        if provided.contains_key(&name) {
            return Err(BridgeError::invalid(format!("service {name} is already provided by this client")));
        }
        let service = RemoteService { name: name.clone(), value, session: Arc::downgrade(self) };
        let handle = self
            .fiber
            .context()
            .provide_serialized_as(name.as_str(), service)
            .map_err(|error| BridgeError::invalid(error.to_string()))?;
        provided.insert(name, handle);
        Ok(())
    }

    pub(crate) fn withdraw(&self, name: &str) {
        if let Some(handle) = self.provided.lock().remove(name) {
            drop(handle.dispose());
        }
    }

    /// Ends the session: fails pending requests, tells the client why (if
    /// `reason` is given) and withdraws its services. Runs on the bridge's runtime.
    pub(crate) fn close(&self, reason: Option<&str>) {
        if let Some(reason) = reason {
            self.send(&HostMessage::Closed { reason });
        }
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        for (_, reply) in self.pending.lock().drain() {
            let _ = reply.send(Err(BridgeError::unavailable("the session closed")));
        }
        self.provided.lock().clear();
        drop(self.fiber.dispose());
    }
}

/// A service provided by a client, as the host app sees it: the JSON value
/// the client provided it with, and its methods, called across the bridge.
///
/// Get it with `ctx.get_as::<RemoteService>(name)` (or `live_as`). A client
/// may close at any time, so a host plugin should not depend on one it can
/// run without: prefer `live_as` over declaring it in `inject`.
pub struct RemoteService {
    name: String,
    value: Value,
    session: Weak<Session>,
}

impl RemoteService {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn value(&self) -> &Value {
        &self.value
    }

    /// Calls a method in the client. Fails with `Unavailable` once the client
    /// is gone and with `Timeout` when it does not answer within the bridge's
    /// timeout, which needs the caller's runtime to have its time driver enabled.
    pub async fn call(&self, method: &str, args: Value) -> Result<Value, BridgeError> {
        let session = self
            .session
            .upgrade()
            .filter(|session| !session.is_closed())
            .ok_or_else(|| BridgeError::unavailable(format!("the client of {} is gone", self.name)))?;
        session.request(&self.name, method, args).await
    }
}

impl Serialize for RemoteService {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.value.serialize(serializer)
    }
}
