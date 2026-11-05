use std::collections::HashSet;
use std::future::Future;
use std::sync::{Arc, Weak};

use serde::Serialize;
use serde_json::Value;

use crate::context::Context;
use crate::effect::{Disposer, EffectCell};
use crate::error::{Error, KardiaError, Result};
use crate::events::{PluginInserted, PluginRemoved, Replaced};
use crate::fiber::Fiber;
use crate::key::FiberId;
use crate::logger::Level;
use crate::plugin::{Inject, Plugin, PluginDef, RuntimeKey};
use crate::root::RootInner;
use crate::scope::{Scope, ScopeKind};
use crate::state::{FiberData, RuntimeData, State, Target};

impl Context {
    /// Instantiates a typed plugin as a child fiber of the current fiber.
    pub fn plugin<P: Plugin>(&self, config: P::Config) -> Result<Fiber>
    where
        P::Config: Serialize,
    {
        self.plugin_def(&PluginDef::of::<P>(), serde_json::to_value(config)?)
    }

    /// Instantiates a typed plugin from a raw configuration value.
    pub fn plugin_value<P: Plugin>(&self, config: Value) -> Result<Fiber> {
        self.plugin_def(&PluginDef::of::<P>(), config)
    }

    /// Instantiates a plugin definition (O-Insert). The instantiation is an
    /// effect of the current fiber, whose inverse retires the new fiber.
    pub fn plugin_def(&self, def: &PluginDef, config: Value) -> Result<Fiber> {
        Ok(self.root.run(|st| st.insert_fiber(&self.root, self, def, config))?)
    }

    /// Instantiates an anonymous closure plugin with the given dependencies.
    pub fn inject<F, Fut>(&self, inject: impl Into<Inject>, apply: F) -> Result<Fiber>
    where
        F: Fn(Context) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let def = PluginDef::new(move |ctx, _| apply(ctx)).inject(inject);
        self.plugin_def(&def, Value::Null)
    }

    pub fn registry(&self) -> Registry {
        Registry { ctx: self.clone() }
    }
}

/// Registry operations of a root.
pub struct Registry {
    ctx: Context,
}

impl Registry {
    /// Number of plugins with at least one fiber.
    pub fn size(&self) -> usize {
        self.ctx.root.with(|st| st.runtimes.len())
    }

    /// Keys of the registered plugins, in registration order.
    pub fn keys(&self) -> Vec<RuntimeKey> {
        self.ctx.root.with(|st| st.runtimes.keys().copied().collect())
    }

    /// The registry key a plugin resolves to, following replacements.
    pub fn get(&self, def: &PluginDef) -> Option<RuntimeKey> {
        self.ctx.root.with(|st| {
            let (key, _) = st.forward(def);
            st.runtimes.contains_key(&key).then_some(key)
        })
    }

    pub fn has(&self, def: &PluginDef) -> bool {
        self.get(def).is_some()
    }

    /// The fibers of a plugin, in instantiation order.
    pub fn fibers(&self, def: &PluginDef) -> Vec<Fiber> {
        let ids = self.ctx.root.with(|st| {
            let (key, _) = st.forward(def);
            st.runtimes.get(&key).map(|runtime| runtime.fibers.clone()).unwrap_or_default()
        });
        ids.into_iter().map(|id| Fiber::of(&self.ctx.root, id)).collect()
    }

    /// Swaps the implementation behind every fiber of `old` in place, and
    /// forwards future instantiations of `old` to `new`. Returns the moved
    /// fibers; each reloads with the new implementation.
    pub fn replace(&self, old: &PluginDef, new: &PluginDef) -> Vec<Fiber> {
        let ids = self.ctx.root.run(|st| st.replace(old, new));
        ids.into_iter().map(|id| Fiber::of(&self.ctx.root, id)).collect()
    }
}

impl State {
    pub(crate) fn forward(&self, def: &PluginDef) -> (RuntimeKey, PluginDef) {
        let mut def = def.clone();
        while let Some(next) = self.aliases.get(&def.key()) {
            def = next.clone();
        }
        (def.key(), def)
    }

    pub(crate) fn insert_fiber(
        &mut self,
        root: &Arc<RootInner>,
        parent: &Context,
        def: &PluginDef,
        config: Value,
    ) -> Result<Fiber, KardiaError> {
        self.assert_active(parent.fiber)?;
        let (key, def) = self.forward(def);
        self.runtimes.entry(key).or_insert_with(|| RuntimeData { def: def.clone(), fibers: Vec::new() });
        let id = self.next_fiber_id();
        let scope = Scope::child(&parent.scope, ScopeKind::Fiber(id));
        let inject = def.inject_spec().clone();
        let keys: Vec<_> = inject.keys().cloned().collect();
        self.fibers.insert(id, FiberData::new(parent.fiber, scope.clone(), Some(key), inject, config));
        let event_scope = scope.clone();
        self.emit(move |root| PluginInserted { fiber: Fiber::from_parts(root, id, event_scope) });
        for name in &keys {
            self.check_impl(id, name);
        }
        self.runtimes[&key].fibers.push(id);
        self.refresh(id);
        let cell = EffectCell::new("ctx.plugin()", parent.fiber);
        cell.push_disposer(retire(Arc::downgrade(root), id));
        self.attach_effect(parent.fiber, &cell);
        self.fibers.get_mut(&id).expect("just inserted").retire = Some(cell);
        self.warn_cycle(id);
        Ok(Fiber::from_parts(root, id, scope))
    }

