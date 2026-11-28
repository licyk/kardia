//! The bridge plugin that runs a guest in a fiber, and the module code that
//! reads a guest's metadata.

use std::cell::RefCell;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use kardia_core::{Context, Disposer, Error, Inject, PluginDef, Result};
use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use crate::graph::{Exports, ModuleCode};
use crate::guest::meta::ModuleMeta;
use crate::guest::{
    CallControl, Export, GuestCode, GuestFailure, GuestFailureKind, GuestHost, GuestInstance, GuestMeta,
};
use crate::hmr::normalize;
#[cfg(feature = "process")]
use crate::process::{ExitCause, GuestExited, GuestHandle, GuestStarted};

/// The instance a fiber owns; empty once the fiber disposed it.
#[derive(Default)]
pub(crate) struct Slot {
    /// The lifecycle exports (`meta`, `apply`, `dispose`) hold this lock and
    /// the instance it guards; `call` only reads the instance, so it takes a
    /// clone of the handle and leaves the lock to them.
    instance: Mutex<Option<Arc<dyn GuestInstance>>>,
    disposed: AtomicBool,
    /// How many of the guest's host calls a transport is answering right now
    /// on a thread of its own: a call into the instance meanwhile would wait
    /// for the very export that made the host call.
    answering: std::sync::atomic::AtomicUsize,
    #[cfg(feature = "process")]
    handle: std::sync::OnceLock<GuestHandle>,
}

impl Slot {
    pub(crate) fn is_available(&self) -> bool {
        #[cfg(feature = "process")]
        if self.handle.get().is_some_and(|handle| handle.exit().is_some()) {
            return false;
        }
        !self.disposed.load(Ordering::Acquire)
    }

    #[cfg(feature = "process")]
    pub(crate) fn handle(&self) -> Option<GuestHandle> {
        self.handle.get().cloned()
    }

    /// The running instance, cloned out of the slot so a call need not hold
    /// the lock a lifecycle export takes; `None` once the slot is empty. A
    /// lifecycle export holds that lock while the guest runs, so a guest
    /// reaching back for its own instance must be turned away before this.
    pub(crate) fn running(&self) -> Option<Arc<dyn GuestInstance>> {
        self.instance.lock().clone()
    }

    /// Whether a call into the instance now would re-enter it: it is running
    /// on this thread, or a transport is answering its host call elsewhere.
    pub(crate) fn reentrant(&self) -> bool {
        let id = std::ptr::from_ref(self) as usize;
        ENTERED.with_borrow(|entered| entered.contains(&id)) || self.answering.load(Ordering::Acquire) > 0
    }

    /// Counts a host call the transport answers on a thread of its own.
    pub(crate) fn answering(slot: &Arc<Self>) -> Answering {
        slot.answering.fetch_add(1, Ordering::AcqRel);
        Answering(slot.clone())
    }

    /// Why an emptied slot cannot be called.
    #[cfg(feature = "process")]
    pub(crate) fn gone(&self) -> Error {
        gone(self.handle.get())
    }

    #[cfg(not(feature = "process"))]
    pub(crate) fn gone(&self) -> Error {
        disposed()
    }
}

/// Why a guest cannot be called once its instance is gone: its process
/// exited on its own, or its fiber disposed it.
#[cfg(feature = "process")]
pub(crate) fn gone(handle: Option<&GuestHandle>) -> Error {
    match handle.and_then(GuestHandle::exit) {
        Some(exit) if exit.cause != ExitCause::Disposed => exited(),
        _ => disposed(),
    }
}

