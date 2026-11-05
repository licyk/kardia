use std::any::{Any, TypeId};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::Value;

use crate::effect::EffectCell;
use crate::error::{Error, KardiaError};
use crate::events::{ConfigHook, ListenerEntry, UpdateHook};
use crate::fiber::FiberState;
use crate::id_table::IdTable;
use crate::key::{FiberId, ImplId, Key, RealmId};
use crate::logger::Level;
use crate::plugin::{Inject, PluginDef, RuntimeKey};
use crate::root::Action;
use crate::scope::Scope;

pub(crate) struct State {
    pub(crate) fibers: HashMap<FiberId, FiberData>,
    /// Plugins with at least one fiber, in registration order.
    pub(crate) runtimes: IndexMap<RuntimeKey, RuntimeData>,
    /// Replaced plugins, forwarded to their replacement.
    pub(crate) aliases: HashMap<RuntimeKey, PluginDef>,
    /// Every binding that still exists, live or withdrawn-but-referenced.
    pub(crate) impls: HashMap<ImplId, Impl>,
    /// The live binding of each realm (the paper's Σ).
    pub(crate) store: HashMap<RealmId, ImplId>,
    /// Root realm of each key, assigned lazily on first provision.
    pub(crate) root_realms: HashMap<Key, RealmId>,
    pub(crate) listeners: HashMap<TypeId, Vec<Arc<ListenerEntry>>>,
    pub(crate) config_hooks: IdTable<ConfigHook>,
    next_fiber: u64,
    next_impl: u64,
    pub(crate) next_listener: u64,
    pub(crate) actions: Vec<Action>,
}

pub(crate) struct RuntimeData {
    pub(crate) def: PluginDef,
    pub(crate) fibers: Vec<FiberId>,
}

/// What a fiber should run against: ⊥, or its revision plus the binding
/// chosen for every declared key (in declaration order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    Inactive,
    Epoch { revision: u64, providers: Vec<ImplId> },
}

pub(crate) struct FiberData {
    pub(crate) parent: FiberId,
    pub(crate) scope: Arc<Scope>,
    pub(crate) runtime: Option<RuntimeKey>,
    pub(crate) inject: Inject,
    /// The source configuration, as given (not decoded).
    pub(crate) config: Value,
    pub(crate) state: FiberState,
    pub(crate) removed: bool,
    pub(crate) revision: u64,
    /// Live resolution of each declared key.
    pub(crate) resolved: HashMap<Key, ImplId>,
    pub(crate) target: Target,
    /// The target the current episode was loaded for.
    pub(crate) episode: Option<Target>,
    /// The committed view ω, fixed for an episode.
    pub(crate) committed: Option<HashMap<Key, ImplId>>,
    /// A reload or unload transition is in flight.
    pub(crate) inertia: bool,
    pub(crate) error: Option<Error>,
    /// The fiber's own bindings (σ_n), in provision order.
    pub(crate) provisions: Vec<ImplId>,
    /// Set while the accumulator is being disposed during an unload.
    pub(crate) draining: bool,
    /// Provisions withdrawn during a drain. They stay visible to the
    /// fiber's descendants until the drain is over.
    pub(crate) withdrawn: Vec<ImplId>,
    /// The accumulator: effects in insertion order.
    pub(crate) disposables: IdTable<Arc<EffectCell>>,
    pub(crate) update_hooks: IdTable<Arc<UpdateHook>>,
    /// The effect on the parent that instantiated this fiber (O-Insert).
    pub(crate) retire: Option<Arc<EffectCell>>,
}

pub(crate) struct Impl {
    pub(crate) key: Key,
    pub(crate) realm: RealmId,
    pub(crate) provider: FiberId,
    pub(crate) provider_scope: Arc<Scope>,
    pub(crate) value: Arc<dyn Any + Send + Sync>,
    /// Extra availability predicate, evaluated for each consumer under the
    /// runtime lock; it must not call into the runtime.
    pub(crate) check: Option<Check>,
    /// A JSON reading of the value, if it has one.
    pub(crate) view: Option<View>,
    /// Fibers whose committed view names this binding (relied_n).
    pub(crate) dependents: HashSet<FiberId>,
}

/// A JSON reading of a bound value.
pub type View = Arc<dyn Fn() -> Value + Send + Sync>;

/// An availability predicate of a binding, given the consumer's
/// interception metadata for the key (declared `inject` metadata first).
pub type Check = Arc<dyn Fn(&[Value]) -> bool + Send + Sync>;

