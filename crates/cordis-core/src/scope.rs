use std::sync::Arc;

use parking_lot::RwLock;
use serde_json::Value;

use crate::key::{FiberId, Key, RealmId};

/// One node of a context's derivation chain.
///
/// This replaces the JavaScript prototype chains of Cordis: every derived
/// context (a fiber's own context, `isolate`, `intercept`) is a node pointing
/// at the context it was derived from, so "own" vs. "inherited" is explicit.
pub(crate) struct Scope {
    parent: RwLock<Option<Arc<Scope>>>,
    pub(crate) kind: ScopeKind,
}

pub(crate) enum ScopeKind {
    /// The context of a fiber: everything below it belongs to that fiber.
    Fiber(FiberId),
    Isolate(Key, RealmId),
    Intercept(Key, Value),
    /// A transparent node, used by the loader as a re-pointable attachment point.
    Plain,
}

impl Scope {
    pub(crate) fn root() -> Arc<Self> {
        Arc::new(Self {
            parent: RwLock::new(None),
            kind: ScopeKind::Fiber(FiberId::ROOT),
        })
    }

    pub(crate) fn child(parent: &Arc<Scope>, kind: ScopeKind) -> Arc<Self> {
        Arc::new(Self {
            parent: RwLock::new(Some(parent.clone())),
            kind,
        })
    }

    pub(crate) fn parent(&self) -> Option<Arc<Scope>> {
        self.parent.read().clone()
    }

    pub(crate) fn set_parent(&self, parent: Arc<Scope>) {
        *self.parent.write() = Some(parent);
    }

    /// The nearest realm override for `key`, if any context on the chain isolates it.
    pub(crate) fn own_realm(self: &Arc<Self>, key: &str) -> Option<RealmId> {
        let mut node = Some(self.clone());
        while let Some(scope) = node {
            if let ScopeKind::Isolate(name, realm) = &scope.kind
                && &**name == key
            {
                return Some(*realm);
            }
            node = scope.parent();
        }
        None
    }

    /// Interception metadata for `key`, ordered from the outermost context to this one.
    pub(crate) fn intercepts(self: &Arc<Self>, key: &str) -> Vec<Value> {
        let mut values = Vec::new();
        let mut node = Some(self.clone());
        while let Some(scope) = node {
            if let ScopeKind::Intercept(name, value) = &scope.kind
                && &**name == key
            {
                values.push(value.clone());
            }
            node = scope.parent();
        }
        values.reverse();
        values
    }
}
