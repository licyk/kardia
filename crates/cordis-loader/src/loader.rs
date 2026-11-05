use std::collections::HashMap;
use std::hash::{BuildHasher, RandomState};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use cordis_core::{
    Check, ConfigCommitted, Context, Error, Fiber, FiberId, FiberState, Inject, Key, Overlay, PathSegment, PluginDef,
    PluginRemoved, RealmId, Result, Service, StatusChanged, merge_shallow,
};
use futures::future::{BoxFuture, Shared};
use parking_lot::Mutex;
use serde_json::{Map, Value};

use crate::catalog::Resolver;
use crate::locate::Location;
use crate::options::{EntryOptions, EntryValidationError, SEP, merge, sort_keys, validate_options};

pub(crate) type EntryId = u64;
pub(crate) type GroupId = u64;
pub(crate) type TreeId = u64;

/// The loader service (`"loader"`): a declarative tree of entries kept in
/// step with live fibers in both directions.
#[derive(Clone)]
pub struct Loader(pub(crate) Arc<LoaderInner>);

impl Service for Loader {
    const NAME: &'static str = "loader";
}

pub(crate) struct LoaderInner {
    pub(crate) ctx: Context,
    pub(crate) resolver: Arc<dyn Resolver>,
    pub(crate) state: Mutex<LoaderState>,
    /// Entries whose initialization has not settled yet (the `await` barrier).
    pub(crate) pending: Arc<AtomicUsize>,
    next_id: AtomicU64,
    pub(crate) root_tree: TreeId,
    pub(crate) enable_logs: AtomicBool,
}

#[derive(Default)]
pub(crate) struct LoaderState {
    pub(crate) entries: HashMap<EntryId, EntryData>,
    pub(crate) groups: HashMap<GroupId, GroupData>,
    pub(crate) trees: HashMap<TreeId, TreeData>,
    /// Inner fibers created by the loader, by fiber.
    pub(crate) inner_fibers: HashMap<FiberId, EntryId>,
    pub(crate) outer_fibers: HashMap<FiberId, EntryId>,
    /// Shared realms by label, then key.
    pub(crate) global_realms: HashMap<String, HashMap<Key, RealmId>>,
}

pub(crate) struct TreeData {
    pub(crate) ctx: Context,
    pub(crate) root: GroupId,
    /// Entries by local id.
    pub(crate) store: HashMap<String, EntryId>,
    pub(crate) backend: Arc<dyn TreeBackend>,
    /// The entry whose plugin owns this tree (none for the loader's own tree).
    pub(crate) owner: Option<EntryId>,
}

pub(crate) struct GroupData {
    pub(crate) ctx: Context,
    pub(crate) tree: TreeId,
    /// The entry options of the group, in order (the source of truth).
    pub(crate) data: Vec<Value>,
    pub(crate) owner: Option<EntryId>,
}

pub(crate) struct EntryData {
    pub(crate) parent: GroupId,
    /// The options the entry runs with (also kept in its group's data).
    pub(crate) options: Value,
    /// The entry's re-pointable context; its fiber is its group's.
    pub(crate) ctx: Context,
    /// The entry's own isolation table.
    pub(crate) overlay: Arc<Overlay>,
    /// The interception table shared by the contexts of its inner fibers.
    pub(crate) intercept: Arc<Overlay>,
    /// Holds the entry's `inject` and parents the inner fiber.
    pub(crate) outer: Option<Fiber>,
    pub(crate) inner: Option<Fiber>,
    pub(crate) subgroup: Option<GroupId>,
    pub(crate) subtree: Option<TreeId>,
    pub(crate) inject: Inject,
    pub(crate) init: Option<(u64, Shared<BoxFuture<'static, ()>>)>,
    /// Set while the loader itself disposes the entry's fiber.
    pub(crate) disposing: bool,
    /// The evaluated `disabled`; `None` (undecided) reads as disabled.
    pub(crate) verdict: Option<bool>,
    pub(crate) local_realms: HashMap<Key, RealmId>,
}

impl EntryData {
    pub(crate) fn read(&self) -> EntryOptions {
        EntryOptions::read(&self.options)
    }

    pub(crate) fn local_id(&self) -> String {
        self.read().id
    }

    pub(crate) fn inner_live(&self) -> bool {
        self.inner.as_ref().is_some_and(|fiber| !fiber.is_removed())
    }
}

/// Where a tree's changes are persisted.
pub trait TreeBackend: Send + Sync + 'static {
    /// Called synchronously after a change has been applied to the tree.
    fn commit(&self, change: &EntryChange) {
        let _ = change;
    }

