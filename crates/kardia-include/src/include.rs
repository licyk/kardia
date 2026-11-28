use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use futures::future::{BoxFuture, Shared};
use futures::{FutureExt, TryStreamExt};

use kardia_core::{Context, Disposer, Error, Inject, PathSegment, Plugin, PluginDef, Result};
use kardia_fs::{Fs, FsExt, Provider, Url, WatchOptions};
use kardia_loader::{EXPR_KEY, EntryChange, Loader, Location, TreeBackend, TreeHandle, expr_source, value_at};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::Value;

use crate::journal::{Journal, apply_journal, merge, reconcile, record};
use crate::patch::{
    Owner, PatchIndex, Patched, apply_patches, ensure_ids, ensure_insert_ids, random_id, route_journal,
};
use crate::tree::{find, flatten, group_list, id_of};
use crate::yaml::{self, Position, Syntax};

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncludeConfig {
    pub path: String,
    /// Written if the file does not exist yet.
    #[serde(default)]
    pub initial: Option<Vec<Value>>,
    #[serde(default)]
    pub patches: Vec<Value>,
    /// Re-read the file whenever it changes (needs the `fs` service).
    #[serde(default)]
    pub watch: bool,
}

/// Grafts the entries of a YAML / JSON file into the loader as a subtree,
/// and writes runtime changes of those entries back to the file.
pub struct IncludePlugin;

impl Plugin for IncludePlugin {
    const NAME: &'static str = "include";
    type Config = IncludeConfig;

    fn inject() -> Inject {
        Inject::new().with(<Loader as kardia_core::Service>::NAME).with("std:fs")
    }

    async fn apply(ctx: Context, config: Arc<IncludeConfig>) -> Result<()> {
        Include::run(ctx, (*config).clone()).await
    }
}

