use std::sync::Arc;
use std::time::Duration;

use indexmap::IndexMap;
use kardia_core::{Context, EffectHandle, PluginDef, Result, Service, Svc};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::runtime::Handle;

use crate::endpoint::Endpoint;
use crate::protocol::{BridgeError, ClientRequest, HostMessage, PROTOCOL, ServiceInfo};
use crate::session::{Outbox, Session};

/// Options of a [`Bridge`].
#[derive(Debug, Clone)]
pub struct BridgeOptions {
    /// How long a call into a client may take before it fails with `Timeout`.
    pub timeout: Duration,
}

impl Default for BridgeOptions {
    fn default() -> Self {
        Self { timeout: Duration::from_secs(30) }
    }
}

/// The host side of the bridge, and the `bridge` service.
///
/// The application creates one, mounts [`Bridge::plugin`] in its kardia app
/// and keeps the handle to route client requests into [`Bridge::handle`]
/// (from a Tauri command, for example). Plugins inject `bridge` and
/// [`BridgeExt::expose`] services to clients; the bridge mirrors which of
/// them are exposed to every client, so client plugins depending on one run
/// exactly while it is exposed.
#[derive(Clone)]
pub struct Bridge {
    hub: Arc<Hub>,
}

impl Service for Bridge {
    const NAME: &'static str = "bridge";
}

impl Default for Bridge {
    fn default() -> Self {
        Self::new(BridgeOptions::default())
    }
}

impl Bridge {
    pub fn new(options: BridgeOptions) -> Self {
        Self { hub: Arc::new(Hub { timeout: options.timeout, state: Mutex::new(HubState::default()) }) }
    }

    /// The plugin that runs the bridge and provides it as `bridge`. While it
    /// is not active, every request fails with `Unavailable`; when it stops,
    /// every session is closed.
    pub fn plugin(&self) -> PluginDef {
        let bridge = self.clone();
        PluginDef::new(move |ctx, _| {
            let bridge = bridge.clone();
            async move {
                bridge.hub.start(&ctx)?;
                let hub = bridge.hub.clone();
                ctx.on_dispose(move || hub.stop())?;
                ctx.provide(bridge)?;
                Ok(())
            }
        })
        .name("bridge")
        .provides([Self::NAME])
    }

    /// Handles one client request (a JSON object, see the crate docs) and
    /// returns its result. An `open` request also needs the session's
    /// `outbox`. Safe to call from any thread or runtime: the work runs on
    /// the runtime the bridge plugin runs on.
    pub async fn handle(&self, request: Value, outbox: Option<Arc<dyn Outbox>>) -> Result<Value, BridgeError> {
        let runtime = self.hub.runtime()?;
        let hub = self.hub.clone();
        runtime.spawn(async move { hub.dispatch(request, outbox).await }).await.map_err(|error| {
            // the runtime the bridge ran on shut down under the request
            if error.is_cancelled() {
                BridgeError::unavailable("the bridge is not running")
            } else {
                BridgeError::failed(error.to_string())
            }
        })?
    }

    /// Sends an event to every client.
    pub fn emit(&self, name: &str, payload: &Value) {
        self.hub.broadcast(&HostMessage::Event { name, payload });
    }

    /// Closes every session of `client`, for example when its window is destroyed.
    pub fn close_client(&self, client: &str) {
        let sessions = self.hub.take_sessions(|session| session.client == client);
        self.hub.close_later(sessions, None);
    }
}

/// Exposing services through an injected `bridge`.
pub trait BridgeExt {
    /// Exposes `endpoint` to clients as `name`, as an effect of the calling
    /// plugin: it is withdrawn when that plugin stops. Fails if `name` is
    /// already exposed.
    fn expose(&self, name: &str, endpoint: impl Endpoint) -> Result<EffectHandle>;
}

impl BridgeExt for Svc<Bridge> {
    fn expose(&self, name: &str, endpoint: impl Endpoint) -> Result<EffectHandle> {
        let hub = self.hub.clone();
        let endpoint: Arc<dyn Endpoint> = Arc::new(endpoint);
        self.caller().effect_sync(format!("bridge.expose({name:?})"), |scope| {
            let id = hub.add_exposure(name, endpoint)?;
            let name = name.to_string();
            scope.defer_fn(move || hub.remove_exposure(&name, id));
            Ok(())
        })
    }
}

struct Hub {
    timeout: Duration,
    state: Mutex<HubState>,
}

#[derive(Default)]
struct HubState {
    /// The running bridge plugin: its runtime and context.
    host: Option<(Handle, Context)>,
    sessions: IndexMap<u64, Arc<Session>>,
    exposures: IndexMap<String, Exposure>,
    next_session: u64,
    next_exposure: u64,
}

struct Exposure {
    id: u64,
    value: Value,
    endpoint: Arc<dyn Endpoint>,
}

impl HubState {
    fn services(&self) -> Vec<ServiceInfo> {
        self.exposures
            .iter()
            .map(|(name, exposure)| ServiceInfo { name: name.clone(), id: exposure.id, value: exposure.value.clone() })
            .collect()
    }
}

impl Hub {
    fn start(&self, ctx: &Context) -> Result<()> {
        let mut state = self.state.lock();
        if state.host.is_some() {
            return Err(kardia_core::Error::msg("this bridge is already running in another plugin"));
        }
        state.host = Some((Handle::current(), ctx.clone()));
        Ok(())
    }

    fn stop(&self) {
        let sessions = {
            let mut state = self.state.lock();
            state.host = None;
            std::mem::take(&mut state.sessions)
        };
        for session in sessions.into_values() {
            session.close(Some("stopped"));
        }
    }