    /// Resolves plugin names for this tree; defaults to the loader's resolver.
    fn resolver(&self) -> Option<Arc<dyn Resolver>> {
        None
    }

    /// The directory relative paths inside this tree resolve against.
    fn base_dir(&self) -> Option<std::path::PathBuf> {
        None
    }

    /// The object behind the tree, for its owner to find it again.
    fn handle(&self) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        None
    }

    /// Where the value at `path` in the options of entry `id` is written.
    /// With `value`, only if that is the value written there; without, a
    /// missing value is located at its closest written ancestor.
    fn locate(&self, id: &str, path: &[PathSegment], value: Option<&Value>) -> Option<Location> {
        let _ = (id, path, value);
        None
    }

    /// Where char `offset` of the expression `expr` at `path` in the
    /// options of entry `id` is written, if it is written there.
    fn locate_expr(&self, id: &str, path: &[PathSegment], expr: &str, offset: usize) -> Option<Location> {
        let _ = (id, path, expr, offset);
        None
    }
}

/// The loader's own tree lives in memory only.
pub struct MemoryTree;

impl TreeBackend for MemoryTree {}

/// One tree-side mutation, reported after it has been applied.
///
/// `options` absent: the entry was removed from `group`. `legacy` absent:
/// it was created there. Both present: it was updated, and `from` is the
/// group it left if it moved within the tree. A move into another tree is
/// reported as a removal from the source tree and a creation in the target.
#[derive(Clone)]
pub struct EntryChange {
    pub id: String,
    pub group: GroupHandle,
    pub from: Option<GroupHandle>,
    pub options: Option<Value>,
    pub legacy: Option<Value>,
}

#[derive(Clone)]
pub struct EntryHandle {
    pub(crate) loader: Loader,
    pub(crate) id: EntryId,
}

#[derive(Clone)]
pub struct GroupHandle {
    pub(crate) loader: Loader,
    pub(crate) id: GroupId,
}

#[derive(Clone)]
pub struct TreeHandle {
    pub(crate) loader: Loader,
    pub(crate) id: TreeId,
}

macro_rules! handle_eq {
    ($($handle:ident),*) => {$(
        impl PartialEq for $handle {
            fn eq(&self, other: &Self) -> bool {
                Arc::ptr_eq(&self.loader.0, &other.loader.0) && self.id == other.id
            }
        }

        impl std::fmt::Debug for $handle {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, concat!(stringify!($handle), "({})"), self.id)
            }
        }
    )*};
}

handle_eq!(EntryHandle, GroupHandle, TreeHandle);

impl Loader {
    /// The loader plugin, resolving entry names through `resolver`.
    pub fn plugin(resolver: impl Resolver) -> PluginDef {
        Self::plugin_with(Arc::new(resolver), Arc::new(MemoryTree))
    }

    /// The loader plugin with a custom backend for its own tree.
    pub fn plugin_with(resolver: Arc<dyn Resolver>, backend: Arc<dyn TreeBackend>) -> PluginDef {
        PluginDef::new(move |ctx, _| {
            let (resolver, backend) = (resolver.clone(), backend.clone());
            async move { Loader::install(ctx, resolver, backend) }
        })
        .name("loader")
        .provides([Loader::NAME])
    }

    fn install(ctx: Context, resolver: Arc<dyn Resolver>, backend: Arc<dyn TreeBackend>) -> Result<()> {
        let mut state = LoaderState::default();
        let tree_ctx = ctx.extend();
        let (root_tree, root_group) = (1, 2);
        state
            .groups
            .insert(root_group, GroupData { ctx: tree_ctx.clone(), tree: root_tree, data: Vec::new(), owner: None });
        state.trees.insert(
            root_tree,
            TreeData { ctx: tree_ctx, root: root_group, store: HashMap::new(), backend, owner: None },
        );
        let loader = Loader(Arc::new(LoaderInner {
            ctx: ctx.clone(),
            resolver,
            state: Mutex::new(state),
            pending: Arc::new(AtomicUsize::new(0)),
            next_id: AtomicU64::new(3),
            root_tree,
            enable_logs: AtomicBool::new(false),
        }));
        let pending = loader.0.pending.clone();
        // `inject: { loader: { await: true } }` waits until every entry has settled
        let check: Check = Arc::new(move |meta: &[Value]| {
            merge_shallow(meta.iter().cloned())["await"] != Value::Bool(true) || pending.load(Ordering::SeqCst) == 0
        });
        ctx.provide_checked(Loader::NAME, Arc::new(loader.clone()), Some(check))?;
        let this = loader.clone();
        ctx.on(move |event: &PluginRemoved| this.on_removed(&event.fiber))?;
        let this = loader.clone();
        ctx.on(move |event: &ConfigCommitted| this.on_committed(&event.fiber, &event.config))?;
        let this = loader.clone();
        ctx.on(move |event: &StatusChanged| this.on_status(&event.fiber, event.new))?;
        let this = loader.clone();
        ctx.on_config(Arc::new(move |fiber, config, next| {
            let source = config.clone();
            this.interpolate_config(fiber, config)
                .and_then(next)
                .map_err(|error| this.locate_config_error(fiber, &source, error))
        }))?;
        Ok(())
    }

