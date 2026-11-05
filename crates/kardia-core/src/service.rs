use std::any::{Any, type_name};
use std::fmt;
use std::ops::Deref;
use std::sync::{Arc, Weak};

use serde_json::Value;

use crate::access::{Located, Resolved};
use crate::context::{Context, merge_shallow};
use crate::effect::{Disposer, EffectHandle};
use crate::error::{AccessError, KardiaError, Result};
use crate::fiber::Fiber;
use crate::fiber::FiberState;
use crate::key::{FiberId, ImplId, Key, RealmId};
use crate::logger::Logger;
use crate::root::RootInner;
use crate::state::{Check, Impl, State, View};

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
    provider: FiberId,
}

impl<T> Clone for Svc<T> {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            caller: self.caller.clone(),
            key: self.key.clone(),
            impl_id: self.impl_id,
            provider: self.provider,
        }
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

    /// The fiber that provides this binding (the service's def site).
    pub fn provider(&self) -> Fiber {
        Fiber::of(&self.caller.root, self.provider)
    }

    /// A logger for work done on the caller's behalf: named by the caller's
    /// `intercept("logger", { name })`, else after the providing plugin.
    pub fn logger(&self) -> Logger {
        let fallback = self.caller.root.with(|st| st.fiber_name(self.provider));
        Logger::for_caller(&self.caller, &fallback)
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
        let view = (&value as &dyn Any).downcast_ref::<Arc<Value>>().map(|value| {
            let value = value.clone();
            Arc::new(move || (*value).clone()) as View
        });
        self.bind_effect(key.into(), value, check, view)
    }

    /// Provides a value that expressions and tooling can read as JSON.
    pub fn provide_serialized_as<T: Any + Send + Sync + serde::Serialize>(
        &self,
        key: impl Into<Key>,
        value: T,
    ) -> Result<EffectHandle> {
        let value = Arc::new(value);
        let reader = value.clone();
        let view: View = Arc::new(move || serde_json::to_value(&*reader).unwrap_or(Value::Null));
        self.bind_effect(key.into(), value, None, Some(view))
    }

    fn bind_effect(
        &self,
        key: Key,
        value: Arc<dyn Any + Send + Sync>,
        check: Option<Check>,
        view: Option<View>,
    ) -> Result<EffectHandle> {
        let label = format!("ctx.provide({:?})", key.as_str());
        self.effect_sync(label, |scope| {
            let id = self.root.run(|st| st.bind(self, key, value, check, view))?;
            scope.defer(withdraw(Arc::downgrade(&self.root), id));
            Ok(())
        })
    }

    /// Moves the live binding of `key` from realm `from` to realm `to`, if
    /// `to` is free. The binding keeps its identity, so consumers that
    /// committed to it are not restarted. Returns whether it moved.
    pub fn migrate_binding(&self, key: &str, from: RealmId, to: RealmId) -> bool {
        self.root.run(|st| {
            let Some(&id) = st.store.get(&from) else { return false };
            if st.impls[&id].key.as_str() != key || st.store.contains_key(&to) {
                return false;
            }
            st.store.remove(&from);
            st.store.insert(to, id);
            st.impls.get_mut(&id).expect("bound").realm = to;
            true
        })
    }

    /// Re-resolves every fiber that declares `key` and resolves it to `realm`
    /// (after isolation tables changed under live fibers).
    pub fn refresh_consumers(&self, key: &str, realm: RealmId) {
        let key = Key::new(key);
        self.root.run(|st| st.notify(&key, realm));
    }

    /// The live bindings of `key`: each realm with the binding it holds.
    pub fn bindings(&self, key: &str) -> Vec<(RealmId, ImplId)> {
        self.root.with(|st| {
            let mut bindings: Vec<_> = st
                .store
                .iter()
                .filter(|(_, id)| st.impls[id].key.as_str() == key)
                .map(|(realm, id)| (*realm, *id))
                .collect();
            bindings.sort_by_key(|(_, id)| *id);
            bindings
        })
    }

    /// The context that provided the live binding of `key` in `realm`.
    pub fn binding_provider(&self, key: &str, realm: RealmId) -> Option<Context> {
        self.root.with(|st| {
            let binding = &st.impls[st.store.get(&realm)?];
            (binding.key.as_str() == key).then(|| Context {
                root: self.root.clone(),
                fiber: binding.provider,
                scope: binding.provider_scope.clone(),
            })
        })
    }

    /// Checked access (Algorithm 6): resolves through committed views.
    pub fn get<S: Service>(&self) -> Result<Svc<S>, AccessError> {
        self.get_as(S::NAME)
    }

    pub fn get_as<T: Any + Send + Sync>(&self, key: impl Into<Key>) -> Result<Svc<T>, AccessError> {
        let key = key.into();
        let resolved = self.root.with(|st| st.access(self, &key))?;
        self.wrap(key, resolved)
    }

    /// Checked access to the JSON view of a service (see
    /// [`Context::provide_serialized_as`]; JSON values are their own view).
    pub fn view(&self, key: impl Into<Key>) -> Result<Value, AccessError> {
        let key = key.into();
        let (impl_id, ..) = self.root.with(|st| st.access(self, &key))?;
        let view = self.root.with(|st| st.impls.get(&impl_id).and_then(|binding| binding.view.clone()));
        view.map(|view| view()).ok_or(AccessError::TypeMismatch { key, expected: "a JSON view" })
    }

    /// Reflective access to the live store: the current binding of the
    /// realm, if its provider is active. Never fails for undeclared keys.
    pub fn live<S: Service>(&self) -> Option<Svc<S>> {
        self.live_as(S::NAME)
    }

    pub fn live_as<T: Any + Send + Sync>(&self, key: impl Into<Key>) -> Option<Svc<T>> {
        let key = key.into();
        let resolved = self.root.with(|st| st.resolved(st.live_impl(&self.scope, &key, true)?))?;
        self.wrap(key, resolved).ok()
    }

    fn wrap<T: Any + Send + Sync>(
        &self,
        key: Key,
        (impl_id, value, provider): Resolved,
    ) -> Result<Svc<T>, AccessError> {
        let value = value
            .downcast::<T>()
            .map_err(|_| AccessError::TypeMismatch { key: key.clone(), expected: type_name::<T>() })?;
        Ok(Svc { value, caller: self.clone(), key, impl_id, provider })
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
        view: Option<View>,
    ) -> Result<ImplId, KardiaError> {
        // a retired fiber's stale load may still collect inverses, but it must
        // not occupy a realm its successor may already be providing in
        if self.fiber(ctx.fiber).is_none_or(|fiber| fiber.removed) {
            return Err(KardiaError::InactiveEffect);
        }
        let root_realm = *self.root_realms.entry(key.clone()).or_insert_with(RealmId::new);
        let realm = ctx.scope.own_realm(&key).unwrap_or(root_realm);
        if let Some(existing) = self.store.get(&realm) {
            let by = self.fiber_name(self.impls[existing].provider);
            return Err(KardiaError::AlreadyProvided { key, by });
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
                view,
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
