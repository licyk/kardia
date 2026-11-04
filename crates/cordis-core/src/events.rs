use std::any::{Any, TypeId, type_name};
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::FutureExt;
use futures::future::{BoxFuture, join_all};
use parking_lot::Mutex;
use serde_json::Value;

use crate::config::ErasedConfig;
use crate::context::Context;
use crate::effect::EffectHandle;
use crate::error::{AggregateError, Error, Result};
use crate::fiber::{Fiber, FiberState};
use crate::key::{FiberId, Key, RealmId};
use crate::root::RootInner;
use crate::scope::Scope;

/// A typed event. Listeners are registered per event type.
pub trait Event: Any + Send + Sync {
    type Output: Bailable;

    /// Whether a listener registered in `listener` receives this event.
    fn filter(&self, _listener: &Context) -> bool {
        true
    }
}

/// Listener results that can stop a `bail` / `serial` dispatch.
pub trait Bailable: Send + 'static {
    fn is_bailed(&self) -> bool {
        false
    }

    /// A failure reported by a `parallel` dispatch.
    fn into_error(self) -> Option<Error>
    where
        Self: Sized,
    {
        None
    }
}

impl Bailable for () {}

impl Bailable for bool {
    fn is_bailed(&self) -> bool {
        *self
    }
}

impl<T: Send + 'static> Bailable for Option<T> {
    fn is_bailed(&self) -> bool {
        self.is_some()
    }
}

impl<T: Bailable> Bailable for Result<T, Error> {
    fn is_bailed(&self) -> bool {
        self.as_ref().map_or(true, Bailable::is_bailed)
    }

    fn into_error(self) -> Option<Error> {
        self.err()
    }
}

type SyncListener<E> = Box<dyn Fn(&E) -> <E as Event>::Output + Send + Sync>;
type AsyncListener<E> = Box<dyn Fn(&E) -> BoxFuture<'static, <E as Event>::Output> + Send + Sync>;

enum Callback<E: Event> {
    Sync(SyncListener<E>),
    Async(AsyncListener<E>),
}

pub(crate) struct ListenerEntry {
    id: u64,
    fiber: FiberId,
    scope: Arc<Scope>,
    removed: AtomicBool,
    callback: Box<dyn Any + Send + Sync>,
}

impl ListenerEntry {
    fn callback<E: Event>(&self) -> &Callback<E> {
        self.callback.downcast_ref().expect("listener registered for this event type")
    }
}

/// A deferred dispatch of an internal event, built after the critical section.
type Dispatch = Box<dyn FnOnce(&Arc<RootInner>) + Send>;

pub(crate) struct Emit(Dispatch);

impl Emit {
    pub(crate) fn new<E: Event>(make: impl FnOnce(&Arc<RootInner>) -> E + Send + 'static) -> Self {
        Self(Box::new(move |root| {
            let event = make(root);
            Context::of_root(root).emit(&event);
        }))
    }

    pub(crate) fn run(self, root: &Arc<RootInner>) {
        (self.0)(root)
    }
}

impl Context {
    /// Registers a listener as an effect of the current fiber.
    pub fn on<E: Event>(&self, listener: impl Fn(&E) -> E::Output + Send + Sync + 'static) -> Result<EffectHandle> {
        self.listen(Callback::Sync(Box::new(listener)), false)
    }

    /// Registers a listener that runs before the ones registered so far.
    pub fn on_prepend<E: Event>(
        &self,
        listener: impl Fn(&E) -> E::Output + Send + Sync + 'static,
    ) -> Result<EffectHandle> {
        self.listen(Callback::Sync(Box::new(listener)), true)
    }

    /// Registers an asynchronous listener. `emit` and `bail` start it
    /// without waiting; `serial` and `parallel` await it.
    pub fn on_async<E, F, Fut>(&self, listener: F) -> Result<EffectHandle>
    where
        E: Event,
        F: Fn(&E) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = E::Output> + Send + 'static,
    {
        self.listen(Callback::Async(Box::new(move |event: &E| listener(event).boxed())), false)
    }

