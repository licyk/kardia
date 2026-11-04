use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::effect::EffectCell;
use crate::error::{CordisError, Error};
use crate::fiber::FiberState;
use crate::id_table::IdTable;
use crate::key::{FiberId, ImplId, Key, RealmId};
use crate::logger::Level;
use crate::root::Action;
use crate::scope::Scope;

pub(crate) struct State {
    pub(crate) fibers: HashMap<FiberId, FiberData>,
    /// Every binding that still exists, live or withdrawn-but-referenced.
    pub(crate) impls: HashMap<ImplId, Impl>,
    /// The live binding of each realm (the paper's Σ).
    pub(crate) store: HashMap<RealmId, ImplId>,
    /// Root realm of each key, assigned lazily on first provision.
    pub(crate) root_realms: HashMap<Key, RealmId>,
    next_fiber: u64,
    next_impl: u64,
    pub(crate) actions: Vec<Action>,
}

pub(crate) struct FiberData {
    pub(crate) id: FiberId,
    pub(crate) parent: FiberId,
    pub(crate) scope: Arc<Scope>,
    pub(crate) state: FiberState,
    pub(crate) removed: bool,
    /// The fiber's own bindings (σ_n), in provision order.
    pub(crate) provisions: Vec<ImplId>,
    /// Set while the accumulator is being disposed during an unload.
    pub(crate) draining: bool,
    /// Provisions withdrawn during a drain. They stay visible to the
    /// fiber's descendants until the drain is over.
    pub(crate) withdrawn: Vec<ImplId>,
    /// The accumulator: effects in insertion order.
    pub(crate) disposables: IdTable<Arc<EffectCell>>,
}

pub(crate) struct Impl {
    pub(crate) id: ImplId,
    pub(crate) key: Key,
    pub(crate) realm: RealmId,
    pub(crate) provider: FiberId,
    pub(crate) provider_scope: Arc<Scope>,
    pub(crate) value: Arc<dyn Any + Send + Sync>,
    /// Fibers whose committed view names this binding (relied_n).
    pub(crate) dependents: HashSet<FiberId>,
}

impl State {
    pub(crate) fn new() -> Self {
        let mut fibers = HashMap::new();
        fibers.insert(
            FiberId::ROOT,
            FiberData::new(FiberId::ROOT, FiberId::ROOT, Scope::root(), FiberState::Active),
        );
        Self {
            fibers,
            impls: HashMap::new(),
            store: HashMap::new(),
            root_realms: HashMap::new(),
            next_fiber: 0,
            next_impl: 0,
            actions: Vec::new(),
        }
    }

    pub(crate) fn next_impl_id(&mut self) -> ImplId {
        self.next_impl += 1;
        ImplId(self.next_impl)
    }

    #[allow(dead_code)]
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

    pub(crate) fn fiber_name(&self, _id: FiberId) -> String {
        "root".into()
    }

    pub(crate) fn assert_active(&self, id: FiberId) -> Result<(), CordisError> {
        match self.fiber(id) {
            Some(fiber) if !fiber.removed => Ok(()),
            _ => Err(CordisError::InactiveEffect),
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
        self.actions.push(Action::Log {
            name,
            fiber,
            level,
            error,
        });
    }

    pub(crate) fn drop_later(&mut self, value: impl Any + Send) {
        self.actions.push(Action::Drop(Box::new(value)));
    }
}

impl FiberData {
    pub(crate) fn new(id: FiberId, parent: FiberId, scope: Arc<Scope>, state: FiberState) -> Self {
        Self {
            id,
            parent,
            scope,
            state,
            removed: false,
            provisions: Vec::new(),
            draining: false,
            withdrawn: Vec::new(),
            disposables: IdTable::new(),
        }
    }
}
