use std::any::{Any, type_name};
use std::fmt;
use std::ops::Deref;
use std::sync::{Arc, Weak};

use serde_json::Value;

use crate::access::Located;
use crate::context::{Context, merge_shallow};
use crate::effect::{Disposer, EffectHandle};
use crate::error::{AccessError, CordisError, Result};
use crate::fiber::FiberState;
use crate::key::{FiberId, ImplId, Key, RealmId};
use crate::root::RootInner;
use crate::state::{Check, Impl, State};

/// A service type bound to its coeffect key.
pub trait Service: Any + Send + Sync {
    /// Coeffect key. A string (not a type id) because configuration names services.
    const NAME: &'static str;

    /// Merges interception metadata (paper Def. 26), outermost first.
    /// Defaults to a right-biased shallow merge of JSON objects.
    fn merge_intercept(values: Vec<Value>) -> Value {
        merge_shallow(values)
    }
}

/// An accessed service together with the context that accessed it.
///
/// The caller context is where effects created on the caller's behalf
/// belong (`svc.caller().effect(..)`), and where interception metadata is
/// read from. A service resolves its own dependencies through the context it
/// stored when it was constructed.
pub struct Svc<T> {
    value: Arc<T>,
    caller: Context,
    key: Key,
    impl_id: ImplId,
}

impl<T> Clone for Svc<T> {
    fn clone(&self) -> Self {
        Self { value: self.value.clone(), caller: self.caller.clone(), key: self.key.clone(), impl_id: self.impl_id }
    }
}

impl<T> Deref for Svc<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T: fmt::Debug> fmt::Debug for Svc<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&*self.value, f)
    }
}

impl<T> Svc<T> {
    pub fn caller(&self) -> &Context {
        &self.caller
    }

    pub fn key(&self) -> &Key {
        &self.key
    }

    pub fn impl_id(&self) -> ImplId {
        self.impl_id
    }

    pub fn arc(&self) -> &Arc<T> {
        &self.value
    }

    /// Interception metadata applying to this access, outermost first:
    /// the metadata declared with the admitting `inject`, then the caller's
    /// context chain.
    pub fn intercepts(&self) -> Vec<Value> {
        let mut values = Vec::new();
        if let Some(meta) = self.caller.declared_meta(&self.key) {
            values.push(meta);
        }
        values.extend(self.caller.intercepts(&self.key));
        values
    }
}

impl<S: Service> Svc<S> {
    /// The merged interception metadata of this access.
    pub fn meta(&self) -> Value {
        S::merge_intercept(self.intercepts())
    }
}

impl Context {
    /// Provides a service in this context's realm, as an effect of the
    /// current fiber. One realm holds at most one binding per key.
    pub fn provide<S: Service>(&self, value: S) -> Result<EffectHandle> {
        self.provide_arc_as(S::NAME, Arc::new(value))
    }

    pub fn provide_arc<S: Service>(&self, value: Arc<S>) -> Result<EffectHandle> {
        self.provide_arc_as(S::NAME, value)
    }

    /// Provides any value under an explicit key.
    pub fn provide_as<T: Any + Send + Sync>(&self, key: impl Into<Key>, value: T) -> Result<EffectHandle> {
        self.provide_arc_as(key, Arc::new(value))
    }

    pub fn provide_arc_as<T: Any + Send + Sync>(&self, key: impl Into<Key>, value: Arc<T>) -> Result<EffectHandle> {
        self.provide_checked(key, value, None)
    }

    /// Provides a value whose availability additionally depends on `check`,
    /// evaluated for each consumer context. The predicate runs under the
    /// runtime lock and must not call into the runtime.
    pub fn provide_checked<T: Any + Send + Sync>(
        &self,
        key: impl Into<Key>,
        value: Arc<T>,
        check: Option<Check>,
    ) -> Result<EffectHandle> {
        let key = key.into();
        let label = format!("ctx.provide({:?})", key.as_str());
        self.effect_sync(label, |scope| {
            let id = self.root.run(|st| st.bind(self, key, value, check))?;
            scope.defer(withdraw(Arc::downgrade(&self.root), id));
            Ok(())
        })
    }

    /// Checked access (Algorithm 6): resolves through committed views.
    pub fn get<S: Service>(&self) -> Result<Svc<S>, AccessError> {
        self.get_as(S::NAME)
    }

    pub fn get_as<T: Any + Send + Sync>(&self, key: impl Into<Key>) -> Result<Svc<T>, AccessError> {
        let key = key.into();
        let (impl_id, value) = self.root.with(|st| st.access(self, &key))?;
        self.wrap(key, impl_id, value)
    }

    /// Reflective access to the live store: the current binding of the
    /// realm, if its provider is active. Never fails for undeclared keys.
    pub fn live<S: Service>(&self) -> Option<Svc<S>> {
        self.live_as(S::NAME)
    }