    /// Registers a listener that runs at most once.
    pub fn once<E: Event>(&self, listener: impl FnOnce(&E) -> E::Output + Send + 'static) -> Result<EffectHandle>
    where
        E::Output: Default,
    {
        let listener = Mutex::new(Some(listener));
        let handle: Arc<Mutex<Option<EffectHandle>>> = Default::default();
        let own = handle.clone();
        let registered = self.on(move |event: &E| {
            let Some(listener) = listener.lock().take() else { return Default::default() };
            if let Some(handle) = own.lock().take() {
                drop(handle.dispose());
            }
            listener(event)
        })?;
        *handle.lock() = Some(registered.clone());
        Ok(registered)
    }

    /// Calls every listener; asynchronous ones are started, not awaited.
    pub fn emit<E: Event>(&self, event: &E) {
        for entry in self.listeners_for(event) {
            if entry.removed.load(Ordering::Acquire) {
                continue;
            }
            match entry.callback::<E>() {
                Callback::Sync(listener) => {
                    if let Err(panic) = catch_unwind(AssertUnwindSafe(|| listener(event))) {
                        self.logger().log_error(&Error::from_panic(panic));
                    }
                }
                Callback::Async(listener) => {
                    tokio::spawn(listener(event));
                }
            }
        }
    }

    /// Calls synchronous listeners until one returns a bailing result.
    pub fn bail<E: Event>(&self, event: &E) -> Option<E::Output> {
        for entry in self.listeners_for(event) {
            if entry.removed.load(Ordering::Acquire) {
                continue;
            }
            match entry.callback::<E>() {
                Callback::Sync(listener) => {
                    let result = listener(event);
                    if result.is_bailed() {
                        return Some(result);
                    }
                }
                Callback::Async(listener) => {
                    tokio::spawn(listener(event));
                }
            }
        }
        None
    }

    /// Calls listeners one after another, awaiting each, until one bails.
    pub async fn serial<E: Event>(&self, event: &E) -> Option<E::Output> {
        for entry in self.listeners_for(event) {
            if entry.removed.load(Ordering::Acquire) {
                continue;
            }
            let result = match entry.callback::<E>() {
                Callback::Sync(listener) => listener(event),
                Callback::Async(listener) => listener(event).await,
            };
            if result.is_bailed() {
                return Some(result);
            }
        }
        None
    }

    /// Calls all listeners concurrently and collects their failures.
    pub async fn parallel<E: Event>(&self, event: &E) -> Result<(), AggregateError> {
        let mut tasks = Vec::new();
        for entry in self.listeners_for(event) {
            if entry.removed.load(Ordering::Acquire) {
                continue;
            }
            let task = match entry.callback::<E>() {
                Callback::Sync(listener) => {
                    let result = catch_unwind(AssertUnwindSafe(|| listener(event)));
                    futures::future::ready(result.map_err(Error::from_panic)).boxed()
                }
                Callback::Async(listener) => {
                    AssertUnwindSafe(listener(event)).catch_unwind().map(|r| r.map_err(Error::from_panic)).boxed()
                }
            };
            tasks.push(task);
        }
        let errors: Vec<Error> = join_all(tasks)
            .await
            .into_iter()
            .filter_map(|result| match result {
                Ok(output) => output.into_error(),
                Err(error) => Some(error),
            })
            .collect();
        if errors.is_empty() {
            return Ok(());
        }
        let message = format!("one or more listeners of event {} failed", short_name::<E>());
        Err(AggregateError { message, errors })
    }

    /// Number of listeners currently registered for `E`.
    pub fn listener_count<E: Event>(&self) -> usize {
        self.root.with(|st| st.listeners.get(&TypeId::of::<E>()).map_or(0, Vec::len))
    }

    fn listen<E: Event>(&self, callback: Callback<E>, prepend: bool) -> Result<EffectHandle> {
        let label = format!("ctx.on({})", short_name::<E>());
        let root = Arc::downgrade(&self.root);
        self.effect_sync(label, |scope| {
            let entry = self.root.run(|st| {
                st.next_listener += 1;
                let entry = Arc::new(ListenerEntry {
                    id: st.next_listener,
                    fiber: self.fiber,
                    scope: self.scope.clone(),
                    removed: AtomicBool::new(false),
                    callback: Box::new(callback),
                });
                let list = st.listeners.entry(TypeId::of::<E>()).or_default();
                if prepend {
                    list.insert(0, entry.clone());
                } else {
                    list.push(entry.clone());
                }
                entry
            });
            scope.defer_fn(move || {
                entry.removed.store(true, Ordering::Release);
                let Some(root) = root.upgrade() else { return };
                root.run(|st| {
                    let type_id = TypeId::of::<E>();
                    if let Some(list) = st.listeners.get_mut(&type_id) {
                        list.retain(|other| other.id != entry.id);
                        if list.is_empty() {
                            st.listeners.remove(&type_id);
                        }
                    }
                    st.drop_later(entry);
                });
            });
            Ok(())
        })
    }

    fn listeners_for<E: Event>(&self, event: &E) -> Vec<Arc<ListenerEntry>> {
        let entries = self.root.with(|st| st.listeners.get(&TypeId::of::<E>()).cloned().unwrap_or_default());
        entries
            .into_iter()
            .filter(|entry| {
                let listener = Context { root: self.root.clone(), fiber: entry.fiber, scope: entry.scope.clone() };
                event.filter(&listener)
            })
            .collect()
    }
}

fn short_name<E>() -> &'static str {
    let name = type_name::<E>();
    name.rsplit("::").next().unwrap_or(name)
}