impl State {
    pub(crate) fn new() -> Self {
        let mut fiber = FiberData::new(FiberId::ROOT, Scope::root(), None, Inject::new(), Value::Null);
        fiber.state = FiberState::Active;
        fiber.target = Target::Epoch { revision: 0, providers: Vec::new() };
        fiber.episode = Some(fiber.target.clone());
        fiber.committed = Some(HashMap::new());
        Self {
            fibers: HashMap::from([(FiberId::ROOT, fiber)]),
            runtimes: IndexMap::new(),
            aliases: HashMap::new(),
            impls: HashMap::new(),
            store: HashMap::new(),
            root_realms: HashMap::new(),
            listeners: HashMap::new(),
            config_hooks: IdTable::new(),
            next_fiber: 0,
            next_impl: 0,
            next_listener: 0,
            actions: Vec::new(),
        }
    }

    pub(crate) fn next_impl_id(&mut self) -> ImplId {
        self.next_impl += 1;
        ImplId(self.next_impl)
    }

    pub(crate) fn next_fiber_id(&mut self) -> FiberId {
        self.next_fiber += 1;
        FiberId(self.next_fiber)
    }

    pub(crate) fn fiber(&self, id: FiberId) -> Option<&FiberData> {
        self.fibers.get(&id)
    }

    pub(crate) fn fiber_mut(&mut self, id: FiberId) -> Option<&mut FiberData> {
        self.fibers.get_mut(&id)
    }

    pub(crate) fn def_of(&self, id: FiberId) -> Option<PluginDef> {
        let runtime = self.fiber(id)?.runtime?;
        Some(self.runtimes.get(&runtime)?.def.clone())
    }

    /// The plugin name of the fiber or its nearest named ancestor.
    pub(crate) fn fiber_name(&self, mut id: FiberId) -> String {
        while let Some(fiber) = self.fiber(id) {
            if let Some(name) = self.runtime_name(fiber) {
                return name;
            }
            if id == FiberId::ROOT {
                break;
            }
            id = fiber.parent;
        }
        "root".into()
    }

    fn runtime_name(&self, fiber: &FiberData) -> Option<String> {
        let runtime = self.runtimes.get(&fiber.runtime?)?;
        runtime.def.plugin_name().map(String::from)
    }

    /// A retired fiber accepts no new effects, except from its own load
    /// still in flight: those are collected and reverted by the unload that
    /// follows, so nothing the load acquired leaks.
    pub(crate) fn assert_active(&self, id: FiberId) -> Result<(), KardiaError> {
        match self.fiber(id) {
            Some(fiber) if !fiber.removed || fiber.state == FiberState::Loading => Ok(()),
            _ => Err(KardiaError::InactiveEffect),
        }
    }

    pub(crate) fn attach_effect(&mut self, id: FiberId, cell: &Arc<EffectCell>) {
        if let Some(fiber) = self.fibers.get_mut(&id) {
            cell.set_entry(Some(fiber.disposables.insert(cell.clone())));
        }
    }

    /// Removes an effect from its fiber's accumulator. The cell is returned
    /// so that the caller drops it after the critical section.
    pub(crate) fn detach_effect(&mut self, id: FiberId, entry: u64) -> Option<Arc<EffectCell>> {
        self.fibers.get_mut(&id)?.disposables.remove(entry)
    }

    pub(crate) fn realm_of(&self, scope: &Arc<Scope>, key: &str) -> Option<RealmId> {
        scope.own_realm(key).or_else(|| self.root_realms.get(key).copied())
    }

    pub(crate) fn log(&mut self, fiber: FiberId, level: Level, error: Error) {
        let name = self.fiber_name(fiber);
        self.actions.push(Action::Log { name, fiber, level, error });
    }

    pub(crate) fn drop_later(&mut self, value: impl Any + Send) {
        self.actions.push(Action::Drop(Box::new(value)));
    }
}

impl FiberData {
    pub(crate) fn new(
        parent: FiberId,
        scope: Arc<Scope>,
        runtime: Option<RuntimeKey>,
        inject: Inject,
        config: Value,
    ) -> Self {
        Self {
            parent,
            scope,
            runtime,
            inject,
            config,
            state: FiberState::Pending,
            removed: false,
            revision: 0,
            resolved: HashMap::new(),
            target: Target::Inactive,
            episode: None,
            committed: None,
            inertia: false,
            error: None,
            provisions: Vec::new(),
            draining: false,
            withdrawn: Vec::new(),
            disposables: IdTable::new(),
            update_hooks: IdTable::new(),
            retire: None,
        }
    }

    /// The state a fiber settles in once no transition is in flight (`_getState`).
    pub(crate) fn settled_state(&self) -> FiberState {
        if self.removed {
            FiberState::Disposed
        } else if self.error.is_some() {
            FiberState::Failed
        } else if self.target != Target::Inactive {
            FiberState::Active
        } else {
            FiberState::Pending
        }
    }
}