    pub fn live_as<T: Any + Send + Sync>(&self, key: impl Into<Key>) -> Option<Svc<T>> {
        let key = key.into();
        let (impl_id, value) = self.root.with(|st| {
            let id = st.live_impl(&self.scope, &key, true)?;
            Some((id, st.impls[&id].value.clone()))
        })?;
        self.wrap(key, impl_id, value).ok()
    }

    fn wrap<T: Any + Send + Sync>(
        &self,
        key: Key,
        impl_id: ImplId,
        value: Arc<dyn Any + Send + Sync>,
    ) -> Result<Svc<T>, AccessError> {
        let value = value
            .downcast::<T>()
            .map_err(|_| AccessError::TypeMismatch { key: key.clone(), expected: type_name::<T>() })?;
        Ok(Svc { value, caller: self.clone(), key, impl_id })
    }

    /// Interception metadata declared by the fiber whose `inject` admits an
    /// access to `key` from this context.
    pub(crate) fn declared_meta(&self, key: &Key) -> Option<Value> {
        self.root.with(|st| match st.locate(self, key)? {
            Located::Declared { fiber, .. } => st.fiber(fiber)?.inject.meta(key).cloned(),
            Located::Own(_) => None,
        })
    }
}

fn withdraw(root: Weak<RootInner>, id: ImplId) -> Disposer {
    Disposer::future(move || {
        let root = root.upgrade();
        if let Some(root) = &root {
            root.run(|st| st.unbind(id));
        }
        async move {
            let Some(root) = root else { return };
            // Retired bindings cannot gain new dependents; wait for the
            // cleanup of those that committed to it.
            root.wait_until(|st| st.impls.get(&id).is_none_or(|i| i.dependents.is_empty())).await;
            root.run(|st| st.retire_provision(id));
        }
    })
}

impl State {
    pub(crate) fn bind(
        &mut self,
        ctx: &Context,
        key: Key,
        value: Arc<dyn Any + Send + Sync>,
        check: Option<Check>,
    ) -> Result<ImplId, CordisError> {
        // a retired fiber's stale load may still collect inverses, but it must
        // not occupy a realm its successor may already be providing in
        if self.fiber(ctx.fiber).is_none_or(|fiber| fiber.removed) {
            return Err(CordisError::InactiveEffect);
        }
        let root_realm = *self.root_realms.entry(key.clone()).or_insert_with(RealmId::new);
        let realm = ctx.scope.own_realm(&key).unwrap_or(root_realm);
        if let Some(existing) = self.store.get(&realm) {
            let by = self.fiber_name(self.impls[existing].provider);
            return Err(CordisError::AlreadyProvided { key, by });
        }
        let id = self.next_impl_id();
        self.impls.insert(
            id,
            Impl {
                key: key.clone(),
                realm,
                provider: ctx.fiber,
                provider_scope: ctx.scope.clone(),
                value,
                check,
                dependents: Default::default(),
            },
        );
        self.store.insert(realm, id);
        let fiber = self.fiber_mut(ctx.fiber).expect("provider fiber is active");
        fiber.provisions.push(id);
        if fiber.state == FiberState::Active {
            self.notify(&key, realm);
        }
        Ok(id)
    }

    pub(crate) fn unbind(&mut self, id: ImplId) {
        let Some(binding) = self.impls.get(&id) else { return };
        let (key, realm) = (binding.key.clone(), binding.realm);
        if self.store.get(&realm) == Some(&id) {
            self.store.remove(&realm);
            self.notify(&key, realm);
        }
    }

    /// Removes a withdrawn binding from its provider's provisions. During a
    /// drain this is postponed to the end of the drain, so that the
    /// provider's descendants can still reach it during their own cleanup.
    pub(crate) fn retire_provision(&mut self, id: ImplId) {
        let Some(provider) = self.impls.get(&id).map(|binding| binding.provider) else {
            return;
        };
        if let Some(fiber) = self.fibers.get_mut(&provider)
            && fiber.draining
        {
            fiber.withdrawn.push(id);
            return;
        }
        self.remove_provision(provider, id);
    }

    pub(crate) fn remove_provision(&mut self, provider: FiberId, id: ImplId) {
        if let Some(fiber) = self.fibers.get_mut(&provider) {
            fiber.provisions.retain(|p| *p != id);
        }
        if let Some(binding) = self.impls.remove(&id) {
            self.drop_later(binding.value);
        }
    }

    /// The binding a context would see in the live store.
    /// `strict` additionally requires its provider to be active.
    pub(crate) fn live_impl(&self, scope: &Arc<crate::scope::Scope>, key: &str, strict: bool) -> Option<ImplId> {
        let realm = self.realm_of(scope, key)?;
        let id = *self.store.get(&realm)?;
        if strict {
            let provider = self.impls[&id].provider;
            if self.fiber(provider).is_none_or(|fiber| fiber.state != FiberState::Active) {
                return None;
            }
        }
        Some(id)
    }
}
