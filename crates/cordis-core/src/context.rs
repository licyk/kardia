use std::fmt;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::key::{FiberId, Key, RealmId};
use crate::root::RootInner;
use crate::scope::{Overlay, Scope, ScopeKind};

/// A cheap, clonable handle to a position in the runtime: a root, the fiber
/// that owns effects created through it, and its derivation scope
/// (isolation and interception overlays).
#[derive(Clone)]
pub struct Context {
    pub(crate) root: Arc<RootInner>,
    pub(crate) fiber: FiberId,
    pub(crate) scope: Arc<Scope>,
}

impl Context {
    /// Creates a new root context. Its fiber is the always-active root fiber.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let root = RootInner::new();
        let scope = root.with(|st| st.fibers[&FiberId::ROOT].scope.clone());
        Self { root, fiber: FiberId::ROOT, scope }
    }

    pub(crate) fn of_root(root: &Arc<RootInner>) -> Self {
        let scope = root.with(|st| st.fibers[&FiberId::ROOT].scope.clone());
        Self { root: root.clone(), fiber: FiberId::ROOT, scope }
    }

    pub fn fiber_id(&self) -> FiberId {
        self.fiber
    }

    pub fn is_root(&self) -> bool {
        self.fiber == FiberId::ROOT
    }

    /// Whether both contexts belong to the same root.
    pub fn same_root(&self, other: &Context) -> bool {
        Arc::ptr_eq(&self.root, &other.root)
    }

    /// Whether both handles denote the very same derived context.
    pub fn ptr_eq(&self, other: &Context) -> bool {
        Arc::ptr_eq(&self.scope, &other.scope)
    }

    /// The root context.
    pub fn root(&self) -> Context {
        Self::of_root(&self.root)
    }

    /// Derives a context in which `key` lives in a fresh, private realm.
    pub fn isolate(&self, key: impl Into<Key>) -> Context {
        self.isolate_in(key, RealmId::new())
    }

    /// Derives a context in which `key` lives in the given (shared) realm.
    pub fn isolate_in(&self, key: impl Into<Key>, realm: RealmId) -> Context {
        self.derive(ScopeKind::Isolate(key.into(), realm))
    }

    /// Derives a context carrying interception metadata for `key`.
    pub fn intercept(&self, key: impl Into<Key>, meta: Value) -> Context {
        self.derive(ScopeKind::Intercept(key.into(), meta))
    }

    /// Derives a context with its own mutable isolation and interception
    /// tables (see [`Overlay`]).
    pub fn extend(&self) -> Context {
        self.extend_with(&Overlay::new())
    }

    /// Derives a context backed by a given overlay; several contexts may
    /// share one overlay.
    pub fn extend_with(&self, overlay: &Arc<Overlay>) -> Context {
        self.derive(ScopeKind::Overlay(overlay.clone()))
    }

    /// The overlay of this very context, if it was made by `extend`.
    pub fn overlay(&self) -> Option<Arc<Overlay>> {
        match &self.scope.kind {
            ScopeKind::Overlay(overlay) => Some(overlay.clone()),
            _ => None,
        }
    }

    /// Re-attaches this context, with everything derived below it, under
    /// `parent`. Like a re-pointed prototype, the returned handle belongs to
    /// `parent`'s fiber.
    pub fn reparent(&self, parent: &Context) -> Context {
        self.scope.set_parent(parent.scope.clone());
        Self { root: self.root.clone(), fiber: parent.fiber, scope: self.scope.clone() }
    }

    /// The overlay this context is directly derived from, if any.
    pub fn parent_overlay(&self) -> Option<Arc<Overlay>> {
        match &self.scope.parent()?.kind {
            ScopeKind::Overlay(overlay) => Some(overlay.clone()),
            _ => None,
        }
    }

    /// The tag of the nearest tagged overlay on the derivation chain.
    pub fn tag(&self) -> Option<u64> {
        self.scope.tag()
    }

    /// Whether this context is derived from `ancestor` (or is it).
    pub fn is_within(&self, ancestor: &Context) -> bool {
        self.scope.is_within(&ancestor.scope)
    }

    /// Every key isolated somewhere on this context's derivation chain.
    pub fn isolated_keys(&self) -> Vec<Key> {
        self.scope.isolated_keys()
    }

    /// The realm `key` resolves to from this context, if it has one yet.
    pub fn realm(&self, key: &str) -> Option<RealmId> {
        self.root.with(|st| st.realm_of(&self.scope, key))
    }

    /// Whether this context isolates `key` itself rather than inheriting
    /// the root realm.
    pub fn has_isolation(&self, key: &str) -> bool {
        self.scope.own_realm(key).is_some()
    }

    /// Interception metadata for `key` from the outermost context to this one.
    pub fn intercepts(&self, key: &str) -> Vec<Value> {
        self.scope.intercepts(key)
    }

    /// Interception metadata for `key`, merged right-biased and deserialized.
    /// Malformed metadata yields the default.
    pub fn intercept_meta<T: DeserializeOwned + Default>(&self, key: &str) -> T {
        serde_json::from_value(merge_shallow(self.intercepts(key))).unwrap_or_default()
    }

    fn derive(&self, kind: ScopeKind) -> Context {
        Self { root: self.root.clone(), fiber: self.fiber, scope: Scope::child(&self.scope, kind) }
    }
}

/// Merges JSON objects key by key, later values winning (like `Object.assign`).
pub fn merge_shallow(values: impl IntoIterator<Item = Value>) -> Value {
    let mut result = serde_json::Map::new();
    for value in values {
        if let Value::Object(map) = value {
            result.extend(map);
        }
    }
    Value::Object(result)
}

impl fmt::Debug for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = self.root.with(|st| st.fiber_name(self.fiber));
        write!(f, "Context <{name}>")
    }
}