/// Reads what a guest module depends on and exports from a fresh instance,
/// and wraps it as module code; its plugin runs `code` in a bridge plugin.
pub fn guest_module(id: &str, code: Arc<dyn GuestCode>) -> Result<ModuleCode> {
    let instance = code.instantiate(GuestHost::new(None, Weak::new(), Arc::default()))?;
    let instance: Arc<dyn GuestInstance> = instance.into();
    let control = CallControl::default();
    let output = instance.call(Export::Meta, Vec::new(), &control);
    instance.call(Export::Dispose, Vec::new(), &control)?;
    drop(instance);
    let meta: ModuleMeta = serde_json::from_slice(&output?)
        .map_err(|error| Error::msg(format!("invalid metadata of module {id}: {error}")))?;
    let deps: Vec<String> = meta.deps.iter().map(|dep| resolve(id, dep)).collect();
    let plugin = meta.plugin.map(Arc::new);
    Ok(ModuleCode::new(deps.clone(), move |imports| {
        Ok(match (&plugin, &meta.value) {
            (Some(plugin), _) => {
                let values: Map<String, Value> =
                    deps.iter().filter_map(|dep| Some((dep.clone(), imports.value(dep)?))).collect();
                Exports::Plugin(bridge(code.clone(), plugin.clone(), Value::Object(values), false))
            }
            (None, Some(value)) => Exports::Value(value.clone()),
            (None, None) => Exports::None,
        })
    }))
}

/// A plugin running `code`, with metadata from the caller instead of the
/// guest: making it starts nothing. Each instance's own metadata must match
/// `meta` when it starts, or its fiber fails.
pub fn guest_plugin(code: Arc<dyn GuestCode>, meta: GuestMeta) -> PluginDef {
    bridge(code, Arc::new(meta), Value::Object(Map::new()), true)
}

/// A plugin that instantiates the guest in each fiber and applies it with
/// `{ config, imports }`. Disposing the fiber disposes the instance. Every
/// guest call runs off the async workers.
fn bridge(code: Arc<dyn GuestCode>, meta: Arc<GuestMeta>, imports: Value, check: bool) -> PluginDef {
    let expected = meta.clone();
    let def = PluginDef::new(move |ctx, config| {
        let (code, expected) = (code.clone(), expected.clone());
        let input = json!({ "config": config, "imports": imports });
        async move {
            let slot = Arc::new(Slot::default());
            let host = GuestHost::new(Some(ctx.clone()), Arc::downgrade(&slot), expected.clone());
            let instance: Arc<dyn GuestInstance> = blocking(move || code.instantiate(host)).await?.into();
            #[cfg(feature = "process")]
            let handle = instance.handle();
            *slot.instance.lock() = Some(instance.clone());
            dispose_with(&ctx, &slot)?;
            #[cfg(feature = "process")]
            if let Some(handle) = &handle {
                let _ = slot.handle.set(handle.clone());
                ctx.emit(&GuestStarted { fiber: ctx.fiber(), handle: handle.clone() });
            }
            if check {
                let running = slot.clone();
                blocking(move || check_meta(&running, &expected)).await?;
            }
            let input = serde_json::to_vec(&input)?;
            blocking(move || run(&slot, &instance, Export::Apply, input, &CallControl::default())).await?;
            #[cfg(feature = "process")]
            if let Some(handle) = handle {
                watch_exit(&ctx, handle)?;
            }
            Ok(())
        }
    });
    let inject = meta.inject.iter().fold(Inject::new(), |inject, key| inject.with(key.as_str()));
    let def = def.inject(inject).provides(meta.provide.iter().map(String::as_str));
    match &meta.name {
        Some(name) => def.name(name),
        None => def,
    }
}

/// Disposes the instance when the fiber is disposed.
fn dispose_with(ctx: &Context, slot: &Arc<Slot>) -> Result<()> {
    let (logger, slot) = (ctx.logger(), slot.clone());
    ctx.defer(
        "guest",
        Disposer::future(move || async move {
            let instance = slot.instance.lock().clone();
            if let Some(instance) = instance {
                let slot = slot.clone();
                if let Err(error) =
                    blocking(move || run(&slot, &instance, Export::Dispose, Vec::new(), &CallControl::default())).await
                {
                    logger.log_error(&error);
                }
            }
        }),
    )?;
    Ok(())
}

/// Fails the fiber when the guest's process exits before it is disposed.
#[cfg(feature = "process")]
fn watch_exit(ctx: &Context, handle: GuestHandle) -> Result<()> {
    let watcher = ctx.clone();
    let task = tokio::spawn(async move {
        let exit = handle.exited().await;
        if exit.cause == ExitCause::Disposed {
            return;
        }
        let fiber = watcher.fiber();
        watcher.emit(&GuestExited { fiber: fiber.clone(), exit: exit.clone() });
        drop(fiber.fail(exit.failure().into()));
    });
    ctx.on_dispose(move || task.abort())?;
    Ok(())
}