    /// The configuration of an entry's own plugin is interpolated in the
    /// plugin's context on every (re)load; the source keeps the expressions.
    /// A group's configuration is its entry list and stays as it is, and
    /// plugins created by plugin code get their config from that code.
    fn interpolate_config(&self, fiber: &Fiber, config: Value) -> Result<Value> {
        if !crate::expr::has_expr(&config) || self.own_entry(fiber).is_none() {
            return Ok(config);
        }
        crate::expr::interpolate_at(fiber.context(), &config, &mut vec![PathSegment::Key("config".into())])
    }

    /// The entry whose own (non-group) plugin runs in `fiber`.
    pub(crate) fn own_entry(&self, fiber: &Fiber) -> Option<EntryId> {
        let ctx = fiber.context();
        let entry = self.entry_of_context(ctx)?;
        self.with(|st| {
            let data = st.entries.get(&entry)?;
            let own = !data.read().group
                && ctx.parent_overlay().is_some_and(|overlay| Arc::ptr_eq(&overlay, &data.intercept));
            own.then_some(entry)
        })
    }

    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut LoaderState) -> R) -> R {
        f(&mut self.0.state.lock())
    }

    pub(crate) fn next_id(&self) -> u64 {
        self.0.next_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn context(&self) -> &Context {
        &self.0.ctx
    }

    /// Logs `apply plugin` / `unload plugin` messages for entries.
    pub fn set_logs(&self, enabled: bool) {
        self.0.enable_logs.store(enabled, Ordering::Relaxed);
    }

    /// The loader's own (in-memory) tree.
    pub fn tree(&self) -> TreeHandle {
        TreeHandle { loader: self.clone(), id: self.0.root_tree }
    }

    pub fn root(&self) -> GroupHandle {
        self.tree().root()
    }

    pub async fn create(&self, options: Value, parent: Option<&str>, position: Option<usize>) -> Result<String> {
        self.tree().create(options, parent, position).await
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        self.tree().remove(id)
    }

    pub async fn update(
        &self,
        id: &str,
        patch: Value,
        parent: Option<Option<&str>>,
        position: Option<usize>,
    ) -> Result<()> {
        self.tree().update(id, patch, parent, position).await
    }

    pub fn resolve(&self, id: &str) -> Result<EntryHandle> {
        self.tree().resolve(id)
    }

    /// Every entry of every tree, depth first.
    pub fn entries(&self) -> Vec<EntryHandle> {
        self.tree().entries()
    }

    /// Whether some entry is still initializing.
    pub fn is_busy(&self) -> bool {
        self.0.pending.load(Ordering::SeqCst) > 0
    }

    /// Waits until no entry is initializing and every entry fiber has settled.
    pub async fn settled(&self) {
        loop {
            while self.is_busy() {
                tokio::task::yield_now().await;
            }
            let fibers: Vec<Fiber> = self.with(|st| {
                st.entries.values().flat_map(|entry| entry.outer.iter().chain(entry.inner.iter()).cloned()).collect()
            });
            for fiber in &fibers {
                let _ = fiber.settled().await;
            }
            tokio::task::yield_now().await;
            let busy = self.is_busy()
                || self.with(|st| {
                    st.entries.values().any(|entry| {
                        entry.init.is_some()
                            || entry.outer.iter().chain(entry.inner.iter()).any(|f| is_transitioning(f.state()))
                    })
                });
            if !busy {
                return;
            }
        }
    }

    /// Creates a tree owned by the plugin running in `ctx` (e.g. a file-backed include).
    pub fn create_tree(&self, ctx: &Context, backend: Arc<dyn TreeBackend>) -> TreeHandle {
        let (tree, group) = (self.next_id(), self.next_id());
        let owner = self.entry_of_context(ctx);
        let tree_ctx = ctx.extend();
        self.with(|st| {
            st.groups.insert(group, GroupData { ctx: tree_ctx.clone(), tree, data: Vec::new(), owner });
            st.trees.insert(tree, TreeData { ctx: tree_ctx, root: group, store: HashMap::new(), backend, owner });
            if let Some(entry) = owner.and_then(|id| st.entries.get_mut(&id)) {
                entry.subtree = Some(tree);
                entry.subgroup = Some(group);
            }
        });
        TreeHandle { loader: self.clone(), id: tree }
    }

    /// The entry a context belongs to (the nearest one on its derivation chain).
    pub fn entry_of(&self, ctx: &Context) -> Option<EntryHandle> {
        self.entry_of_context(ctx).map(|id| EntryHandle { loader: self.clone(), id })
    }

    pub(crate) fn entry_of_context(&self, ctx: &Context) -> Option<EntryId> {
        let tag = ctx.tag()?;
        self.with(|st| st.entries.contains_key(&tag).then_some(tag))
    }

    pub(crate) fn commit(&self, tree: TreeId, change: EntryChange) {
        let backend = self.with(|st| st.trees.get(&tree).map(|tree| tree.backend.clone()));
        if let Some(backend) = backend {
            backend.commit(&change);
        }
    }

    pub(crate) fn group_handle(&self, id: GroupId) -> GroupHandle {
        GroupHandle { loader: self.clone(), id }
    }

    /// Releases one pending entry; the last one re-checks the `await` barrier.
    pub(crate) fn release_pending(&self) {
        if self.0.pending.fetch_sub(1, Ordering::SeqCst) == 1
            && let Some(realm) = self.0.ctx.realm(Loader::NAME)
        {
            self.0.ctx.refresh_consumers(Loader::NAME, realm);
        }
    }

    pub(crate) fn show_log(&self, entry: EntryId, action: &str) {
        if !self.0.enable_logs.load(Ordering::Relaxed) {
            return;
        }
        let Some(options) = self.with(|st| st.entries.get(&entry).map(EntryData::read)) else { return };
        if !options.group {
            self.0.ctx.root().logger_named("loader").info(format_args!("{action} plugin {}", options.name));
        }
    }

    /// The loader's reaction to a fiber retiring: a plugin that disposed
    /// itself is persisted as `disabled: true`.
    fn on_removed(&self, fiber: &Fiber) {
        let Some(entry) = self.with(|st| st.inner_fibers.remove(&fiber.id())) else { return };
        // the entry's tree is being disposed
        let tree_ctx = self.with(|st| {
            let parent = st.entries.get(&entry)?.parent;
            Some(st.trees[&st.groups[&parent].tree].ctx.clone())
        });
        let Some(tree_ctx) = tree_ctx else { return };
        if tree_ctx.fiber().is_removed() {
            return;
        }
        self.show_log(entry, "unload");
        // the entry's parent is being unloaded, which takes the entry with it
        if fiber.parent().is_some_and(|parent| parent.state() == FiberState::Unloading) {
            return;
        }
        let change = self.with(|st| {
            let data = st.entries.get(&entry)?;
            // removed by the loader, or disposed through `entry.dispose()`
            if data.disposing || !st.registered(entry) {
                return None;
            }
            let legacy = data.options.clone();
            let mut options = legacy.clone();
            options.as_object_mut()?.insert("disabled".into(), Value::Bool(true));
            sort_keys(&mut options);
            st.entries.get_mut(&entry)?.verdict = Some(true);
            st.set_entry_options(entry, options.clone());
            let parent = st.entries[&entry].parent;
            Some((parent, legacy, options, st.entries[&entry].outer.is_some()))
        });
        let Some((group, legacy, options, has_outer)) = change else { return };
        let tree = self.with(|st| st.groups[&group].tree);
        let id = EntryOptions::read(&options).id;
        self.commit(
            tree,
            EntryChange {
                id,
                group: self.group_handle(group),
                from: None,
                options: Some(options),
                legacy: Some(legacy),
            },
        );
        if has_outer {
            drop(self.dispose_entry(entry));
        }
    }

    /// A plugin updated its own configuration: write it back to its entry.
    fn on_committed(&self, fiber: &Fiber, config: &Value) {
        let change = self.with(|st| {
            let entry = *st.inner_fibers.get(&fiber.id())?;
            let legacy = st.entries.get(&entry)?.options.clone();
            let mut options = legacy.clone();
            options.as_object_mut()?.insert("config".into(), config.clone());
            sort_keys(&mut options);
            st.set_entry_options(entry, options.clone());
            Some((st.entries[&entry].parent, legacy, options))
        });
        let Some((group, legacy, options)) = change else { return };
        let tree = self.with(|st| st.groups[&group].tree);
        let id = EntryOptions::read(&options).id;
        self.commit(
            tree,
            EntryChange {
                id,
                group: self.group_handle(group),
                from: None,
                options: Some(options),
                legacy: Some(legacy),
            },
        );
    }

    /// Dependency loss unloads an outer fiber without `dispose`; the entry
    /// then reads as disabled until its outer fiber is reactivated.
    fn on_status(&self, fiber: &Fiber, new: FiberState) {
        if new != FiberState::Unloading {
            return;
        }
        self.with(|st| {
            if let Some(entry) = st.outer_fibers.get(&fiber.id()).copied()
                && let Some(data) = st.entries.get_mut(&entry)
                && data.outer.as_ref().is_some_and(|outer| outer.id() == fiber.id())
            {
                data.verdict = None;
            }
        });
    }
}

