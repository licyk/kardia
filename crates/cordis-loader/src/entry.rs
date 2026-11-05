//! The entry state machine (`config/entry.ts`). Every synchronous segment of
//! the reference code is one critical section on the loader state; after
//! every `await` the entry re-checks that it is still current.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use cordis_core::{Context, Disposal, Error, Fiber, FiberState, Key, PathSegment, PluginDef, RealmId, Result};
use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::Value;

use crate::expr::{evaluate_at, interpolate_at, truthy};
use crate::loader::{EntryHandle, EntryId, Loader, LoaderState, PatchHook};
use crate::options::{Disabled, EntryOptions, diff_keys};

/// Tag of the outer fibers that hold an entry's `inject`.
pub const OUTER_TAG: &str = "loader:outer";

impl Loader {
    /// `Entry.update` after the options have been assigned: rebuild, or
    /// patch the running fibers in place.
    pub(crate) fn entry_update(&self, entry: EntryId, legacy: Value, force: bool) -> BoxFuture<'static, Result<()>> {
        let this = self.clone();
        async move {
            let Some(changed) = this.resolve_inject(entry) else {
                drop(this.dispose_entry(entry));
                return Ok(());
            };
            if this.with(|st| st.blocked(entry)) {
                drop(this.dispose_entry(entry));
                return Ok(());
            }
            let Some((options, live)) = this.with(|st| {
                let data = st.entries.get(&entry)?;
                Some((data.options.clone(), data.outer.is_some() || data.inner_live()))
            }) else {
                return Ok(());
            };
            // the plugin and the outer fiber are bound when they are created
            let renamed = EntryOptions::read(&options).name != EntryOptions::read(&legacy).name;
            if changed || renamed || !live {
                // the disposal is part of the init task, so a concurrent update joins it
                let disposal = if live { this.dispose_entry(entry) } else { None };
                this.init(entry, disposal).await;
                return Ok(());
            }
            let diff = diff_keys(&options, &legacy);
            if diff.is_empty() && !force {
                return Ok(());
            }
            this.partial_dispose(entry, &legacy, true);
            this.patch_context(entry, diff).await
        }
        .boxed()
    }

    /// Evaluates `inject` (in the parent group's context). Returns whether
    /// it changed, or `None` on failure, which disables the entry.
    fn resolve_inject(&self, entry: EntryId) -> Option<bool> {
        let (declared, parent) = self.with(|st| {
            let data = st.entries.get(&entry)?;
            Some((data.read().inject, st.groups[&data.parent].ctx.clone()))
        })?;
        let mut inject = cordis_core::Inject::new();
        for key in declared.keys() {
            let mut path = vec![PathSegment::Key("inject".into()), PathSegment::Key(key.to_string())];
            match declared.meta(key).map(|meta| interpolate_at(&parent, meta, &mut path)).transpose() {
                Ok(Some(meta)) => inject = inject.with_meta(key.clone(), meta),
                Ok(None) => inject = inject.with(key.clone()),
                Err(error) => {
                    self.report(entry, &error);
                    self.with(|st| {
                        if let Some(data) = st.entries.get_mut(&entry) {
                            data.inject = cordis_core::Inject::new();
                            data.verdict = Some(true);
                        }
                    });
                    return None;
                }
            }
        }
        self.with(|st| {
            let data = st.entries.get_mut(&entry)?;
            let changed = inject != data.inject;
            data.inject = inject;
            Some(changed)
        })
    }

    /// Disposes the entry's top fiber, marking the disposal as loader-driven.
    pub(crate) fn dispose_entry(&self, entry: EntryId) -> Option<Disposal> {
        let fiber = self.with(|st| {
            let data = st.entries.get_mut(&entry)?;
            let fiber = data.outer.take().or_else(|| data.inner.clone())?;
            st.outer_fibers.remove(&fiber.id());
            Some(fiber)
        })?;
        Some(self.dispose_fiber(entry, &fiber))
    }

    fn dispose_fiber(&self, entry: EntryId, fiber: &Fiber) -> Disposal {
        self.set_disposing(entry, true);
        let disposal = fiber.dispose();
        self.set_disposing(entry, false);
        disposal
    }

    fn set_disposing(&self, entry: EntryId, disposing: bool) {
        self.with(|st| {
            if let Some(data) = st.entries.get_mut(&entry) {
                data.disposing = disposing;
            }
        });
    }

    /// Concurrency-safe (re)initialization: concurrent callers join one task.
    pub(crate) async fn init(&self, entry: EntryId, disposal: Option<Disposal>) {
        let (id, task) = {
            let mut state = self.0.state.lock();
            let Some(data) = state.entries.get_mut(&entry) else { return };
            match &data.init {
                Some(task) => task.clone(),
                None => {
                    let id = self.next_id();
                    let job = tokio::spawn(self.init_task(entry, disposal));
                    let task = async move { drop(job.await) }.boxed().shared();
                    data.init = Some((id, task.clone()));
                    drop(state);
                    self.0.pending.fetch_add(1, Ordering::SeqCst);
                    self.settle_after(entry, task.clone());
                    (id, task)
                }
            }
        };
        task.await;
        // `init_task` releases the task before creating fibers, whose apply may start another
        self.with(|st| {
            if let Some(data) = st.entries.get_mut(&entry)
                && data.init.as_ref().is_some_and(|(current, _)| *current == id)
            {
                data.init = None;
            }
        });
    }

    /// Releases the `await` barrier once the entry's fibers have settled.
    fn settle_after(&self, entry: EntryId, task: futures::future::Shared<BoxFuture<'static, ()>>) {
        let this = self.clone();
        tokio::spawn(async move {
            task.await;
            let fibers = this.with(|st| st.entries.get(&entry).map(|data| (data.outer.clone(), data.inner.clone())));
            if let Some((outer, inner)) = fibers {
                for fiber in outer.into_iter().chain(inner) {
                    let _ = fiber.settled().await;
                }
            }
            this.release_pending();
        });
    }

    fn init_task(&self, entry: EntryId, disposal: Option<Disposal>) -> BoxFuture<'static, ()> {
        let this = self.clone();
        async move {
            if let Some(disposal) = disposal {
                let _ = disposal.await;
            }
            // a later update has already rebuilt the entry
            let Some(name) = this.with(|st| {
                let data = st.entries.get_mut(&entry)?;
                if data.outer.is_some() || data.inner_live() {
                    return None;
                }
                data.verdict = None;
                Some(data.read().name)
            }) else {
                return;
            };
            let def = this.import(entry, &name).await;
            // the name may change while the plugin is resolved
            if this.current_name(entry).is_some_and(|current| current != name) {
                return this.init_task(entry, None).await;
            }
            let Some(def) = def else { return };
            if !this.with(|st| st.registered(entry) && !st.blocked(entry)) {
                return;
            }
            if let Err(error) = this.patch_context(entry, Vec::new()).await {
                this.report(entry, &error);
            }
            if this.current_name(entry).is_some_and(|current| current != name) {
                return this.init_task(entry, None).await;
            }
            let Some((ctx, inject)) = this.with(|st| {
                if !st.registered(entry) || st.blocked(entry) {
                    return None;
                }
                let data = st.entries.get_mut(&entry)?;
                data.init = None;
                Some((data.ctx.clone(), data.inject.clone()))
            }) else {
                return;
            };
            if inject.is_empty() {
                this.start(entry, &ctx, def);
                return;
            }
            match ctx.plugin_def(&this.outer_def(inject), Value::Null) {
                Ok(outer) => this.with(|st| {
                    st.outer_fibers.insert(outer.id(), entry);
                    if let Some(data) = st.entries.get_mut(&entry) {
                        data.outer = Some(outer);
                    }
                }),
                Err(error) => this.0.ctx.logger().log_error(&error),
            }
        }
        .boxed()
    }

    fn current_name(&self, entry: EntryId) -> Option<String> {
        self.with(|st| st.entries.get(&entry).map(|data| data.read().name))
    }

    /// Resolves the entry's plugin; failures are reported here.
    pub(crate) async fn import(&self, entry: EntryId, name: &str) -> Option<PluginDef> {
        let resolver = self.with(|st| {
            let tree = st.groups[&st.entries.get(&entry)?.parent].tree;
            st.trees[&tree].backend.resolver()
        });
        let resolver = resolver.unwrap_or_else(|| self.0.resolver.clone());
        match resolver.resolve(name).await {
            Ok(def) => Some(def),
            Err(error) => {
                self.report(entry, &error);
                None
            }
        }
    }

    /// Holds the entry's `inject`; resolves the plugin again (it may have
    /// been replaced meanwhile) and stops at a checkpoint if a dependency
    /// was lost during the resolution.
    fn outer_def(&self, inject: cordis_core::Inject) -> PluginDef {
        let loader = Arc::downgrade(&self.0);
        PluginDef::new(move |ctx, _| {
            let loader = loader.upgrade().map(Loader);
            async move {
                let Some(loader) = loader else { return Ok(()) };
                let Some(entry) = loader.entry_of_context(&ctx) else { return Ok(()) };
                let Some(name) = loader.current_name(entry) else { return Ok(()) };
                let def = loader.import(entry, &name).await;
                ctx.checkpoint()?;
                if let Some(def) = def {
                    loader.start(entry, &ctx, def);
                }
                Ok(())
            }
        })
        .name(OUTER_TAG)
        .tag(OUTER_TAG)
        .inject(inject)
    }

    /// Starts the inner (plugin) fiber in `ctx`, unless the entry is disabled.
    pub(crate) fn start(&self, entry: EntryId, ctx: &Context, def: PluginDef) {
        let (prepared, failures) = self.with(|st| {
            if !st.registered(entry) {
                return (None, Vec::new());
            }
            let (admitted, failures) = st.admit(entry, ctx);
            let data = &st.entries[&entry];
            (admitted.then(|| (data.intercept.clone(), data.read().config)), failures)
        });
        for error in &failures {
            self.report(entry, error);
        }
        let Some((intercept, config)) = prepared else { return };
        self.show_log(entry, "apply");
        let inner_ctx = ctx.extend_with(&intercept);
        match inner_ctx.plugin_def(&def, config) {
            Ok(fiber) => self.with(|st| {
                st.inner_fibers.insert(fiber.id(), entry);
                if let Some(data) = st.entries.get_mut(&entry) {
                    data.inner = Some(fiber);
                }
            }),
            Err(error) => self.report(entry, &error),
        }
    }

    /// Re-attaches the entry under its group (with its isolation table) and
    /// reconciles the running fibers with the changed option keys.
    pub(crate) async fn patch_context(&self, entry: EntryId, diff: Vec<String>) -> Result<()> {
        let (table, diffs) = self.isolate_prepare(entry);
        let hooks: Arc<[PatchHook]> = self.with(|st| st.patch_hooks.values().cloned().collect());
        let result = self.patch_chain(entry, hooks, 0, table, diff).await;
        self.isolate_finish(diffs);
        result
    }

    /// The `on_patch_context` hooks from `index` on, around the patch itself.
    /// Each `next` builds the rest of the chain only when it is awaited, so a
    /// hook runs after the ones before it, and not at all if one of them
    /// does not continue.
    fn patch_chain(
        &self,
        entry: EntryId,
        hooks: Arc<[PatchHook]>,
        index: usize,
        table: HashMap<Key, RealmId>,
        diff: Vec<String>,
    ) -> BoxFuture<'static, Result<()>> {
        let this = self.clone();
        let Some(hook) = hooks.get(index).cloned() else {
            return async move { this.patch_fibers(entry, table, diff).await }.boxed();
        };
        let handle = EntryHandle { loader: self.clone(), id: entry };
        let next = async move { this.patch_chain(entry, hooks, index + 1, table, diff).await }.boxed();
        hook(handle, next)
    }

    /// Moves the entry's context under its parent's and reconciles its fibers.
    async fn patch_fibers(&self, entry: EntryId, table: HashMap<Key, RealmId>, diff: Vec<String>) -> Result<()> {
        let Some((outer, inner_live, ctx)) = self.with(|st| {
            let parent = st.groups[&st.entries.get(&entry)?.parent].ctx.clone();
            let data = st.entries.get_mut(&entry)?;
            data.ctx = data.ctx.reparent(&parent);
            data.overlay.set_isolations(table);
            Some((data.outer.clone(), data.inner_live(), data.ctx.clone()))
        }) else {
            return Ok(());
        };
        match outer {
            Some(outer) if outer.state() == FiberState::Active => {
                self.reconcile(entry, outer.context().clone(), &diff).await
            }
            Some(_) => Ok(()),
            None if inner_live => self.reconcile(entry, ctx, &diff).await,
            None => Ok(()),
        }
    }

    async fn reconcile(&self, entry: EntryId, ctx: Context, diff: &[String]) -> Result<()> {
        let Some((inner, outer)) = self.with(|st| {
            let data = st.entries.get(&entry)?;
            Some((data.inner.clone().filter(|fiber| !fiber.is_removed()), data.outer.clone()))
        }) else {
            return Ok(());
        };
        let Some(inner) = inner else {
            // the reactivation evaluates `disabled`
            if let Some(outer) = outer {
                let _ = outer.restart().await;
            }
            return Ok(());
        };
        let (admitted, failures, options) = self.with(|st| {
            let (admitted, failures) = st.admit(entry, &ctx);
            (admitted, failures, st.entries[&entry].read())
        });
        for error in &failures {
            self.report(entry, error);
        }
        if !admitted {
            drop(self.dispose_fiber(entry, &inner));
            return Ok(());
        }
        if !diff.iter().any(|key| key == "config") && !options.group {
            return Ok(());
        }
        let mut config = options.config;
        loop {
            let settle = inner.update_with(config.clone(), true)?;
            // a concurrent update may have changed the options since they were
            // read; whichever update applies last applies the latest config
            let latest = self.with(|st| st.entries.get(&entry).map(|data| data.read().config));
            match latest {
                Some(latest) if latest != config => config = latest,
                _ => return settle.await,
            }
        }
    }

    pub(crate) fn partial_dispose(&self, entry: EntryId, legacy: &Value, active: bool) {
        self.with(|st| st.collect_realms(entry, legacy, active));
    }
}

