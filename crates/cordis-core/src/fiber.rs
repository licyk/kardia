use std::fmt;
use std::sync::Arc;

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::Value;

use crate::context::Context;
use crate::effect::{Disposal, EffectHandle, EffectMeta};
use crate::error::{Error, Interrupted, Result};
use crate::events::ConfigCommitted;
use crate::key::FiberId;
use crate::lifecycle::resolve_config;
use crate::plugin::{Inject, PluginDef, RuntimeKey};
use crate::root::RootInner;
use crate::scope::Scope;
use crate::state::{FiberData, Target};

/// The paper's θ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FiberState {
    /// Waiting for its dependencies.
    Pending,
    Loading,
    Active,
    Unloading,
    /// Its last load failed; only `update` / `restart` / `replace` recover it.
    Failed,
    Disposed,
}

/// Resolves when a fiber has no transition in flight, with its failure if any.
pub type Settle = BoxFuture<'static, Result<()>>;

/// A handle to a fiber (one instantiation of a plugin).
#[derive(Clone)]
pub struct Fiber {
    pub(crate) ctx: Context,
}

impl Fiber {
    pub(crate) fn from_parts(root: &Arc<RootInner>, id: FiberId, scope: Arc<Scope>) -> Self {
        Self { ctx: Context { root: root.clone(), fiber: id, scope } }
    }

    pub(crate) fn of(root: &Arc<RootInner>, id: FiberId) -> Self {
        let scope = root.with(|st| st.fiber(id).map(|fiber| fiber.scope.clone()));
        Self::from_parts(root, id, scope.unwrap_or_else(Scope::root))
    }

    pub fn id(&self) -> FiberId {
        self.ctx.fiber
    }

    /// The fiber's own context.
    pub fn context(&self) -> &Context {
        &self.ctx
    }

    pub fn state(&self) -> FiberState {
        self.read(|fiber| fiber.state).unwrap_or(FiberState::Disposed)
    }

    /// Whether the fiber has been retired (its instantiation reverted).
    pub fn is_removed(&self) -> bool {
        self.read(|fiber| fiber.removed).unwrap_or(true)
    }

    pub fn is_root(&self) -> bool {
        self.ctx.fiber == FiberId::ROOT
    }

    /// The source configuration, as last given.
    pub fn config(&self) -> Value {
        self.read(|fiber| fiber.config.clone()).unwrap_or_default()
    }

    pub fn inject(&self) -> Inject {
        self.read(|fiber| fiber.inject.clone()).unwrap_or_default()
    }

    pub fn error(&self) -> Option<Error> {
        self.read(|fiber| fiber.error.clone()).flatten()
    }

    pub fn runtime(&self) -> Option<RuntimeKey> {
        self.read(|fiber| fiber.runtime).flatten()
    }

    /// The plugin definition currently behind the fiber.
    pub fn def(&self) -> Option<PluginDef> {
        self.ctx.root.with(|st| st.def_of(self.ctx.fiber))
    }

    /// The plugin name of the fiber or its nearest named ancestor.
    pub fn name(&self) -> String {
        self.ctx.root.with(|st| st.fiber_name(self.ctx.fiber))
    }

    /// The fiber this one was instantiated from (the root is its own parent).
    pub fn parent(&self) -> Option<Fiber> {
        let parent = self.read(|fiber| fiber.parent)?;
        Some(Fiber::of(&self.ctx.root, parent))
    }

    /// The effect on the parent that instantiated this fiber.
    pub fn effect_handle(&self) -> Option<EffectHandle> {
        let cell = self.read(|fiber| fiber.retire.clone()).flatten()?;
        Some(EffectHandle::new(&self.ctx.root, cell))
    }

    /// The effects currently registered on the fiber, in insertion order.
    pub fn effects(&self) -> Vec<EffectMeta> {
        let cells = self.read(|fiber| fiber.disposables.values().cloned().collect::<Vec<_>>()).unwrap_or_default();
        cells.iter().map(|cell| cell.meta()).collect()
    }

    /// Number of effects in the fiber's accumulator.
    pub fn effect_count(&self) -> usize {
        self.read(|fiber| fiber.disposables.len()).unwrap_or(0)
    }