fn is_transitioning(state: FiberState) -> bool {
    matches!(state, FiberState::Loading | FiberState::Unloading)
}

impl LoaderState {
    pub(crate) fn registered(&self, entry: EntryId) -> bool {
        let Some(data) = self.entries.get(&entry) else { return false };
        let tree = self.groups[&data.parent].tree;
        self.trees[&tree].store.get(&data.local_id()) == Some(&entry)
    }

    /// Replaces an entry's options, in the entry and in its group's data.
    pub(crate) fn set_entry_options(&mut self, entry: EntryId, options: Value) {
        let Some(data) = self.entries.get_mut(&entry) else { return };
        let id = EntryOptions::read(&options).id;
        data.options = options.clone();
        let group = data.parent;
        if let Some(slot) = self.groups.get_mut(&group).and_then(|g| g.data.iter_mut().find(|o| o["id"] == id.as_str()))
        {
            *slot = options;
        }
        self.sync_owner(group);
    }

    /// A group plugin's data is the `config` of its own entry: mirror it up.
    pub(crate) fn sync_owner(&mut self, group: GroupId) {
        let Some(data) = self.groups.get(&group) else { return };
        let (tree, list) = (data.tree, Value::Array(data.data.clone()));
        let Some(owner) = data.owner else { return };
        let Some(owner_data) = self.entries.get(&owner) else { return };
        if owner_data.subgroup != Some(group) || self.groups.get(&owner_data.parent).map(|g| g.tree) != Some(tree) {
            return;
        }
        let mut options = owner_data.options.clone();
        if let Some(map) = options.as_object_mut() {
            map.insert("config".into(), list);
        }
        self.set_entry_options(owner, options);
    }