/// A fiber was created (`internal/plugin` on insertion).
pub struct PluginInserted {
    pub fiber: Fiber,
}

/// A fiber was retired (`internal/plugin` on removal).
pub struct PluginRemoved {
    pub fiber: Fiber,
}

pub struct StatusChanged {
    pub fiber: Fiber,
    pub old: FiberState,
    pub new: FiberState,
}

/// A binding of `key` in `realm` appeared or disappeared. Delivered only to
/// listeners whose context resolves `key` to the same realm.
pub struct ServiceChanged {
    pub key: Key,
    pub realm: RealmId,
}

/// A fiber's configuration was replaced through `update` (write-back point).
pub struct ConfigCommitted {
    pub fiber: Fiber,
    pub config: Value,
}

/// A fiber was moved to another plugin implementation by `registry.replace`.
pub struct Replaced {
    pub fiber: Fiber,
}

impl Event for PluginInserted {
    type Output = ();
}

impl Event for PluginRemoved {
    type Output = ();
}

impl Event for StatusChanged {
    type Output = ();
}

impl Event for ConfigCommitted {
    type Output = ();
}

impl Event for Replaced {
    type Output = ();
}

impl Event for ServiceChanged {
    type Output = ();

    fn filter(&self, listener: &Context) -> bool {
        listener.realm(&self.key) == Some(self.realm)
    }
}

pub(crate) struct UpdateHook {
    removed: AtomicBool,
    accept: Box<dyn Fn(&ErasedConfig) -> bool + Send + Sync>,
}

impl UpdateHook {
    pub(crate) fn accepts(&self, config: &ErasedConfig) -> bool {
        !self.removed.load(Ordering::Acquire) && (self.accept)(config)
    }
}

/// A configuration resolution hook: receives the source configuration and
/// the next stage (ultimately the plugin's own decoding).
pub type ConfigHook =
    Arc<dyn Fn(&Fiber, Value, &dyn Fn(Value) -> Result<ErasedConfig>) -> Result<ErasedConfig> + Send + Sync>;

impl Context {
    /// Offers configuration updates of the current fiber to `accept` before
    /// restarting it. Returning `true` takes the update over (no restart).
    /// The hook sees the decoded configuration; it is skipped if `C` does
    /// not match the plugin's configuration type.
    pub fn on_update<C: Any>(&self, accept: impl Fn(&C) -> bool + Send + Sync + 'static) -> Result<EffectHandle> {
        let hook = Arc::new(UpdateHook {
            removed: AtomicBool::new(false),
            accept: Box::new(move |config| config.downcast_ref::<C>().is_some_and(&accept)),
        });
        let root = Arc::downgrade(&self.root);
        let fiber = self.fiber;
        self.effect_sync("ctx.on_update()", |scope| {
            let id = self.root.run(|st| st.fiber_mut(fiber).map(|f| f.update_hooks.insert(hook.clone())));
            scope.defer_fn(move || {
                hook.removed.store(true, Ordering::Release);
                if let (Some(root), Some(id)) = (root.upgrade(), id) {
                    let removed = root.run(|st| st.fiber_mut(fiber).and_then(|f| f.update_hooks.remove(id)));
                    drop(removed);
                }
            });
            Ok(())
        })
    }

    /// Registers a configuration resolution hook for every fiber of the root.
    pub fn on_config(&self, hook: ConfigHook) -> Result<EffectHandle> {
        let root = Arc::downgrade(&self.root);
        self.effect_sync("ctx.on_config()", |scope| {
            let id = self.root.run(|st| st.config_hooks.insert(hook));
            scope.defer_fn(move || {
                if let Some(root) = root.upgrade() {
                    let removed = root.run(|st| st.config_hooks.remove(id));
                    drop(removed);
                }
            });
            Ok(())
        })
    }
}