impl LoaderState {
    /// Evaluates `disabled`, then, unless it disables the entry, `intercept`.
    /// Returns whether the plugin may run, and the failures to report. A
    /// group still runs with a failing `disabled` (its verdict only reaches
    /// its children), but not with a failing `intercept`.
    pub(crate) fn admit(&mut self, entry: EntryId, ctx: &Context) -> (bool, Vec<Error>) {
        let mut failures: Vec<Error> = self.resolve_disabled(entry, ctx).into_iter().collect();
        if self.is_disabled(entry) {
            return (false, failures);
        }
        let intercept = self.apply_intercept(entry, ctx);
        let admitted = intercept.is_none();
        failures.extend(intercept);
        (admitted, failures)
    }

    /// `disabled` evaluates to a verdict in `ctx` (the outer fiber's
    /// context, which sees the entry's `inject`); a failure disables the entry.
    pub(crate) fn resolve_disabled(&mut self, entry: EntryId, ctx: &Context) -> Option<Error> {
        let data = self.entries.get_mut(&entry)?;
        let (verdict, failure) = match data.read().disabled {
            None => (false, None),
            Some(Disabled::Bool(disabled)) => (disabled, None),
            Some(Disabled::Expr(source)) => match evaluate_at(ctx, &source, &[PathSegment::Key("disabled".into())]) {
                Ok(value) => (truthy(&value), None),
                Err(error) => (true, Some(error)),
            },
        };
        data.verdict = Some(verdict);
        failure
    }