    pub(crate) fn set_group_data(&mut self, group: GroupId, data: Vec<Value>) {
        if let Some(target) = self.groups.get_mut(&group) {
            target.data = data;
        }
        self.sync_owner(group);
    }

    /// The composite id of an entry across nested trees.
    pub(crate) fn composite_id(&self, entry: EntryId) -> String {
        let data = &self.entries[&entry];
        let tree = &self.trees[&self.groups[&data.parent].tree];
        match tree.owner.filter(|owner| self.entries.contains_key(owner)) {
            Some(owner) => format!("{}{SEP}{}", self.composite_id(owner), data.local_id()),
            None => data.local_id(),
        }
    }

    /// Assigns a fresh random id to options without one.
    pub(crate) fn ensure_id(&self, tree: TreeId, options: &mut Value) -> String {
        if let Some(id) = options.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()) {
            return id.to_string();
        }
        let store = &self.trees[&tree].store;
        let id = loop {
            let id = format!("{:08x}", RandomState::new().hash_one(store.len()) as u32);
            if !store.contains_key(&id) {
                break id;
            }
        };
        if let Some(map) = options.as_object_mut() {
            map.insert("id".into(), Value::String(id.clone()));
        }
        sort_keys(options);
        id
    }
}

impl EntryHandle {
    /// The composite id (`include:inner`).
    pub fn id(&self) -> String {
        self.loader.with(|st| st.composite_id(self.id))
    }