    /// Waits until no transition is in flight; fails with the fiber's error.
    pub fn settled(&self) -> Settle {
        let root = self.ctx.root.clone();
        let id = self.ctx.fiber;
        async move {
            root.wait_until(|st| st.fiber(id).is_none_or(|fiber| !fiber.inertia)).await;
            match root.with(|st| st.fiber(id).and_then(|fiber| fiber.error.clone())) {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }
        .boxed()
    }

    /// Retires the fiber. For the root fiber this restarts it, reverting
    /// everything registered on it.
    pub fn dispose(&self) -> Disposal {
        if self.is_root() {
            let restart = self.restart();
            return Disposal::spawn(async move {
                let _ = restart.await;
                Ok(())
            });
        }
        match self.effect_handle() {
            Some(handle) => handle.dispose(),
            None => Disposal::ready(Ok(())),
        }
    }

    /// Bumps the revision, so the fiber unloads and loads again.
    pub fn restart(&self) -> Settle {
        let id = self.ctx.fiber;
        let started = self.ctx.root.run(|st| {
            st.assert_active(id)?;
            st.fiber_mut(id).expect("active fiber exists").revision += 1;
            st.refresh(id);
            Ok::<_, Error>(())
        });
        match started {
            Ok(()) => self.settled(),
            Err(error) => futures::future::ready(Err(error)).boxed(),
        }
    }

    /// Replaces the configuration. If the fiber is loaded, the new config is
    /// validated first (a rejected config leaves the running instance
    /// untouched) and offered to the fiber's update hooks; if none takes it
    /// over, the fiber restarts.
    pub fn update(&self, config: Value) -> Result<Settle> {
        self.update_with(config, false)
    }

    /// Like [`Fiber::update`]; `no_save` suppresses [`ConfigCommitted`].
    pub fn update_with(&self, config: Value, no_save: bool) -> Result<Settle> {
        let id = self.ctx.fiber;
        let (loaded, def, hooks) = self.ctx.root.with(|st| {
            st.assert_active(id)?;
            let fiber = st.fiber(id).expect("active fiber exists");
            let hooks: Vec<_> = fiber.update_hooks.values().cloned().collect();
            Ok::<_, Error>((fiber.target != Target::Inactive, st.def_of(id), hooks))
        })?;
        let resolved = match (&def, loaded) {
            (Some(def), true) => Some(resolve_config(self, def, config.clone())?),
            _ => None,
        };
        if !no_save {
            self.ctx.root().emit(&ConfigCommitted { fiber: self.clone(), config: config.clone() });
        }
        self.ctx.root.run(|st| {
            if let Some(fiber) = st.fiber_mut(id) {
                fiber.config = config;
                fiber.error = None;
            }
        });
        if let Some(resolved) = resolved
            && hooks.iter().any(|hook| hook.accepts(&resolved))
        {
            return Ok(futures::future::ready(Ok(())).boxed());
        }
        Ok(self.restart())
    }

    fn read<T>(&self, f: impl FnOnce(&FiberData) -> T) -> Option<T> {
        self.ctx.root.with(|st| st.fiber(self.ctx.fiber).map(f))
    }
}

impl PartialEq for Fiber {
    fn eq(&self, other: &Self) -> bool {
        self.ctx.same_root(&other.ctx) && self.ctx.fiber == other.ctx.fiber
    }
}

impl Eq for Fiber {}

impl fmt::Debug for Fiber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fiber({:?}, {:?})", self.ctx.fiber, self.state())
    }
}

impl From<&Fiber> for EffectHandle {
    fn from(fiber: &Fiber) -> Self {
        fiber.effect_handle().expect("only a live, non-root fiber has an instantiation effect")
    }
}

impl From<Fiber> for EffectHandle {
    fn from(fiber: Fiber) -> Self {
        (&fiber).into()
    }
}

impl Context {
    /// The fiber this context belongs to.
    pub fn fiber(&self) -> Fiber {
        Fiber::of(&self.root, self.fiber)
    }

    /// An iteration boundary of the fiber's own load (L-Divert): fails with
    /// [`Interrupted`] once the load has been superseded or the fiber retired.
    /// Returning that error from `apply` is not a failure.
    pub fn checkpoint(&self) -> Result<(), Interrupted> {
        self.root.with(|st| match st.fiber(self.fiber) {
            Some(fiber) if !fiber.removed && fiber.episode.as_ref().is_none_or(|episode| *episode == fiber.target) => {
                Ok(())
            }
            _ => Err(Interrupted),
        })
    }
}
