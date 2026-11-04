//! Checked access to coeffects (Algorithm 6).

use std::any::Any;
use std::sync::Arc;

use crate::context::Context;
use crate::error::AccessError;
use crate::key::{FiberId, ImplId, Key};
use crate::state::State;

pub(crate) enum Located {
    /// A fiber on the way provides the binding itself, in the same realm.
    Own(ImplId),
    /// `fiber` declares the key; `binding` is its committed choice, if any.
    Declared { fiber: FiberId, binding: Option<ImplId> },
}

/// A reached binding: its identity, value and provider fiber.
pub(crate) type Resolved = (ImplId, Arc<dyn Any + Send + Sync>, FiberId);

impl State {
    pub(crate) fn resolved(&self, impl_id: ImplId) -> Option<Resolved> {
        let binding = self.impls.get(&impl_id)?;
        Some((impl_id, binding.value.clone(), binding.provider))
    }

    /// Walks from the context up through its fibers: own provisions in the
    /// same realm, then the nearest fiber declaring the key. The walk stops
    /// at a context that isolates the key again. The fiber chain is read off
    /// the (re-pointable) derivation chain, as with JavaScript prototypes.
    pub(crate) fn locate(&self, ctx: &Context, key: &Key) -> Option<Located> {
        let realm = self.realm_of(&ctx.scope, key);
        let mut node = Some(ctx.scope.clone());
        let mut id = ctx.fiber;
        loop {
            let fiber = self.fiber(id)?;
            let own = fiber.provisions.iter().rev().find(|impl_id| {
                self.impls
                    .get(impl_id)
                    .is_some_and(|binding| binding.key == *key && self.realm_of(&binding.provider_scope, key) == realm)
            });
            if let Some(impl_id) = own {
                return Some(Located::Own(*impl_id));
            }
            loop {
                let scope = node?;
                if Arc::ptr_eq(&scope, &fiber.scope) {
                    break;
                }
                if scope.isolates(key).is_some() {
                    return None;
                }
                node = scope.parent();
            }
            let binding = fiber.committed.as_ref().and_then(|committed| committed.get(key)).copied();
            if binding.is_some() || fiber.inject.contains(key) {
                return Some(Located::Declared { fiber: id, binding });
            }
            if id == FiberId::ROOT {
                return None;
            }
            node = fiber.scope.parent();
            id = node.as_ref()?.fiber()?;
        }
    }

    /// The binding an access from `ctx` reaches. The root context has
    /// unchecked access to the live store, as it cannot declare `inject`.
    pub(crate) fn access(&self, ctx: &Context, key: &Key) -> Result<Resolved, AccessError> {
        if self.fiber(ctx.fiber).is_none() {
            // the context's fiber has been disposed and collected
            return Err(AccessError::Inactive { key: key.clone() });
        }
        let found = if ctx.is_root() {
            self.live_impl(&ctx.scope, key, false)
        } else {
            match self.locate(ctx, key) {
                None => return Err(AccessError::Undeclared { key: key.clone() }),
                Some(Located::Own(impl_id)) => Some(impl_id),
                Some(Located::Declared { binding, .. }) => binding,
            }
        };
        found.and_then(|impl_id| self.resolved(impl_id)).ok_or_else(|| AccessError::Inactive { key: key.clone() })
    }
}