    pub fn options(&self) -> Value {
        self.loader.with(|st| st.entries.get(&self.id).map(|e| e.options.clone()).unwrap_or_default())
    }

    pub fn inner_fiber(&self) -> Option<Fiber> {
        self.loader.with(|st| st.entries.get(&self.id)?.inner.clone())
    }

    pub fn outer_fiber(&self) -> Option<Fiber> {
        self.loader.with(|st| st.entries.get(&self.id)?.outer.clone())
    }

    /// Whether the entry is (or reads as) disabled, including through its ancestors.
    pub fn is_disabled(&self) -> bool {
        self.loader.with(|st| st.is_disabled(self.id))
    }

    pub fn subgroup(&self) -> Option<GroupHandle> {
        let id = self.loader.with(|st| st.entries.get(&self.id)?.subgroup)?;
        Some(self.loader.group_handle(id))
    }

    pub fn subtree(&self) -> Option<TreeHandle> {
        let id = self.loader.with(|st| st.entries.get(&self.id)?.subtree)?;
        Some(TreeHandle { loader: self.loader.clone(), id })
    }

    pub fn parent(&self) -> GroupHandle {
        let id = self.loader.with(|st| st.entries[&self.id].parent);
        self.loader.group_handle(id)
    }
}

impl GroupHandle {
    /// The entry whose plugin owns this group (none for the loader's root group).
    pub fn owner(&self) -> Option<EntryHandle> {
        let id = self.loader.with(|st| st.groups.get(&self.id)?.owner)?;
        Some(EntryHandle { loader: self.loader.clone(), id })
    }

    /// The group's entry options, in order.
    pub fn data(&self) -> Vec<Value> {
        self.loader.with(|st| st.groups.get(&self.id).map(|g| g.data.clone()).unwrap_or_default())
    }

    pub fn tree(&self) -> TreeHandle {
        let id = self.loader.with(|st| st.groups[&self.id].tree);
        TreeHandle { loader: self.loader.clone(), id }
    }

    pub fn context(&self) -> Context {
        self.loader.with(|st| st.groups[&self.id].ctx.clone())
    }

    /// Keyed reconciliation of the group's entries with `config`.
    pub async fn update(&self, config: Vec<Value>) {
        self.loader.group_update(self.id, config).await;
    }

    /// Removes every entry of the group, keeping its data.
    pub fn stop(&self) {
        self.loader.group_stop(self.id);
    }
}

impl TreeHandle {
    /// The directory relative paths in this tree resolve against: the
    /// backend's, else the enclosing tree's, else the working directory.
    pub fn base_dir(&self) -> std::path::PathBuf {
        let (backend, owner) = self.loader.with(|st| (st.trees[&self.id].backend.clone(), st.trees[&self.id].owner));
        if let Some(dir) = backend.base_dir() {
            return dir;
        }
        let parent = owner.map(|owner| EntryHandle { loader: self.loader.clone(), id: owner });
        match parent.and_then(|entry| self.loader.with(|st| st.entries.contains_key(&entry.id)).then_some(entry)) {
            Some(entry) => entry.parent().tree().base_dir(),
            None => std::env::current_dir().unwrap_or_default(),
        }
    }