    /// Groups are never disabled themselves; they pass their verdict on.
    pub(crate) fn is_disabled(&self, entry: EntryId) -> bool {
        let Some(data) = self.entries.get(&entry) else { return true };
        if data.read().group {
            return false;
        }
        data.verdict != Some(false) || self.parent_disabled(entry)
    }

    fn parent_disabled(&self, entry: EntryId) -> bool {
        let mut group = self.entries[&entry].parent;
        while let Some(owner) = self.groups.get(&group).and_then(|g| g.owner) {
            let Some(data) = self.entries.get(&owner) else { return false };
            if data.verdict != Some(false) {
                return true;
            }
            group = data.parent;
        }
        false
    }

    /// A literal `disabled: true` or a disabled ancestor blocks the entry;
    /// an expression waits for the patched context.
    pub(crate) fn blocked(&mut self, entry: EntryId) -> bool {
        let Some(data) = self.entries.get_mut(&entry) else { return true };
        let options = data.read();
        if options.group {
            return false;
        }
        if options.disabled == Some(Disabled::Bool(true)) {
            data.verdict = Some(true);
            return true;
        }
        self.parent_disabled(entry)
    }

    /// Evaluates the entry's `intercept` in `ctx` and swaps its
    /// interception table in place, so that running plugins see the new
    /// metadata without a restart. A plugin does not run with an intercept
    /// that cannot be evaluated: a failure disables the entry.
    pub(crate) fn apply_intercept(&mut self, entry: EntryId, ctx: &Context) -> Option<Error> {
        let data = self.entries.get_mut(&entry)?;
        let intercept = Value::Object(data.read().intercept);
        match interpolate_at(ctx, &intercept, &mut vec![PathSegment::Key("intercept".into())]) {
            Ok(Value::Object(table)) => {
                data.intercept.set_intercepts(table.into_iter().map(|(key, meta)| (key.into(), meta)).collect());
                None
            }
            Ok(_) => None,
            Err(error) => {
                data.verdict = Some(true);
                Some(error)
            }
        }
    }
}