fn check_meta(slot: &Slot, expected: &GuestMeta) -> Result<()> {
    let Some(instance) = slot.instance.lock().clone() else {
        return Err(disposed());
    };
    let output = run(slot, &instance, Export::Meta, Vec::new(), &CallControl::default())?;
    let meta: ModuleMeta = serde_json::from_slice(&output)
        .map_err(|error| Error::msg(format!("invalid metadata of the guest: {error}")))?;
    match meta.plugin {
        Some(actual) if actual.matches(expected) => Ok(()),
        actual => Err(Error::msg(format!(
            "the guest declares {}, but its plugin is defined as {}",
            json!(actual),
            json!(expected)
        ))),
    }
}

thread_local! {
    static ENTERED: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

/// Calls an export of `instance`, the one in `slot`; `Dispose` also releases
/// it. A guest calling back into an instance that is running on this thread
/// (directly or through other guests) fails instead of deadlocking.
pub(crate) fn run(
    slot: &Slot,
    instance: &Arc<dyn GuestInstance>,
    export: Export,
    input: Vec<u8>,
    control: &CallControl,
) -> Result<Vec<u8>> {
    let id = std::ptr::from_ref(slot) as usize;
    if slot.reentrant() {
        return Err(Error::msg("re-entrant call into a guest instance"));
    }
    ENTERED.with_borrow_mut(|entered| entered.push(id));
    let _entered = Entered;
    if export.is_lifecycle() {
        let mut guard = match control.deadline() {
            // waiting for another lifecycle export to finish: this one fails, the guest is not stopped
            Some(deadline) => slot.instance.try_lock_until(deadline).ok_or_else(timed_out)?,
            None => slot.instance.lock(),
        };
        if control.is_cancelled() {
            return Err(cancelled());
        }
        let result = match guard.as_ref() {
            Some(running) if Arc::ptr_eq(running, instance) => running.call(export, input, control),
            _ => Err(slot.gone()),
        };
        if export == Export::Dispose {
            // a dispose that failed still ends the instance: its transport is broken either way
            *guard = None;
            slot.disposed.store(true, Ordering::Release);
        }
        result
    } else {
        if control.is_cancelled() {
            return Err(cancelled());
        }
        // the instance leaves the slot only through a lifecycle export, which
        // holds the lock; a `call` that outlives its instance fails on its own
        if slot.instance.lock().as_ref().is_none_or(|running| !Arc::ptr_eq(running, instance)) {
            return Err(slot.gone());
        }
        instance.call(export, input, control)
    }
}

/// The span of a host call answered off the calling thread.
pub(crate) struct Answering(Arc<Slot>);

impl Drop for Answering {
    fn drop(&mut self) {
        self.0.answering.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Leaves the instance entered last on this thread when dropped, so a call
/// that fails or panics leaves it too.
struct Entered;

impl Drop for Entered {
    fn drop(&mut self) {
        ENTERED.with_borrow_mut(|entered| entered.pop());
    }
}

/// Runs `f` on the blocking pool; a panic becomes its error.
pub(crate) async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f).await.map_err(|error| {
        let message = format!("a guest call panicked: {error}");
        match error.is_panic() {
            true => GuestFailure::new(GuestFailureKind::Panicked, message).into(),
            false => Error::msg(message),
        }
    })?
}

/// Resolves a dependency relative to the module that declares it.
fn resolve(id: &str, dep: &str) -> String {
    let base = Path::new(id).parent().unwrap_or(Path::new(""));
    normalize(&base.join(dep)).to_string_lossy().into_owned()
}

pub(crate) fn disposed() -> Error {
    Error::msg("the guest instance is disposed")
}

#[cfg(feature = "process")]
pub(crate) fn exited() -> Error {
    GuestFailure::new(GuestFailureKind::Exited, "the guest process exited").into()
}

pub(crate) fn timed_out() -> Error {
    Error::msg("the guest did not answer in time")
}

pub(crate) fn cancelled() -> Error {
    Error::msg("the guest call was cancelled")
}