    /// See [`TreeBackend::handle`].
    pub fn handle(&self) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        self.loader.with(|st| st.trees.get(&self.id).map(|tree| tree.backend.clone()))?.handle()
    }

    pub fn root(&self) -> GroupHandle {
        let id = self.loader.with(|st| st.trees[&self.id].root);
        self.loader.group_handle(id)
    }

    pub fn context(&self) -> Context {
        self.loader.with(|st| st.trees[&self.id].ctx.clone())
    }

    pub fn entries(&self) -> Vec<EntryHandle> {
        let ids = self.loader.with(|st| st.tree_entries(self.id));
        ids.into_iter().map(|id| EntryHandle { loader: self.loader.clone(), id }).collect()
    }

    pub fn resolve(&self, id: &str) -> Result<EntryHandle> {
        let entry = self.loader.with(|st| st.resolve(self.id, id))?;
        Ok(EntryHandle { loader: self.loader.clone(), id: entry })
    }

    /// See [`TreeBackend::locate`]; `id` is a local id of this tree.
    pub fn locate(&self, id: &str, path: &[PathSegment], value: Option<&Value>) -> Option<Location> {
        self.backend()?.locate(id, path, value)
    }

    /// See [`TreeBackend::locate_expr`]; `id` is a local id of this tree.
    pub fn locate_expr(&self, id: &str, path: &[PathSegment], expr: &str, offset: usize) -> Option<Location> {
        self.backend()?.locate_expr(id, path, expr, offset)
    }

    fn backend(&self) -> Option<Arc<dyn TreeBackend>> {
        self.loader.with(|st| st.trees.get(&self.id).map(|tree| tree.backend.clone()))
    }

    /// The group of entry `parent` (the root group for `None`). A group
    /// plugin creates its group when it loads, so a loading group is awaited.
    async fn resolve_group(&self, parent: Option<&str>) -> Result<GroupId> {
        let Some(id) = parent else { return Ok(self.loader.with(|st| st.trees[&self.id].root)) };
        let entry = self.loader.with(|st| st.resolve(self.id, id))?;
        let (group, inner) = self.loader.with(|st| (st.entries[&entry].subgroup, st.entries[&entry].inner.clone()));
        if let Some(group) = group {
            return Ok(group);
        }
        if let Some(inner) = inner {
            let _ = inner.settled().await;
        }
        self.loader
            .with(|st| st.entries.get(&entry)?.subgroup)
            .ok_or_else(|| Error::msg(format!("entry {id} is not a group")))
    }

    /// Creates an entry (validated deeply before anything is committed).
    pub async fn create(&self, mut options: Value, parent: Option<&str>, position: Option<usize>) -> Result<String> {
        let group = self.resolve_group(parent).await?;
        let issues = validate_options(&options, true);
        if !issues.is_empty() {
            return Err(EntryValidationError { issues }.into());
        }
        let (tree, id) = self.loader.with(|st| {
            let tree = st.groups[&group].tree;
            let id = st.ensure_id(tree, &mut options);
            let mut data = st.groups[&group].data.clone();
            data.insert(position.unwrap_or(data.len()).min(data.len()), options.clone());
            st.set_group_data(group, data);
            (tree, id)
        });
        let change = EntryChange {
            id,
            group: self.loader.group_handle(group),
            from: None,
            options: Some(options.clone()),
            legacy: None,
        };
        self.loader.commit(tree, change);
        self.loader.group_create(group, options).await
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        let entry = self.loader.with(|st| st.resolve(self.id, id))?;
        let (group, legacy) = self.loader.with(|st| (st.entries[&entry].parent, st.entries[&entry].options.clone()));
        let local = EntryOptions::read(&legacy).id;
        self.loader.group_remove(group, &local, false);
        let tree = self.loader.with(|st| st.groups[&group].tree);
        let change = EntryChange {
            id: local,
            group: self.loader.group_handle(group),
            from: None,
            options: None,
            legacy: Some(legacy),
        };
        self.loader.commit(tree, change);
        Ok(())
    }

    /// Updates an entry; `parent: Some(..)` moves it (within or across trees).
    pub async fn update(
        &self,
        id: &str,
        patch: Value,
        parent: Option<Option<&str>>,
        position: Option<usize>,
    ) -> Result<()> {
        let entry = self.loader.with(|st| st.resolve(self.id, id))?;
        let patch = patch.as_object().cloned().unwrap_or_default();
        let (source, legacy) = self.loader.with(|st| (st.entries[&entry].parent, st.entries[&entry].options.clone()));
        let mut options = legacy.clone();
        merge(&mut options, &patch);
        sort_keys(&mut options);
        let issues = validate_options(&options, true);
        if !issues.is_empty() {
            return Err(EntryValidationError { issues }.into());
        }
        let target = match parent {
            Some(parent) => Some(self.resolve_group(parent).await?),
            None => None,
        };
        let source_tree = self.loader.with(|st| st.groups[&source].tree);
        self.loader.with(|st| {
            if let Some(target) = target {
                st.move_entry(entry, source, target, position)?;
            }
            st.set_entry_options(entry, options.clone());
            Ok::<_, Error>(())
        })?;
        let task = self.loader.entry_update(entry, legacy.clone(), true);
        let (group, target_tree) =
            self.loader.with(|st| (st.entries[&entry].parent, st.groups[&st.entries[&entry].parent].tree));
        let local = EntryOptions::read(&legacy).id;
        if target_tree != source_tree {
            let removal = EntryChange {
                id: local.clone(),
                group: self.loader.group_handle(source),
                from: None,
                options: None,
                legacy: Some(legacy),
            };
            self.loader.commit(source_tree, removal);
            let creation = EntryChange {
                id: local,
                group: self.loader.group_handle(group),
                from: None,
                options: Some(options),
                legacy: None,
            };
            self.loader.commit(target_tree, creation);
        } else {
            let from = (group != source).then(|| self.loader.group_handle(source));
            let change = EntryChange {
                id: local,
                group: self.loader.group_handle(group),
                from,
                options: Some(options),
                legacy: Some(legacy),
            };
            self.loader.commit(target_tree, change);
        }
        task.await
    }
}