impl IncludePlugin {
    /// The plugin definition, for a host that registers it under a name.
    pub fn def() -> PluginDef {
        PluginDef::of::<Self>()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Read,
    Parse,
    Validate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigIssue {
    pub message: String,
    pub position: Position,
}

/// A config file that could not be read, parsed or validated.
#[derive(Debug, Clone, thiserror::Error)]
pub struct ConfigFileError {
    pub stage: Stage,
    pub path: PathBuf,
    pub cause: Option<String>,
    pub issues: Vec<ConfigIssue>,
    not_found: bool,
}

impl fmt::Display for ConfigFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let stage = match self.stage {
            Stage::Read => "read",
            Stage::Parse => "parse",
            Stage::Validate => "validate",
        };
        write!(f, "failed to {stage} config file {}", self.path.display())?;
        if let Some(cause) = &self.cause {
            write!(f, ": {cause}")?;
        }
        for issue in &self.issues {
            let Position { line, column } = issue.position;
            write!(f, "\n  - {} (at {}:{line}:{column})", issue.message, self.path.display())?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Json,
    Yaml,
}

/// The file changed between the pre-check and the rename.
#[derive(Debug, thiserror::Error)]
#[error("the config file changed while it was being written")]
struct StaleWrite;

struct Snapshot {
    content: String,
    /// Parsed, ids assigned, unpatched.
    data: Vec<Value>,
    syntax: Syntax,
}

struct Anonymous {
    id: String,
    parent: Option<String>,
    options: Value,
}

type Task = Shared<BoxFuture<'static, ()>>;

#[derive(Default)]
struct State {
    patches: Vec<Value>,
    cache: Option<Snapshot>,
    journal: Journal,
    /// The batch being written: out of the journal, not yet in the cache.
    /// Trees applied meanwhile include it, so that it does not vanish from
    /// the running tree while the write is in flight.
    writing: Journal,
    index: PatchIndex,
    anonymous: Vec<Anonymous>,
    dirty_read: bool,
    dirty_write: bool,
    draining: Option<Task>,
    /// Only the latest intended tree is applied; older ones are skipped.
    pending: Option<Vec<Value>>,
    applying: Option<Task>,
    disposed: bool,
}

/// A running include.
#[derive(Clone)]
pub struct Include(Arc<Inner>);

struct Inner {
    ctx: Context,
    loader: Loader,
    tree: OnceLock<TreeHandle>,
    filename: PathBuf,
    /// The same file as a location of `std:fs`, through which the content is
    /// read and written, so a provider that is not the local disk works too.
    url: Url,
    /// The provider of the file, captured while the plugin is active. The write
    /// that flushes pending changes runs during disposal, when a mount another
    /// plugin owns may already be gone; this handle keeps the provider alive.
    provider: Provider,
    kind: Kind,
    watch: bool,
    state: Mutex<State>,
    seq: AtomicU64,
}

const RENAME_RETRIES: u32 = 10;
const RENAME_BACKOFF: Duration = Duration::from_millis(20);

impl Include {
    async fn run(ctx: Context, config: IncludeConfig) -> Result<()> {
        let loader = (*ctx.get::<Loader>()?).clone();
        let fs = ctx.get::<Fs>()?;
        // The write that flushes pending changes runs while the plugin unloads,
        // which is after a mount another plugin owns may be gone. The provider is
        // captured now and kept by the entry, so reading and writing never depend
        // on the mount still being listed.
        let url = fs.resolve(&config.path)?;
        let provider = fs.provider_of(&url)?;
        let filename = url
            .to_file_path()
            .map_err(|_| kardia_core::ValidationError::message("include.path must resolve to a local file URL"))?;
        let kind = match filename.extension().and_then(|ext| ext.to_str()) {
            Some("json") => Kind::Json,
            Some("yaml" | "yml") => Kind::Yaml,
            ext => return Err(Error::msg(format!("extension \"{}\" not supported", ext.unwrap_or_default()))),
        };
        let base = url.join(".")?;
        let mut patches = config.patches.clone();
        ensure_insert_ids(&mut patches, &mut |_| false);
        let include = Include(Arc::new(Inner {
            ctx: ctx.clone(),
            loader: loader.clone(),
            tree: OnceLock::new(),
            filename,
            url,
            provider,
            kind,
            watch: config.watch,
            state: Mutex::new(State { patches, ..Default::default() }),
            seq: AtomicU64::new(0),
        }));
        let tree_ctx = ctx.intercept("std:fs", serde_json::json!({"baseUrl": base.as_str()}));
        let tree = loader.create_tree(&tree_ctx, Arc::new(Backend(Arc::downgrade(&include.0))));
        let _ = include.0.tree.set(tree);

        // new patches are applied in place; another path or watch mode restarts
        let updates = include.clone();
        let path = config.path.clone();
        ctx.on_update_with(move |next: &IncludeConfig| {
            if next.path != path || next.watch != updates.0.watch {
                return None;
            }
            let mut patches = next.patches.clone();
            ensure_insert_ids(&mut patches, &mut |_| false);
            let mut state = updates.0.state.lock();
            state.patches = patches;
            updates.schedule_apply(&mut state);
            let applying = state.applying.clone();
            Some(
                async move {
                    if let Some(applying) = applying {
                        applying.await;
                    }
                    Ok(())
                }
                .boxed(),
            )
        })?;
        if config.watch {
            let watcher = include.clone();
            ctx.inject(Inject::of::<(Fs,)>(), move |ctx| {
                let include = watcher.clone();
                async move {
                    let watch = ctx.get::<Fs>()?.watch(&include.0.filename, WatchOptions::default())?;
                    watch.on_change(move |_| {
                        let include = include.clone();
                        tokio::spawn(async move { include.refresh().await });
                    });
                    Ok(())
                }
            })?;
        }

        if let Err(error) = include.read().await {
            // only a missing file falls back to `initial`
            if !error.not_found {
                return Err(error.into());
            }
            let Some(mut initial) = config.initial.clone() else {
                return Err(Error::msg(format!("config file not found: {}", include.0.filename.display())));
            };
            ensure_ids(&mut initial, &mut |_| false);
            include.write_text(&include.dump(&initial), None).await?;
            include.read().await?;
        }
        let stopping = include.clone();
        ctx.defer("include", Disposer::future(move || async move { stopping.stop().await }))?;
        let applying = include.0.state.lock().applying.clone();
        if let Some(applying) = applying {
            applying.await;
        }
        Ok(())
    }

    /// The include behind a tree it created.
    pub fn of(tree: &TreeHandle) -> Option<Include> {
        tree.handle()?.downcast::<Inner>().ok().map(Include)
    }

    pub fn filename(&self) -> &Path {
        &self.0.filename
    }

    pub fn tree(&self) -> &TreeHandle {
        self.0.tree.get().expect("set at start")
    }

    /// Re-reads the file and reconciles the tree with it (what a watcher
    /// triggers), then waits for every pending read and write.
    pub async fn refresh(&self) {
        self.0.state.lock().dirty_read = true;
        self.drain();
        self.flush().await;
    }

    /// Resolves once every pending read and write has been processed and
    /// the tree reflects the outcome.
    pub async fn flush(&self) {
        loop {
            let (draining, applying) = {
                let state = self.0.state.lock();
                (state.draining.clone(), state.applying.clone())
            };
            if draining.is_none() && applying.is_none() {
                return;
            }
            if let Some(draining) = draining {
                draining.await;
            }
            if let Some(applying) = applying {
                applying.await;
            }
        }
    }

    /// Flushes what the user last did, then removes the tree's entries.
    pub async fn stop(&self) {
        {
            let mut state = self.0.state.lock();
            state.disposed = true;
            state.pending = None;
            if !state.journal.is_empty() {
                state.dirty_write = true;
            }
        }
        // A drain that was already finishing may stop before it sees the write
        // requested above, so the pending write is checked again after each pass.
        loop {
            self.drain();
            self.flush().await;
            let state = self.0.state.lock();
            if !state.dirty_write && !state.dirty_read {
                break;
            }
        }
        self.tree().root().stop();
    }

    /// Called synchronously by the loader once a tree-side change is in place.
    fn commit(&self, change: &EntryChange) {
        let parent = (change.group != self.tree().root())
            .then(|| change.group.owner().and_then(|owner| id_of(&owner.options()).map(String::from)))
            .flatten();
        let position = change.group.data().iter().position(|options| id_of(options) == Some(&change.id)).unwrap_or(0);
        {
            let mut state = self.0.state.lock();
            let moved = change.from.is_some();
            record(
                &mut state.journal,
                &change.id,
                change.options.as_ref(),
                change.legacy.as_ref(),
                moved,
                parent,
                position,
            );
            state.dirty_write = true;
        }
        self.drain();
    }

    fn drain(&self) {
        let mut state = self.0.state.lock();
        if state.draining.is_some() {
            return;
        }
        let this = self.clone();
        let task = tokio::spawn(async move { this.drain_loop().await });
        state.draining = Some(async move { drop(task.await) }.boxed().shared());
    }

    async fn drain_loop(&self) {
        loop {
            let (read, write) = {
                let mut state = self.0.state.lock();
                if !state.dirty_read && !state.dirty_write {
                    // checked and released together, so a new trigger starts a new drain
                    state.draining = None;
                    return;
                }
                (std::mem::take(&mut state.dirty_read), state.dirty_write)
            };
            if read && let Err(error) = self.read().await {
                // half-written files are routine while editing; with the file in an
                // unknown state nothing is written until another read succeeds
                self.0.ctx.logger().warn(&error);
                let mut state = self.0.state.lock();
                if !state.dirty_read {
                    state.draining = None;
                    return;
                }
                continue;
            }
            if write {
                self.0.state.lock().dirty_write = false;
                if let Err(error) = self.write().await {
                    self.0.ctx.logger().log_error(&error);
                }
            }
        }
    }

    /// The current content of the file, read through `std:fs`.
    async fn read_text(&self) -> Result<String> {
        let stream = self.0.provider.read(self.0.url.clone());
        let bytes: Vec<u8> = stream
            .try_fold(Vec::new(), |mut all, chunk| async move {
                all.extend_from_slice(&chunk);
                Ok(all)
            })
            .await?;
        Ok(String::from_utf8(bytes)?)
    }

    async fn read(&self) -> Result<(), ConfigFileError> {
        let content = self.read_text().await.map_err(|error| ConfigFileError {
            stage: Stage::Read,
            path: self.0.filename.clone(),
            cause: Some(
                if error.downcast_ref::<kardia_fs::FsError>().is_some_and(|e| e.code == kardia_fs::FsCode::NotFound) {
                    "not found".to_owned()
                } else {
                    error.to_string()
                },
            ),
            issues: Vec::new(),
            not_found: error
                .downcast_ref::<kardia_fs::FsError>()
                .is_some_and(|e| e.code == kardia_fs::FsCode::NotFound),
        })?;
        if self.0.state.lock().cache.as_ref().is_some_and(|cache| cache.content == content) {
            return Ok(());
        }
        let (mut data, syntax) = self.parse(&content)?;
        let mut guard = self.0.state.lock();
        let state = &mut *guard;
        self.assign_ids(state, &mut data);
        if let Some(cache) = &state.cache {
            let index = apply_patches(&data, &mut state.patches, &|_| {}).index;
            let (base, theirs) = (flatten(&cache.data), flatten(&data));
            let conflicts = reconcile(&mut state.journal, &base, &theirs, |id, key| index.file_owned(id, key));
            for conflict in conflicts {
                let file = self.0.filename.display();
                self.0.ctx.logger().error(format_args!(
                    "config conflict in {file}: entry {} {}; file wins",
                    conflict.id, conflict.reason
                ));
            }
        }
        state.cache = Some(Snapshot { content, data, syntax });
        self.schedule_apply(state);
        // the file changed while tree-side changes were pending: write the merge
        if !state.journal.is_empty() {
            state.dirty_write = true;
        }
        Ok(())
    }

    fn parse(&self, content: &str) -> Result<(Vec<Value>, Syntax), ConfigFileError> {
        let error = |stage, issues| ConfigFileError {
            stage,
            path: self.0.filename.clone(),
            cause: None,
            issues,
            not_found: false,
        };
        let parsed = match self.0.kind {
            Kind::Yaml => {
                yaml::parse(content).map_err(|e| vec![ConfigIssue { message: e.message, position: e.position }])
            }
            Kind::Json => match serde_json::from_str::<Value>(content) {
                Ok(data) => Ok((data, json_syntax(content))),
                Err(e) => Err(vec![ConfigIssue {
                    message: e.to_string().split(" at line").next().unwrap_or_default().to_string(),
                    position: Position { line: e.line(), column: e.column() },
                }]),
            },
        };
        let (data, syntax) = parsed.map_err(|issues| error(Stage::Parse, issues))?;
        // an empty or truncated file (routine mid-edit) is no array either
        let Value::Array(data) = data else {
            let position = syntax.locate(&[]).unwrap_or(Position { line: 1, column: 1 });
            let issue = ConfigIssue { message: "config file must be a top-level array".into(), position };
            return Err(error(Stage::Validate, vec![issue]));
        };
        // ids, patches and the journal all assume an entry is an object
        let mut issues = Vec::new();
        find_invalid(&data, &mut Vec::new(), &mut |path| {
            let position = syntax.locate(path).unwrap_or(Position { line: 1, column: 1 });
            issues.push(ConfigIssue { message: "entry must be an object".into(), position });
        });
        if !issues.is_empty() {
            return Err(error(Stage::Validate, issues));
        }
        Ok((data, syntax))
    }

    async fn write(&self) -> Result<()> {
        let expected = {
            let state = self.0.state.lock();
            match &state.cache {
                Some(cache) if !state.journal.is_empty() => cache.content.clone(),
                _ => return Ok(()),
            }
        };
        let current = match self.read_text().await {
            Ok(current) => current,
            Err(error)
                if error
                    .downcast_ref::<kardia_fs::FsError>()
                    .is_some_and(|e| e.code == kardia_fs::FsCode::NotFound) =>
            {
                let pending = self.0.state.lock().journal.len();
                let file = self.0.filename.display();
                self.0
                    .ctx
                    .logger()
                    .warn(format_args!("config file {file} is missing, {pending} pending change(s) kept in memory"));
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if current != expected {
            // someone else wrote the file: merge first, then come back
            let mut state = self.0.state.lock();
            state.dirty_read = true;
            state.dirty_write = true;
            return Ok(());
        }
        // snapshot and swap: changes reported meanwhile belong to the next round
        let (batch, mut data, mut patches) = {
            let mut state = self.0.state.lock();
            state.writing = std::mem::take(&mut state.journal);
            let cache = state.cache.as_ref().expect("checked above");
            (state.writing.clone(), cache.data.clone(), state.patches.clone())
        };
        let warn = |message: String| self.warn(message);
        // the routed patches take effect together with the written file: the
        // batch's records are diffs against the old file and the old patches
        let patched = route_journal(&batch, &mut data, &mut patches, &warn).then_some(patches);
        let unchanged = self.0.state.lock().cache.as_ref().is_some_and(|cache| cache.data == data);
        if unchanged {
            // patch-only changes, or ids assigned to anonymous entries
            self.commit_batch(None, patched);
            return Ok(());
        }
        let text = self.dump(&data);
        // A provider that reports a mode says whether the file is writable;
        // one that does not leaves the attempt to the write itself.
        let read_only = match self.0.provider.stat(self.0.url.clone()).await {
            Ok(stats) => stats.mode.is_some_and(|mode| mode & 0o200 == 0),
            Err(_) => false,
        };
        if read_only {
            let pending = self.restore();
            let file = self.0.filename.display();
            self.0
                .ctx
                .logger()
                .warn(format_args!("config file {file} is read-only, {pending} pending change(s) kept in memory"));
            return Ok(());
        }
        if let Err(error) = self.write_text(&text, Some(&expected)).await {
            let pending = self.restore();
            if error.is::<StaleWrite>() {
                let mut state = self.0.state.lock();
                state.dirty_read = true;
                state.dirty_write = true;
                return Ok(());
            }
            let file = self.0.filename.display();
            self.0
                .ctx
                .logger()
                .warn(format_args!("failed to write config file {file}, {pending} pending change(s) kept in memory"));
            return Err(error);
        }
        let syntax = json_syntax(&text);
        self.commit_batch(Some(Snapshot { content: text, data, syntax }), patched);
        Ok(())
    }

    /// Takes in the written batch: the new file content and patches replace
    /// the batch at once, so no tree sees one without the other.
    fn commit_batch(&self, cache: Option<Snapshot>, patches: Option<Vec<Value>>) {
        {
            let mut state = self.0.state.lock();
            if let Some(cache) = cache {
                state.cache = Some(cache);
            }
            if let Some(patches) = &patches {
                state.patches = patches.clone();
            }
            state.writing.clear();
            self.schedule_apply(&mut state);
        }
        if let Some(patches) = patches {
            self.persist_patches(patches);
        }
    }

    /// Puts the unwritten batch back in front of the changes reported since.
    fn restore(&self) -> usize {
        let mut state = self.0.state.lock();
        let mut batch = std::mem::take(&mut state.writing);
        for (id, record) in std::mem::take(&mut state.journal) {
            merge(&mut batch, &id, record);
        }
        state.journal = batch;
        state.journal.len()
    }

    /// Changes to patch-owned values are persisted through the include's own
    /// configuration, i.e. into the parent tree (not awaited: that is the
    /// tree reconciliation, which the drain loop must not block on).
    fn persist_patches(&self, patches: Vec<Value>) {
        let mut config = self.0.ctx.fiber().config();
        if let Some(map) = config.as_object_mut() {
            map.insert("patches".into(), Value::Array(patches));
        }
        if self.0.loader.entry_of(&self.0.ctx).is_none() {
            let file = self.0.filename.display();
            return self.warn(format!("patches of {file} changed at runtime but cannot be persisted"));
        }
        match self.0.ctx.fiber().update(config) {
            Ok(settle) => {
                let ctx = self.0.ctx.clone();
                tokio::spawn(async move {
                    if let Err(error) = settle.await {
                        ctx.logger().log_error(&error);
                    }
                });
            }
            Err(error) => self.0.ctx.logger().log_error(&error),
        }
    }

    /// Writes through a temp file and a rename, so that no reader sees a
    /// truncated file. With `expected`, the rename (and every retry of it)
    /// only goes ahead while the file still holds that content.
    async fn write_text(&self, text: &str, expected: Option<&str>) -> Result<()> {
        let name = self.0.filename.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let seq = self.0.seq.fetch_add(1, Ordering::Relaxed);
        // The temporary name holds only the characters a URL path segment keeps
        // literal: the original name is percent-encoded already, and the rest is
        // `.`, digits and `tmp`.
        let tmp_name = format!(".{name}.{}.{seq}.tmp", std::process::id());
        let tmp = self.0.url.join(&tmp_name)?;
        let provider = self.0.provider.clone();
        let result = async {
            provider.write(tmp.clone(), text.as_bytes().to_vec()).await?;
            for attempt in 0.. {
                if let Some(expected) = expected {
                    let current = match self.read_text().await {
                        Ok(current) => Some(current),
                        Err(error)
                            if error
                                .downcast_ref::<kardia_fs::FsError>()
                                .is_some_and(|e| e.code == kardia_fs::FsCode::NotFound) =>
                        {
                            None
                        }
                        Err(error) => return Err(error),
                    };
                    if current.as_deref() != Some(expected) {
                        return Err(Error::new(StaleWrite));
                    }
                }
                match provider.rename(tmp.clone(), self.0.url.clone()).await {
                    Ok(()) => return Ok(()),
                    Err(error) if attempt < RENAME_RETRIES && is_transient_fs(&error) => {}
                    Err(error) => return Err(error),
                }
                tokio::time::sleep(RENAME_BACKOFF * (attempt + 1)).await;
            }
            unreachable!("the loop returns")
        }
        .await;
        if result.is_err() {
            let _ = provider.remove(tmp.clone()).await;
        }
        result
    }

    fn dump(&self, data: &[Value]) -> String {
        let value = Value::Array(data.to_vec());
        match self.0.kind {
            Kind::Yaml => yaml::dump(&value),
            Kind::Json => serde_json::to_string_pretty(&value).expect("JSON serializes"),
        }
    }

    /// Applies `patches` and the unwritten changes to the cached file data,
    /// and reconciles the tree with the result (latest wins).
    fn schedule_apply(&self, state: &mut State) {
        if state.disposed {
            return;
        }
        let Some(cache) = &state.cache else { return };
        let warn = |message: String| self.warn(message);
        let Patched { data, index } = apply_patches(&cache.data, &mut state.patches, &warn);
        let data = apply_journal(data, &state.writing, |_, _| true, &warn);
        state.pending = Some(apply_journal(data, &state.journal, |_, _| true, &warn));
        state.index = index;
        if state.applying.is_some() {
            return;
        }
        let this = self.clone();
        let task = tokio::spawn(async move {
            loop {
                let tree = {
                    let mut state = this.0.state.lock();
                    match state.pending.take() {
                        Some(tree) => tree,
                        None => {
                            state.applying = None;
                            return;
                        }
                    }
                };
                this.tree().root().update(tree).await;
            }
        });
        state.applying = Some(async move { drop(task.await) }.boxed().shared());
    }

    /// Entries the file leaves anonymous get an id here, before the loader
    /// sees them. One that looks exactly like an entry named on the previous
    /// read keeps that id (unless the file now names it explicitly), so
    /// editing a neighbor does not restart it.
    fn assign_ids(&self, state: &mut State, data: &mut [Value]) {
        let mut used = std::collections::HashSet::new();
        collect_ids(data, &mut used);
        for patch in &state.patches {
            if let Some(Value::Array(insert)) = patch.get("insert") {
                collect_ids(insert, &mut used);
            }
        }
        let mut previous = std::mem::take(&mut state.anonymous);
        let store: std::collections::HashSet<String> =
            self.tree().entries().iter().filter_map(|entry| id_of(&entry.options()).map(String::from)).collect();
        assign(data, None, &mut used, &mut previous, &mut state.anonymous, &store);
    }

    /// See [`TreeBackend::locate`]; with `offset`, a char of the expression
    /// `value`. A value is looked up where its owner writes it: in the file,
    /// or in a patch in the include's own options.
    fn locate(&self, id: &str, path: &[PathSegment], value: Option<&Value>, offset: Option<usize>) -> Option<Location> {
        let in_patch = |index: usize, rest: &[PathSegment]| {
            let prefix =
                [PathSegment::Key("config".into()), PathSegment::Key("patches".into()), PathSegment::Index(index)];
            prefix.into_iter().chain(rest.iter().cloned()).collect::<Vec<_>>()
        };
        let path_in_parent = {
            let state = self.0.state.lock();
            let cache = state.cache.as_ref()?;
            let owner = match path.first() {
                Some(PathSegment::Key(key)) => state.index.key(id, key),
                _ => state.index.entry(Some(id)),
            };
            match owner {
                Owner::File => {
                    let mut full = entry_path(&find(&cache.data, id)?);
                    full.extend_from_slice(path);
                    let written = match full.split_first() {
                        Some((PathSegment::Index(index), rest)) => {
                            cache.data.get(*index).and_then(|v| value_at(v, rest))
                        }
                        _ => None,
                    };
                    if value.is_some() && written != value {
                        return None;
                    }
                    let position = match offset {
                        Some(offset) => cache.syntax.locate_expr(&full, offset)?,
                        None => cache.syntax.locate(&full)?,
                    };
                    let Position { line, column } = position;
                    return Some(Location { file: self.0.filename.clone(), line, column });
                }
                Owner::Patch(index) => in_patch(index, path),
                Owner::Embedded(index, key) => {
                    let list = state.patches.get(index)?.get(key)?.as_array()?;
                    let mut rest = vec![PathSegment::Key(key.into())];
                    rest.extend(entry_path(&find(list, id)?));
                    rest.extend_from_slice(path);
                    in_patch(index, &rest)
                }
            }
        };
        let entry = self.0.loader.entry_of(&self.0.ctx)?;
        let own = entry.options()["id"].as_str()?.to_string();
        let tree = entry.parent().tree();
        match offset {
            Some(offset) => tree.locate_expr(&own, &path_in_parent, expr_source(value?)?, offset),
            None => tree.locate(&own, &path_in_parent, value),
        }
    }

    fn warn(&self, message: String) {
        self.0.ctx.root().logger_named("loader").warn(message);
    }
}

fn assign(
    entries: &mut [Value],
    parent: Option<&str>,
    used: &mut std::collections::HashSet<String>,
    previous: &mut Vec<Anonymous>,
    named: &mut Vec<Anonymous>,
    store: &std::collections::HashSet<String>,
) {
    for options in entries {
        if id_of(options).is_none() && options.is_object() {
            let reused = previous.iter().position(|info| {
                info.parent.as_deref() == parent && !used.contains(&info.id) && info.options == *options
            });
            let id = match reused {
                Some(index) => previous.remove(index).id,
                None => random_id(&mut |id| used.contains(id) || store.contains(id)),
            };
            used.insert(id.clone());
            named.push(Anonymous { id: id.clone(), parent: parent.map(String::from), options: options.clone() });
            let map = options.as_object_mut().expect("checked above");
            let rest = std::mem::take(map);
            map.insert("id".into(), Value::String(id));
            map.extend(rest);
        }
        let id = id_of(options).map(String::from);
        if options.get("group") == Some(&Value::Bool(true))
            && let Some(Value::Array(list)) = options.get_mut("config")
        {
            assign(list, id.as_deref(), used, previous, named, store);
        }
    }
}

fn collect_ids(entries: &[Value], used: &mut std::collections::HashSet<String>) {
    for options in entries {
        if let Some(id) = id_of(options) {
            used.insert(id.to_string());
        }
        if let Some(list) = group_list(options) {
            collect_ids(list, used);
        }
    }
}

/// The positions of JSON (and of YAML written by the include itself), which
/// are YAML too.
fn json_syntax(content: &str) -> Syntax {
    yaml::parse(content).map(|(_, syntax)| syntax).unwrap_or_else(|_| yaml::parse("").expect("empty").1)
}

/// The path of an entry from its index path (see [`find`]).
fn entry_path(indices: &[usize]) -> Vec<PathSegment> {
    let mut path = Vec::new();
    for (depth, index) in indices.iter().enumerate() {
        if depth > 0 {
            path.push(PathSegment::Key("config".into()));
        }
        path.push(PathSegment::Index(*index));
    }
    path
}

fn find_invalid(entries: &[Value], prefix: &mut Vec<PathSegment>, report: &mut dyn FnMut(&[PathSegment])) {
    for (index, options) in entries.iter().enumerate() {
        prefix.push(PathSegment::Index(index));
        if !options.is_object() {
            report(prefix);
        } else if let Some(list) = group_list(options) {
            prefix.push(PathSegment::Key("config".into()));
            find_invalid(list, prefix, report);
            prefix.pop();
        }
        prefix.pop();
    }
}

/// A rename over a file another process holds open (e.g. a virus scanner)
/// fails briefly; a bounded retry is cheap.
/// Whether a failed rename may succeed when it is retried: another process
/// holds the destination, or the directory is briefly busy.
fn is_transient_fs(error: &Error) -> bool {
    error
        .downcast_ref::<kardia_fs::FsError>()
        .is_some_and(|error| matches!(error.code, kardia_fs::FsCode::Busy | kardia_fs::FsCode::PermissionDenied))
}

struct Backend(Weak<Inner>);

impl TreeBackend for Backend {
    fn commit(&self, change: &EntryChange) {
        if let Some(inner) = self.0.upgrade() {
            Include(inner).commit(change);
        }
    }

    fn handle(&self) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        Some(self.0.upgrade()?)
    }

    fn locate(&self, id: &str, path: &[PathSegment], value: Option<&Value>) -> Option<Location> {
        Include(self.0.upgrade()?).locate(id, path, value, None)
    }

    fn locate_expr(&self, id: &str, path: &[PathSegment], expr: &str, offset: usize) -> Option<Location> {
        let value = Value::Object([(EXPR_KEY.to_string(), Value::String(expr.into()))].into_iter().collect());
        Include(self.0.upgrade()?).locate(id, path, Some(&value), Some(offset))
    }
}