    fn runtime(&self) -> Result<Handle, BridgeError> {
        let state = self.state.lock();
        let (runtime, _) = state.host.as_ref().ok_or_else(|| BridgeError::unavailable("the bridge is not running"))?;
        Ok(runtime.clone())
    }

    async fn dispatch(self: Arc<Self>, request: Value, outbox: Option<Arc<dyn Outbox>>) -> Result<Value, BridgeError> {
        let request: ClientRequest = serde_json::from_value(request)
            .map_err(|error| BridgeError::invalid(format!("invalid request: {error}")))?;
        match request {
            ClientRequest::Open { protocol, client } => self.open(protocol, client, outbox),
            ClientRequest::Close { session } => {
                for session in self.take_sessions(|s| s.id == session) {
                    session.close(None);
                }
                Ok(Value::Null)
            }
            ClientRequest::Call { session, service, method, args } => {
                self.session(session)?;
                let endpoint = self.endpoint(&service)?;
                endpoint.call(method, args).await
            }
            ClientRequest::Resolve { session, id, value } => {
                self.session(session)?.settle(id, Ok(value));
                Ok(Value::Null)
            }
            ClientRequest::Reject { session, id, error } => {
                self.session(session)?.settle(id, Err(error));
                Ok(Value::Null)
            }
            ClientRequest::Provide { session, service, value } => {
                self.session(session)?.provide(service, value)?;
                Ok(Value::Null)
            }
            ClientRequest::Withdraw { session, service } => {
                self.session(session)?.withdraw(&service);
                Ok(Value::Null)
            }
        }
    }

    /// Opens a session, replacing earlier ones of the same client (a reloaded page).
    fn open(&self, protocol: u32, client: String, outbox: Option<Arc<dyn Outbox>>) -> Result<Value, BridgeError> {
        if protocol != PROTOCOL {
            return Err(BridgeError::invalid(format!(
                "protocol {protocol} is not supported: the host speaks protocol {PROTOCOL}"
            )));
        }
        let outbox = outbox.ok_or_else(|| BridgeError::invalid("opening a session needs a message stream"))?;
        for session in self.take_sessions(|session| session.client == client) {
            session.close(Some("replaced"));
        }
        let ctx = {
            let state = self.state.lock();
            state
                .host
                .as_ref()
                .map(|(_, ctx)| ctx.clone())
                .ok_or_else(|| BridgeError::unavailable("the bridge is not running"))?
        };
        let def = PluginDef::new(|_, _| async { Ok(()) }).name(format!("bridge-session:{client}"));
        let fiber = ctx.plugin_def(&def, Value::Null)?;
        let mut state = self.state.lock();
        state.next_session += 1;
        let id = state.next_session;
        state.sessions.insert(id, Arc::new(Session::new(id, client, outbox, fiber, self.timeout)));
        Ok(json!({ "session": id, "protocol": PROTOCOL, "services": state.services() }))
    }

    fn session(&self, id: u64) -> Result<Arc<Session>, BridgeError> {
        self.state
            .lock()
            .sessions
            .get(&id)
            .cloned()
            .ok_or_else(|| BridgeError::unavailable(format!("session {id} is closed")))
    }

    fn endpoint(&self, name: &str) -> Result<Arc<dyn Endpoint>, BridgeError> {
        let state = self.state.lock();
        let exposure = state
            .exposures
            .get(name)
            .ok_or_else(|| BridgeError::unavailable(format!("service {name} is not available")))?;
        Ok(exposure.endpoint.clone())
    }

    fn take_sessions(&self, predicate: impl Fn(&Session) -> bool) -> Vec<Arc<Session>> {
        let mut state = self.state.lock();
        let ids: Vec<u64> =
            state.sessions.values().filter(|session| predicate(session)).map(|session| session.id).collect();
        ids.iter().filter_map(|id| state.sessions.shift_remove(id)).collect()
    }

    /// Closes sessions on the bridge's runtime, since closing disposes their fibers.
    fn close_later(&self, sessions: Vec<Arc<Session>>, reason: Option<&'static str>) {
        if sessions.is_empty() {
            return;
        }
        let Ok(runtime) = self.runtime() else { return };
        runtime.spawn(async move {
            for session in sessions {
                session.close(reason);
            }
        });
    }

    fn add_exposure(&self, name: &str, endpoint: Arc<dyn Endpoint>) -> Result<u64> {
        let mut state = self.state.lock();
        if state.exposures.contains_key(name) {
            return Err(kardia_core::Error::msg(format!("service {name} is already exposed")));
        }
        state.next_exposure += 1;
        let id = state.next_exposure;
        let value = endpoint.value();
        state.exposures.insert(name.to_string(), Exposure { id, value, endpoint });
        self.announce(state);
        Ok(id)
    }

    fn remove_exposure(&self, name: &str, id: u64) {
        let mut state = self.state.lock();
        if state.exposures.get(name).is_some_and(|exposure| exposure.id == id) {
            state.exposures.shift_remove(name);
            self.announce(state);
        }
    }

    /// Sends the exposed services to every session. Called with the state
    /// locked, so a session sees every change in order, after the snapshot
    /// its `open` returned.
    fn announce(&self, state: parking_lot::MutexGuard<'_, HubState>) {
        let services = state.services();
        self.send_all(state, &HostMessage::Services { services: &services });
    }

    fn broadcast(&self, message: &HostMessage<'_>) {
        self.send_all(self.state.lock(), message);
    }

    fn send_all(&self, mut state: parking_lot::MutexGuard<'_, HubState>, message: &HostMessage<'_>) {
        let gone: Vec<u64> =
            state.sessions.values().filter(|session| !session.send(message)).map(|session| session.id).collect();
        let sessions: Vec<_> = gone.iter().filter_map(|id| state.sessions.shift_remove(id)).collect();
        drop(state);
        self.close_later(sessions, None);
    }
}