impl LoaderState {
    pub(crate) fn resolve(&self, tree: TreeId, id: &str) -> Result<EntryId> {
        let fail = || Error::msg(format!("cannot resolve entry {id}"));
        let mut parts: Vec<&str> = id.split(SEP).collect();
        let last = parts.pop().ok_or_else(fail)?;
        let mut tree = tree;
        for part in parts {
            let entry = self.trees[&tree].store.get(part).ok_or_else(fail)?;
            tree = self.entries[entry].subtree.ok_or_else(fail)?;
        }
        self.trees[&tree].store.get(last).copied().ok_or_else(fail)
    }

    pub(crate) fn tree_entries(&self, tree: TreeId) -> Vec<EntryId> {
        let mut result = Vec::new();
        let mut ids: Vec<_> = self.trees[&tree].store.values().copied().collect();
        ids.sort();
        for id in ids {
            result.push(id);
            if let Some(subtree) = self.entries[&id].subtree.filter(|subtree| self.trees.contains_key(subtree)) {
                result.extend(self.tree_entries(subtree));
            }
        }
        result
    }

    /// The entry and the entries of its group plugin, recursively, within one tree.
    fn members(&self, entry: EntryId) -> Vec<EntryId> {
        let mut result = vec![entry];
        let tree = self.groups[&self.entries[&entry].parent].tree;
        let Some(group) = self.entries[&entry].subgroup else { return result };
        if self.groups.get(&group).map(|g| g.tree) != Some(tree) {
            return result;
        }
        for options in &self.groups[&group].data {
            let id = EntryOptions::read(options).id;
            if let Some(member) = self.trees[&tree].store.get(&id)
                && self.entries[member].parent == group
            {
                result.extend(self.members(*member));
            }
        }
        result
    }

    fn move_entry(&mut self, entry: EntryId, source: GroupId, target: GroupId, position: Option<usize>) -> Result<()> {
        let (source_tree, target_tree) = (self.groups[&source].tree, self.groups[&target].tree);
        let moved = if source_tree == target_tree { Vec::new() } else { self.members(entry) };
        for member in &moved {
            let id = self.entries[member].local_id();
            if self.trees[&target_tree].store.contains_key(&id) {
                return Err(Error::msg(format!("entry {id} already exists in the target tree")));
            }
        }
        let options = self.entries[&entry].options.clone();
        let id = EntryOptions::read(&options).id;
        let mut data = self.groups[&source].data.clone();
        data.retain(|o| o["id"] != id.as_str());
        self.set_group_data(source, data);
        let mut data = self.groups[&target].data.clone();
        data.insert(position.unwrap_or(data.len()).min(data.len()), options);
        self.set_group_data(target, data);
        self.entries.get_mut(&entry).expect("resolved").parent = target;
        // the target tree owns the moved entries from now on
        for member in moved {
            let id = self.entries[&member].local_id();
            self.trees.get_mut(&source_tree).expect("exists").store.remove(&id);
            self.trees.get_mut(&target_tree).expect("exists").store.insert(id, member);
            if let Some(group) = self.entries[&member].subgroup
                && self.groups[&group].tree == source_tree
            {
                self.groups.get_mut(&group).expect("exists").tree = target_tree;
            }
        }
        Ok(())
    }

    pub(crate) fn new_entry(&mut self, id: EntryId, loader_ctx: &Context, parent: GroupId) {
        let overlay = Overlay::tagged(id);
        let ctx = loader_ctx.extend_with(&overlay);
        self.entries.insert(
            id,
            EntryData {
                parent,
                options: Value::Object(Map::new()),
                ctx,
                overlay,
                intercept: Overlay::tagged(id),
                outer: None,
                inner: None,
                subgroup: None,
                subtree: None,
                inject: Inject::new(),
                init: None,
                disposing: false,
                verdict: None,
                local_realms: HashMap::new(),
            },
        );
    }
}
