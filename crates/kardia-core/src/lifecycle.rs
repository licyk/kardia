//! The fiber state machine: a port of `_checkImpl` / `_refresh` / `_setEpoch`
//! / `_reload` / `_unload` / `_updateState`, split into critical sections on
//! [`State`] and the asynchronous transitions that run between them.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures::FutureExt;
use futures::future::join_all;
use serde_json::Value;

use crate::config::ErasedConfig;
use crate::error::{Error, Result};
use crate::events::{Emit, ServiceChanged, StatusChanged};
use crate::fiber::{Fiber, FiberState};
use crate::key::{FiberId, Key, RealmId};
use crate::logger::Level;
use crate::plugin::PluginDef;
use crate::root::{Action, RootInner};
use crate::state::{State, Target};

impl State {
    /// Re-resolves one declared key of a fiber against the live store.
    pub(crate) fn check_impl(&mut self, id: FiberId, key: &Key) {
        let Some(fiber) = self.fiber(id) else { return };
        let found = self.live_impl(&fiber.scope, key, true).filter(|impl_id| {
            let Some(check) = &self.impls[impl_id].check else { return true };
            let mut meta: Vec<Value> = fiber.inject.meta(key).cloned().into_iter().collect();
            meta.extend(fiber.scope.intercepts(key));
            check(&meta)
        });
        let fiber = self.fiber_mut(id).expect("checked above");
        match found {
            Some(impl_id) => fiber.resolved.insert(key.clone(), impl_id),
            None => fiber.resolved.remove(key),
        };
    }

    fn compute_target(&self, id: FiberId) -> Target {
        let fiber = &self.fibers[&id];
        let mut providers = Vec::with_capacity(fiber.inject.len());
        for key in fiber.inject.keys() {
            match fiber.resolved.get(key) {
                Some(impl_id) => providers.push(*impl_id),
                None => return Target::Inactive,
            }
        }
        Target::Epoch { revision: fiber.revision, providers }
    }

    pub(crate) fn refresh(&mut self, id: FiberId) {
        if self.fibers.contains_key(&id) {
            let target = self.compute_target(id);
            self.set_target(id, target);
        }
    }

    /// `_setEpoch`: records the new target and starts a transition unless
    /// one is in flight (inertia) or the fiber has failed.
    pub(crate) fn set_target(&mut self, id: FiberId, target: Target) {
        let fiber = self.fibers.get_mut(&id).expect("fiber exists");
        if fiber.target == target || fiber.error.is_some() {
            return;
        }
        let old = std::mem::replace(&mut fiber.target, target);
        if fiber.inertia {
            return;
        }
        if fiber.target != Target::Inactive && old == Target::Inactive {
            self.begin_reload(id);
            self.update_state(id, Some(FiberState::Loading));
        } else {
            self.begin_unload(id);
            self.update_state(id, Some(FiberState::Unloading));
        }
    }

    /// `_updateState`: on entering or leaving `Active`, consumers of the
    /// fiber's live provisions are notified (L-Leave precedes any inverse).
    pub(crate) fn update_state(&mut self, id: FiberId, next: Option<FiberState>) {
        let fiber = self.fibers.get_mut(&id).expect("fiber exists");
        let old = fiber.state;
        fiber.state = next.unwrap_or_else(|| fiber.settled_state());
        let new = fiber.state;
        if old == new {
            return;
        }
        let scope = fiber.scope.clone();
        self.emit(move |root| StatusChanged { fiber: Fiber::from_parts(root, id, scope), old, new });
        if old != FiberState::Active && new != FiberState::Active {
            return;
        }
        let provisions = self.fibers[&id].provisions.clone();
        for impl_id in provisions {
            let binding = &self.impls[&impl_id];
            if self.store.get(&binding.realm) == Some(&impl_id) {
                let (key, realm) = (binding.key.clone(), binding.realm);
                self.notify(&key, realm);
            }
        }
    }

    fn begin_reload(&mut self, id: FiberId) {
        let fiber = self.fibers.get_mut(&id).expect("fiber exists");
        fiber.inertia = true;
        fiber.episode = Some(fiber.target.clone());
        let committed = fiber.resolved.clone();
        for impl_id in committed.values() {
            if let Some(binding) = self.impls.get_mut(impl_id)
                && binding.provider != id
            {
                binding.dependents.insert(id);
            }
        }
        self.fibers.get_mut(&id).expect("fiber exists").committed = Some(committed);
        self.actions.push(Action::Reload(id));
    }

    fn begin_unload(&mut self, id: FiberId) {
        self.fibers.get_mut(&id).expect("fiber exists").inertia = true;
        self.actions.push(Action::Unload(id));
    }

    /// Re-resolves every fiber that declares `key` in `realm`.
    pub(crate) fn notify(&mut self, key: &Key, realm: RealmId) {
        let fibers: Vec<FiberId> = self.runtimes.values().flat_map(|runtime| runtime.fibers.iter().copied()).collect();
        for id in fibers {
            let Some(fiber) = self.fiber(id) else { continue };
            if !fiber.inject.contains(key) || self.realm_of(&fiber.scope, key) != Some(realm) {
                continue;
            }
            self.check_impl(id, key);
            self.refresh(id);
        }
        let key = key.clone();
        self.emit(move |_| ServiceChanged { key, realm });
    }

    /// The load of `id` is still current and should run `apply`.
    fn prepare_execute(&self, id: FiberId) -> Option<(Option<PluginDef>, Value)> {
        let fiber = self.fiber(id)?;
        (fiber.episode.as_ref() == Some(&fiber.target)).then(|| (self.def_of(id), fiber.config.clone()))
    }

