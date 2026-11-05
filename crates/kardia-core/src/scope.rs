use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;
use serde_json::Value;

use crate::key::{FiberId, Key, RealmId};

/// One node of a context's derivation chain.
///
/// This replaces the JavaScript prototype chains of Cordis: every derived
/// context (a fiber's own context, `isolate`, `intercept`, `extend`) is a
/// node pointing at the context it was derived from, so "own" vs.
/// "inherited" is explicit. Parents can be re-pointed, which moves
/// everything below a node at once (the loader moves entries this way).
pub(crate) struct Scope {
    parent: RwLock<Option<Arc<Scope>>>,
    pub(crate) kind: ScopeKind,
}

pub(crate) enum ScopeKind {
    /// The context of a fiber: everything below it belongs to that fiber.
    Fiber(FiberId),
    Isolate(Key, RealmId),
    Intercept(Key, Value),
    Overlay(Arc<Overlay>),
}

/// Mutable isolation and interception tables of a derived context.
///
/// Changing them affects every context derived below; the runtime is not
/// notified automatically (see [`crate::Context::refresh_consumers`]).
#[derive(Default)]
pub struct Overlay {
    tag: Option<u64>,
    isolate: RwLock<HashMap<Key, RealmId>>,
    intercept: RwLock<HashMap<Key, Value>>,
}

impl Overlay {
    pub fn new() -> Arc<Self> {
        Arc::default()
    }

    /// An overlay carrying an opaque tag, found again with [`crate::Context::tag`].
    pub fn tagged(tag: u64) -> Arc<Self> {
        Arc::new(Self { tag: Some(tag), ..Default::default() })
    }

    pub fn isolations(&self) -> HashMap<Key, RealmId> {
        self.isolate.read().clone()
    }

    pub fn set_isolations(&self, table: HashMap<Key, RealmId>) {
        *self.isolate.write() = table;
    }

    pub fn set_intercepts(&self, table: HashMap<Key, Value>) {
        *self.intercept.write() = table;
    }
}

impl Scope {
    pub(crate) fn root() -> Arc<Self> {
        Arc::new(Self { parent: RwLock::new(None), kind: ScopeKind::Fiber(FiberId::ROOT) })
    }

    pub(crate) fn child(parent: &Arc<Scope>, kind: ScopeKind) -> Arc<Self> {
        Arc::new(Self { parent: RwLock::new(Some(parent.clone())), kind })
    }

    pub(crate) fn parent(&self) -> Option<Arc<Scope>> {
        self.parent.read().clone()
    }

    pub(crate) fn set_parent(&self, parent: Arc<Scope>) {
        *self.parent.write() = Some(parent);
    }

    /// This node's own realm override for `key`.
    pub(crate) fn isolates(&self, key: &str) -> Option<RealmId> {
        match &self.kind {
            ScopeKind::Isolate(name, realm) if &**name == key => Some(*realm),
            ScopeKind::Overlay(overlay) => overlay.isolate.read().get(key).copied(),
            _ => None,
        }
    }

    /// The nearest realm override for `key`, if any context on the chain isolates it.
    pub(crate) fn own_realm(self: &Arc<Self>, key: &str) -> Option<RealmId> {
        self.ancestors().find_map(|scope| scope.isolates(key))
    }

    /// Interception metadata for `key`, ordered from the outermost context to this one.
    pub(crate) fn intercepts(self: &Arc<Self>, key: &str) -> Vec<Value> {
        let mut values: Vec<Value> = self
            .ancestors()
            .filter_map(|scope| match &scope.kind {
                ScopeKind::Intercept(name, value) if &**name == key => Some(value.clone()),
                ScopeKind::Overlay(overlay) => overlay.intercept.read().get(key).cloned(),
                _ => None,
            })
            .collect();
        values.reverse();
        values
    }

    /// The nearest fiber node on the chain, starting with this one.
    pub(crate) fn fiber(self: &Arc<Self>) -> Option<FiberId> {
        self.ancestors().find_map(|scope| match scope.kind {
            ScopeKind::Fiber(id) => Some(id),
            _ => None,
        })
    }

    pub(crate) fn tag(self: &Arc<Self>) -> Option<u64> {
        self.ancestors().find_map(|scope| match &scope.kind {
            ScopeKind::Overlay(overlay) => overlay.tag,
            _ => None,
        })
    }

    pub(crate) fn is_within(self: &Arc<Self>, ancestor: &Arc<Scope>) -> bool {
        self.ancestors().any(|scope| Arc::ptr_eq(&scope, ancestor))
    }

    /// Every key isolated somewhere on the chain.
    pub(crate) fn isolated_keys(self: &Arc<Self>) -> Vec<Key> {
        let mut keys = Vec::new();
        for scope in self.ancestors() {
            match &scope.kind {
                ScopeKind::Isolate(name, _) => keys.push(name.clone()),
                ScopeKind::Overlay(overlay) => keys.extend(overlay.isolate.read().keys().cloned()),
                _ => {}
            }
        }
        keys
    }

    /// This node and its ancestors, nearest first.
    pub(crate) fn ancestors(self: &Arc<Self>) -> impl Iterator<Item = Arc<Scope>> + use<> {
        std::iter::successors(Some(self.clone()), |scope| scope.parent())
    }
}