    /// The synchronous part of O-Retire.
    fn retire_fiber(&mut self, id: FiberId) {
        let Some(fiber) = self.fibers.get_mut(&id) else { return };
        if fiber.removed {
            return;
        }
        fiber.removed = true;
        let (scope, runtime) = (fiber.scope.clone(), fiber.runtime);
        self.emit(move |root| PluginRemoved { fiber: Fiber::from_parts(root, id, scope) });
        if let Some(key) = runtime
            && let Some(runtime) = self.runtimes.get_mut(&key)
        {
            runtime.fibers.retain(|fiber| *fiber != id);
            if runtime.fibers.is_empty()
                && let Some(runtime) = self.runtimes.shift_remove(&key)
            {
                self.drop_later(runtime.def);
            }
        }
        // A retired provider's bindings stop occupying their realms at once,
        // so a successor can provide there while old consumers still drain
        // (they read through their committed views).
        let provisions = self.fibers[&id].provisions.clone();
        for impl_id in provisions {
            self.unbind(impl_id);
        }
        self.set_target(id, Target::Inactive);
        self.collect_removed(id);
    }

    fn replace(&mut self, old: &PluginDef, new: &PluginDef) -> Vec<FiberId> {
        let (old_key, _) = self.forward(old);
        let key = new.key();
        // `new` is an implementation of its own again
        self.aliases.remove(&key);
        let fibers = match self.runtimes.get(&old_key) {
            Some(runtime) => runtime.fibers.clone(),
            None => Vec::new(),
        };
        if old_key != key {
            self.runtimes.shift_remove(&old_key);
            self.aliases.insert(old_key, new.clone());
        }
        if fibers.is_empty() {
            return fibers;
        }
        self.runtimes.entry(key).or_insert_with(|| RuntimeData { def: new.clone(), fibers: Vec::new() });
        for id in &fibers {
            self.replace_fiber(*id, key, new.inject_spec().clone());
        }
        fibers
    }

    fn replace_fiber(&mut self, id: FiberId, key: RuntimeKey, inject: Inject) {
        let fiber = self.fibers.get_mut(&id).expect("registered fiber exists");
        if fiber.runtime != Some(key) {
            fiber.runtime = Some(key);
            self.runtimes[&key].fibers.push(id);
        }
        let fiber = self.fibers.get_mut(&id).expect("registered fiber exists");
        let keys: Vec<_> = inject.keys().cloned().collect();
        fiber.inject = inject;
        fiber.resolved.clear();
        let scope = fiber.scope.clone();
        self.emit(move |root| Replaced { fiber: Fiber::from_parts(root, id, scope) });
        for name in &keys {
            self.check_impl(id, name);
        }
        let fiber = self.fibers.get_mut(&id).expect("registered fiber exists");
        fiber.error = None;
        fiber.revision += 1;
        self.refresh(id);
    }

    /// Warns when the declared provisions and dependencies of live fibers
    /// form a cycle through the new fiber (paper §6.5); such a fiber stays pending.
    fn warn_cycle(&mut self, start: FiberId) {
        let mut path = vec![start];
        let mut visited = HashSet::new();
        if self.find_cycle(start, start, &mut path, &mut visited) {
            let names: Vec<String> = path.iter().map(|id| self.fiber_name(*id)).collect();
            let error = Error::msg(format!("dependency cycle: {}", names.join(" -> ")));
            self.log(start, Level::Warn, error);
        }
    }

    /// Depth-first search along "depends on a declared provider" edges.
    fn find_cycle(&self, start: FiberId, at: FiberId, path: &mut Vec<FiberId>, visited: &mut HashSet<FiberId>) -> bool {
        let consumer = &self.fibers[&at];
        for key in consumer.inject.keys() {
            let realm = self.realm_of(&consumer.scope, key);
            for runtime in self.runtimes.values() {
                if !runtime.def.provide_spec().contains(key) {
                    continue;
                }
                for provider in &runtime.fibers {
                    let Some(data) = self.fiber(*provider) else { continue };
                    if data.removed || self.realm_of(&data.scope, key) != realm {
                        continue;
                    }
                    if *provider == start {
                        path.push(start);
                        return true;
                    }
                    if visited.insert(*provider) {
                        path.push(*provider);
                        if self.find_cycle(start, *provider, path, visited) {
                            return true;
                        }
                        path.pop();
                    }
                }
            }
        }
        false
    }
}

/// The inverse of an instantiation: retire the fiber, then wait for its
/// transitions to settle.
fn retire(root: Weak<RootInner>, id: FiberId) -> Disposer {
    Disposer::future(move || {
        let root = root.upgrade();
        if let Some(root) = &root {
            root.run(|st| st.retire_fiber(id));
        }
        async move {
            let Some(root) = root else { return };
            root.wait_until(|st| st.fiber(id).is_none_or(|fiber| !fiber.inertia)).await;
            root.run(|st| st.collect_removed(id));
        }
    })
}