    fn finish_reload(&mut self, id: FiberId, outcome: Result<()>) {
        if let Err(error) = outcome
            && !error.is_interrupted()
        {
            self.log(id, Level::Error, error.clone());
            let fiber = self.fibers.get_mut(&id).expect("fiber exists");
            // a superseded load still needs cleanup, but cannot fail the new target
            if fiber.episode.as_ref() == Some(&fiber.target) {
                fiber.error = Some(error);
                fiber.target = Target::Inactive;
            }
        }
        let fiber = self.fibers.get_mut(&id).expect("fiber exists");
        if fiber.episode.as_ref() == Some(&fiber.target) {
            fiber.inertia = false;
            self.update_state(id, None);
        } else {
            self.begin_unload(id);
            self.update_state(id, Some(FiberState::Unloading));
        }
    }

    fn start_drain(&mut self, id: FiberId) -> Vec<Arc<crate::effect::EffectCell>> {
        let fiber = self.fibers.get_mut(&id).expect("fiber exists");
        fiber.draining = true;
        let cells = fiber.disposables.drain_rev();
        for cell in &cells {
            cell.set_entry(None);
        }
        cells
    }

    fn finish_unload(&mut self, id: FiberId) {
        let fiber = self.fibers.get_mut(&id).expect("fiber exists");
        fiber.draining = false;
        fiber.episode = None;
        let withdrawn = std::mem::take(&mut fiber.withdrawn);
        let committed = fiber.committed.take().unwrap_or_default();
        for impl_id in withdrawn {
            self.remove_provision(id, impl_id);
        }
        // release the providers waiting for this fiber to unload
        for impl_id in committed.values() {
            if let Some(binding) = self.impls.get_mut(impl_id) {
                binding.dependents.remove(&id);
            }
        }
        let fiber = self.fibers.get_mut(&id).expect("fiber exists");
        if fiber.target == Target::Inactive {
            fiber.inertia = false;
            self.update_state(id, None);
            self.collect_removed(id);
        } else {
            self.begin_reload(id);
            self.update_state(id, Some(FiberState::Loading));
        }
    }

    /// Settles a removed fiber with no transition in flight to `Disposed`
    /// and drops it from the registry.
    pub(crate) fn collect_removed(&mut self, id: FiberId) {
        let Some(fiber) = self.fiber(id) else { return };
        if !fiber.removed || fiber.inertia {
            return;
        }
        self.update_state(id, None);
        if let Some(fiber) = self.fibers.remove(&id) {
            self.drop_later(fiber);
        }
    }

    pub(crate) fn emit<E: crate::events::Event>(&mut self, event: impl FnOnce(&Arc<RootInner>) -> E + Send + 'static) {
        self.actions.push(Action::Emit(Emit::new(event)));
    }
}

/// `_reload` after its synchronous prefix (`begin_reload`).
pub(crate) async fn reload(root: Arc<RootInner>, id: FiberId) {
    // let superseding updates of the same tick coalesce
    tokio::task::yield_now().await;
    let job = root.with(|st| st.prepare_execute(id));
    let outcome = match job {
        Some((Some(def), config)) => execute(&root, id, def, config).await,
        _ => Ok(()),
    };
    root.run(|st| st.finish_reload(id, outcome));
}

async fn execute(root: &Arc<RootInner>, id: FiberId, def: PluginDef, config: Value) -> Result<()> {
    let fiber = Fiber::of(root, id);
    let config = resolve_config(&fiber, &def, config)?;
    AssertUnwindSafe(def.apply(fiber.ctx.clone(), config))
        .catch_unwind()
        .await
        .unwrap_or_else(|panic| Err(Error::from_panic(panic)))
}

/// Resolves the source configuration of a fiber: the registered config
/// hooks (outermost first), then the plugin's own decoding.
pub(crate) fn resolve_config(fiber: &Fiber, def: &PluginDef, config: Value) -> Result<ErasedConfig> {
    let hooks: Vec<_> = fiber.ctx.root.with(|st| st.config_hooks.values().cloned().collect());
    run_config_hooks(fiber, &hooks, def, config)
}

fn run_config_hooks(
    fiber: &Fiber,
    hooks: &[crate::events::ConfigHook],
    def: &PluginDef,
    config: Value,
) -> Result<ErasedConfig> {
    match hooks.split_first() {
        None => Ok(def.decode(&config)?),
        Some((hook, rest)) => hook(fiber, config, &|config| run_config_hooks(fiber, rest, def, config)),
    }
}

/// `_unload`: waits for the fiber-level guard, then disposes the accumulator
/// newest first, all effects concurrently.
pub(crate) async fn unload(root: Arc<RootInner>, id: FiberId) {
    // Dependents are notified when this fiber leaves `Active`, but may still
    // access its services during their own teardown.
    root.wait_until(|st| {
        st.fiber(id).is_none_or(|fiber| {
            fiber.provisions.iter().all(|p| st.impls.get(p).is_none_or(|binding| binding.dependents.is_empty()))
        })
    })
    .await;
    let cells = root.run(|st| st.start_drain(id));
    let disposals: Vec<_> = cells.iter().map(|cell| cell.dispose(Some(&root))).collect();
    drop(cells);
    for result in join_all(disposals).await {
        if let Err(error) = result {
            root.run(|st| st.log(id, Level::Error, error));
        }
    }
    root.run(|st| st.finish_unload(id));
}
