//! Port of `packages/hmr/tests/index.spec.ts` against an in-memory module
//! source. The fixtures of the reference are written as module code here.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use std::time::{Duration, Instant};

use cordis_core::{Context, Disposer, Error, Event, Fiber, FiberState, Level, PluginDef, Result, Svc};
use cordis_hmr::{Exports, Hmr, HmrExt, ModuleCode, ModuleGraph, ModuleSource, ReconcileResult};
use cordis_loader::{EntryHandle, Loader, MemoryTree};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::sync::{Notify, watch};

/// Asks the plugin `name` for its value.
struct Query(&'static str);

impl Event for Query {
    type Output = Option<Value>;
}

/// Asks the instance of `plugin-multi` configured with this label.
struct Labeled(&'static str);

impl Event for Labeled {
    type Output = Option<Value>;
}

#[derive(Clone, Default)]
struct Files(Arc<Mutex<HashMap<String, ModuleCode>>>);

impl ModuleSource for Files {
    fn load(&self, id: &str) -> Result<ModuleCode> {
        self.0.lock().get(id).cloned().ok_or_else(|| Error::msg(format!("cannot find module {id}")))
    }
}

impl Files {
    fn write(&self, name: &str, code: ModuleCode) -> String {
        let id = path(name);
        self.0.lock().insert(id.clone(), code);
        id
    }
}

fn path(name: &str) -> String {
    format!("/app/{name}")
}

fn value(value: &'static str) -> ModuleCode {
    ModuleCode::new(vec![], move |_| Ok(Exports::Value(json!(value))))
}

/// A module whose plugin is made from `version`.
fn versioned(version: &'static str, plugin: impl Fn(Value) -> PluginDef + Send + Sync + 'static) -> ModuleCode {
    ModuleCode::new(vec![], move |_| Ok(Exports::Plugin(plugin(json!(version)))))
}

/// A module importing `dep`, whose plugin is made from the value of `dep`.
fn importing(dep: &str, plugin: impl Fn(Value) -> PluginDef + Send + Sync + 'static) -> ModuleCode {
    let dep = path(dep);
    ModuleCode::new(vec![dep.clone()], move |imports| {
        Ok(Exports::Plugin(plugin(imports.value(&dep).unwrap_or_default())))
    })
}

fn answer(ctx: &Context, name: &'static str, value: Value) -> Result<()> {
    ctx.on(move |query: &Query| (query.0 == name).then(|| value.clone()))?;
    Ok(())
}

struct App {
    root: Context,
    files: Files,
    loader: Loader,
    hmr: Svc<Hmr>,
}

impl App {
    async fn setup() -> Self {
        let root = Context::new();
        root.set_log_buffer_size(1000);
        let files = Files::default();
        let graph = ModuleGraph::new(files.clone());
        root.plugin_def(&Loader::plugin(graph.clone()), Value::Null).unwrap().settled().await.unwrap();
        root.plugin_def(&Hmr::plugin(graph), Value::Null).unwrap().settled().await.unwrap();
        let loader = (*root.get::<Loader>().unwrap()).clone();
        let hmr = root.get::<Hmr>().unwrap();
        Self { root, files, loader, hmr }
    }

    /// Creates the entry `id` for the module `name` and waits for it.
    async fn load(&self, id: &str, name: &str, code: ModuleCode, config: Value) -> EntryHandle {
        let module = self.files.write(name, code);
        let options = json!({ "id": id, "name": module, "config": config });
        let id = self.loader.create(options, None, None).await.unwrap();
        self.loader.settled().await;
        self.loader.resolve(&id).unwrap()
    }

    fn query(&self, name: &'static str) -> Option<Value> {
        self.root.bail(&Query(name)).flatten()
    }

    /// Also waits for the loader: a fiber has settled once its own apply
    /// has, not the fibers of the entries that apply creates.
    fn reconcile(&self, root: &str) -> impl Future<Output = ReconcileResult> + Send + 'static {
        let (hmr, loader, roots) = (self.hmr.clone(), self.loader.clone(), vec![PathBuf::from(root)]);
        async move {
            let result = hmr.reconcile(move || async move { Ok(roots) }).await.unwrap();
            loader.settled().await;
            result
        }
    }

    fn errors(&self) -> Vec<String> {
        let logs = self.root.log_buffer().into_iter();
        logs.filter(|message| matches!(message.level, Level::Error | Level::Warn)).map(|message| message.text).collect()
    }
}

async fn eventually(mut check: impl FnMut() -> bool) {
    for _ in 0..250 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out");
}

async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future).await.expect("timed out")
}

fn no_failures(results: &[&ReconcileResult]) -> bool {
    results.iter().flat_map(|result| &result.settled_fibers).all(|fiber| fiber.state() != FiberState::Failed)
}

// nested entry tree

#[derive(Default)]
struct Tree {
    /// Milliseconds that `plugin-tree` stalls after creating its subtree.
    stall: AtomicU64,
    nested_applied: AtomicUsize,
    nested_disposed: AtomicUsize,
}

impl Tree {
    /// The instances of `plugin-nested` that have been applied and not disposed.
    fn live_nested(&self) -> usize {
        self.nested_applied.load(SeqCst) - self.nested_disposed.load(SeqCst)
    }
}

/// An entry tree host: its fiber owns a subtree with the entry `nested`.
/// The subtree hangs off an extra plain fiber, so the nested entry is two
/// fiber levels down and its direct parent is never itself reloaded.
fn plugin_tree(stats: &Arc<Tree>) -> ModuleCode {
    let stats = stats.clone();
    importing("tree-dep.rs", move |version| {
        let stats = stats.clone();
        PluginDef::new(move |ctx, _| {
            let (version, stats) = (version.clone(), stats.clone());
            async move {
                answer(&ctx, "tree", version)?;
                let loader = (*ctx.get::<Loader>()?).clone();
                let slot = Arc::new(Mutex::new(None));
                let group = slot.clone();
                let plain = PluginDef::new(move |ctx, _| {
                    *group.lock() = Some(loader.create_tree(&ctx, Arc::new(MemoryTree)).root());
                    async { Ok(()) }
                });
                ctx.plugin_def(&plain, Value::Null)?.settled().await?;
                let group = slot.lock().take().expect("created by the plain fiber");
                let stopping = group.clone();
                ctx.defer("tree", Disposer::new(move || stopping.stop()))?;
                group.update(vec![json!({ "id": "nested", "name": path("plugin-nested.rs") })]).await;
                let stall = stats.stall.load(SeqCst);
                if stall > 0 {
                    tokio::time::sleep(Duration::from_millis(stall)).await;
                }
                Ok(())
            }
        })
        .name("plugin-tree")
    })
}

fn plugin_nested(stats: &Arc<Tree>) -> ModuleCode {
    let stats = stats.clone();
    importing("tree-dep.rs", move |version| {
        let stats = stats.clone();
        PluginDef::new(move |ctx, _| {
            let (version, stats) = (version.clone(), stats.clone());
            async move {
                stats.nested_applied.fetch_add(1, SeqCst);
                answer(&ctx, "nested", version)?;
                ctx.on_dispose(move || {
                    stats.nested_disposed.fetch_add(1, SeqCst);
                })?;
                Ok(())
            }
        })
        .name("plugin-nested")
    })
}

async fn tree_app() -> (App, Arc<Tree>, String) {
    let app = App::setup().await;
    let stats = Arc::new(Tree::default());
    let dep = app.files.write("tree-dep.rs", value("tree-v1"));
    app.files.write("plugin-nested.rs", plugin_nested(&stats));
    app.load("tree", "plugin-tree.rs", plugin_tree(&stats), Value::Null).await;
    eventually(|| app.query("nested") == Some(json!("tree-v1"))).await;
    (app, stats, dep)
}

#[tokio::test]
async fn should_keep_a_single_nested_entry_when_its_host_reloads_too() {
    let (app, stats, dep) = tree_app().await;

    // one change, two plugins in the batch: the tree host and the entry
    // nested two fiber levels below it
    app.files.write("tree-dep.rs", value("tree-v2"));
    let result = within(app.reconcile(&dep)).await;
    assert!(no_failures(&[&result]));
    assert_eq!(app.query("tree"), Some(json!("tree-v2")));
    assert_eq!(app.query("nested"), Some(json!("tree-v2")));
    // the host rebuilds its subtree, which disposes the nested entry, even
    // if the nested entry has been restarted in place before
    assert_eq!(stats.live_nested(), 1);
}

#[tokio::test]
async fn should_keep_a_single_nested_entry_across_a_batch_queued_behind_a_stalled_host() {
    let (app, stats, dep) = tree_app().await;

    // batch 1: the host creates its subtree and then stalls in its apply
    stats.stall.store(300, SeqCst);
    app.files.write("tree-dep.rs", value("tree-v2"));
    let first = tokio::spawn(app.reconcile(&dep));
    eventually(|| app.query("nested") == Some(json!("tree-v2"))).await;

    // batch 2 arrives while the host is still stalled, and is queued behind batch 1
    stats.stall.store(0, SeqCst);
    app.files.write("tree-dep.rs", value("tree-v3"));
    let second = within(app.reconcile(&dep)).await;
    let first = within(first).await.unwrap();
    assert!(no_failures(&[&first, &second]));
    assert_eq!(app.query("tree"), Some(json!("tree-v3")));
    assert_eq!(app.query("nested"), Some(json!("tree-v3")));
    assert_eq!(stats.live_nested(), 1);
}

// resource handoff on reload

#[derive(Default)]
struct Drain {
    held: AtomicBool,
    overlaps: AtomicUsize,
    slow_dispose_started: Mutex<Option<Instant>>,
    slow_released: Mutex<Option<Instant>>,
    fast_applied: Mutex<Option<Instant>>,
}

/// Owns an exclusive resource and releases it slowly.
fn plugin_slow(stats: &Arc<Drain>) -> ModuleCode {
    let stats = stats.clone();
    importing("drain-dep.rs", move |version| {
        let stats = stats.clone();
        PluginDef::new(move |ctx, _| {
            let (version, stats) = (version.clone(), stats.clone());
            async move {
                if stats.held.swap(true, SeqCst) {
                    stats.overlaps.fetch_add(1, SeqCst);
                }
                answer(&ctx, "slow", version)?;
                let release = Disposer::future(move || async move {
                    *stats.slow_dispose_started.lock() = Some(Instant::now());
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    stats.held.store(false, SeqCst);
                    *stats.slow_released.lock() = Some(Instant::now());
                });
                ctx.defer("slow", release)?;
                Ok(())
            }
        })
        .name("plugin-slow")
    })
}

/// Unloads at once; must not be held back by the drain of `plugin-slow`.
fn plugin_fast(stats: &Arc<Drain>) -> ModuleCode {
    let stats = stats.clone();
    importing("drain-dep.rs", move |version| {
        let stats = stats.clone();
        PluginDef::new(move |ctx, _| {
            *stats.fast_applied.lock() = Some(Instant::now());
            let version = version.clone();
            async move { answer(&ctx, "fast", version) }
        })
        .name("plugin-fast")
    })
}

async fn drain_app() -> (App, Arc<Drain>, String) {
    let app = App::setup().await;
    let stats = Arc::new(Drain::default());
    // a dependency of both plugins, so that one change puts both into one batch
    let dep = app.files.write("drain-dep.rs", value("drain-v1"));
    app.load("test-slow", "plugin-slow.rs", plugin_slow(&stats), Value::Null).await;
    app.load("test-fast", "plugin-fast.rs", plugin_fast(&stats), Value::Null).await;
    (app, stats, dep)
}

#[tokio::test]
async fn should_not_start_the_new_instance_before_the_old_one_released() {
    let (app, stats, dep) = drain_app().await;
    assert_eq!(app.query("slow"), Some(json!("drain-v1")));

    app.files.write("drain-dep.rs", value("drain-v2"));
    within(app.reconcile(&dep)).await;
    assert_eq!(app.query("slow"), Some(json!("drain-v2")));
    assert_eq!(stats.overlaps.load(SeqCst), 0);
}

#[tokio::test]
async fn should_not_delay_an_unrelated_plugin_behind_a_slow_one() {
    let (app, stats, dep) = drain_app().await;
    assert_eq!(app.query("fast"), Some(json!("drain-v1")));

    app.files.write("drain-dep.rs", value("drain-v3"));
    let reloaded = tokio::spawn(app.reconcile(&dep));
    eventually(|| app.query("fast") == Some(json!("drain-v3"))).await;
    within(reloaded).await.unwrap();

    // plugin-fast comes back while plugin-slow is still draining
    let fast_applied = stats.fast_applied.lock().unwrap();
    assert!(fast_applied >= stats.slow_dispose_started.lock().unwrap());
    assert!(fast_applied < stats.slow_released.lock().unwrap());
}

// plugin with multiple entries

/// Loaded by two entries at once, so its runtime holds two fibers. Each
/// instance answers only for its own label.
fn plugin_multi(applies: &Arc<Mutex<Vec<String>>>, version: &'static str) -> ModuleCode {
    let applies = applies.clone();
    versioned(version, move |version| {
        let applies = applies.clone();
        PluginDef::new(move |ctx, config| {
            let label = config["label"].as_str().unwrap_or_default().to_string();
            applies.lock().push(format!("{label}:{}", version.as_str().unwrap_or_default()));
            let version = version.clone();
            async move {
                ctx.on(move |query: &Labeled| (query.0 == label).then(|| version.clone()))?;
                Ok(())
            }
        })
        .name("plugin-multi")
    })
}

#[tokio::test]
async fn should_rebuild_every_fiber_each_keeping_its_own_config() {
    let app = App::setup().await;
    let applies = Arc::new(Mutex::new(Vec::new()));
    let entry =
        app.load("multi-a", "plugin-multi.rs", plugin_multi(&applies, "multi-v1"), json!({ "label": "a" })).await;
    app.load("multi-b", "plugin-multi.rs", plugin_multi(&applies, "multi-v1"), json!({ "label": "b" })).await;
    // both entries resolve to the same module, so one runtime holds two fibers
    assert_eq!(app.root.bail(&Labeled("a")).flatten(), Some(json!("multi-v1")));
    assert_eq!(app.root.bail(&Labeled("b")).flatten(), Some(json!("multi-v1")));
    assert_eq!(app.root.registry().fibers(&entry.inner_fiber().unwrap().def().unwrap()).len(), 2);

    applies.lock().clear();
    let module = app.files.write("plugin-multi.rs", plugin_multi(&applies, "multi-v2"));
    let result = within(app.reconcile(&module)).await;
    assert_eq!(result.settled_fibers.len(), 2);
    // exactly one new instance per entry, each with its own config intact
    let mut applies = applies.lock().clone();
    applies.sort();
    assert_eq!(applies, vec!["a:multi-v2", "b:multi-v2"]);
}

// entry changed during unload

#[derive(Default)]
struct Gate {
    acquired: AtomicUsize,
    applies: AtomicUsize,
    /// The fibers of `plugin-disabled-hmr` that have not started to unload.
    fibers: Mutex<Vec<Fiber>>,
    /// The plugin, for a host that does not import its module to mount it.
    shared: Mutex<Option<PluginDef>>,
    barrier: Mutex<Option<Barrier>>,
}

/// Holds every unload of `plugin-disabled-hmr` until released.
#[derive(Clone)]
struct Barrier {
    entered: Arc<Notify>,
    hold: watch::Receiver<bool>,
}

impl Gate {
    /// Holds the next unloads: returns the notification that one has started
    /// and the sender that releases them.
    fn arm(&self) -> (Arc<Notify>, watch::Sender<bool>) {
        let (release, hold) = watch::channel(false);
        let entered = Arc::new(Notify::new());
        *self.barrier.lock() = Some(Barrier { entered: entered.clone(), hold });
        (entered, release)
    }

    fn live(&self) -> Vec<Fiber> {
        self.fibers.lock().iter().filter(|fiber| !fiber.is_removed()).cloned().collect()
    }
}

fn plugin_disabled_hmr(gate: &Arc<Gate>, value: &'static str) -> ModuleCode {
    let gate = gate.clone();
    versioned(value, move |value| {
        let gate = gate.clone();
        PluginDef::new(move |ctx, _| {
            let (gate, value) = (gate.clone(), value.clone());
            async move {
                let fiber = ctx.fiber();
                *gate.shared.lock() = fiber.def();
                gate.acquired.fetch_add(1, SeqCst);
                gate.applies.fetch_add(1, SeqCst);
                gate.fibers.lock().push(fiber.clone());
                answer(&ctx, "value", value)?;
                let unload = Disposer::future(move || async move {
                    gate.acquired.fetch_sub(1, SeqCst);
                    gate.fibers.lock().retain(|other| *other != fiber);
                    let barrier = gate.barrier.lock().clone();
                    if let Some(Barrier { entered, mut hold }) = barrier {
                        entered.notify_one();
                        let _ = hold.wait_for(|released| *released).await;
                    }
                });
                ctx.defer("gate", unload)?;
                Ok(())
            }
        })
        .name("plugin-disabled-hmr")
    })
}

/// Mounts the plugin of `plugin-disabled-hmr` without importing its module.
fn plugin_host(gate: &Arc<Gate>) -> ModuleCode {
    let gate = gate.clone();
    ModuleCode::new(vec![], move |_| {
        let gate = gate.clone();
        let def = PluginDef::new(move |ctx, _| {
            let shared = gate.shared.lock().clone();
            async move {
                let shared = shared.ok_or_else(|| Error::msg("shared plugin missing"))?;
                ctx.plugin_def(&shared, Value::Null)?;
                Ok(())
            }
        });
        Ok(Exports::Plugin(def.name("plugin-host")))
    })
}

async fn gate_app() -> (App, Arc<Gate>, EntryHandle) {
    let app = App::setup().await;
    let gate = Arc::new(Gate::default());
    let entry = app.load("live", "plugin-disabled-hmr.rs", plugin_disabled_hmr(&gate, "initial"), Value::Null).await;
    assert_eq!(gate.acquired.load(SeqCst), 1);
    assert_eq!(app.query("value"), Some(json!("initial")));
    gate.applies.store(0, SeqCst);
    (app, gate, entry)
}

#[tokio::test]
async fn does_not_rebuild_an_entry_disabled_during_unload() {
    let (app, gate, entry) = gate_app().await;
    let previous = entry.inner_fiber().unwrap();
    let (entered, release) = gate.arm();
    let module = app.files.write("plugin-disabled-hmr.rs", plugin_disabled_hmr(&gate, "should-not-live"));
    let reloaded = tokio::spawn(app.reconcile(&module));
    within(entered.notified()).await;

    within(app.loader.update("live", json!({ "disabled": true }), None, None)).await.unwrap();
    release.send(true).unwrap();
    within(reloaded).await.unwrap();

    assert!(entry.is_disabled());
    assert_eq!(entry.inner_fiber(), Some(previous.clone()));
    assert!(previous.is_removed());
    assert_eq!(app.query("value"), None);
    assert_eq!(gate.applies.load(SeqCst), 0);
    assert!(gate.live().is_empty());
}

#[tokio::test]
async fn does_not_rebuild_an_entry_removed_during_unload() {
    let (app, gate, entry) = gate_app().await;
    let previous = entry.inner_fiber().unwrap();
    let (entered, release) = gate.arm();
    let module = app.files.write("plugin-disabled-hmr.rs", plugin_disabled_hmr(&gate, "removed-value"));
    let reloaded = tokio::spawn(app.reconcile(&module));
    within(entered.notified()).await;

    app.loader.remove("live").unwrap();
    // removal does not set `disabled`
    assert!(!entry.is_disabled());
    release.send(true).unwrap();
    within(reloaded).await.unwrap();

    assert_eq!(entry.inner_fiber(), Some(previous.clone()));
    assert!(previous.is_removed());
    assert_eq!(app.query("value"), None);
    assert_eq!(gate.applies.load(SeqCst), 0);
    assert!(gate.live().is_empty());
}

/// In Rust the hmr transaction has decided once the held unload completes,
/// before either re-enable starts, so there is no hmr decision that waits on
/// them; the case guards that re-enables racing a reload leave exactly one
/// live instance of the new code, and that hmr adds no apply of its own.
#[tokio::test]
async fn waits_for_a_second_re_enable_init_before_deciding_whether_to_rebuild() {
    let (app, gate, entry) = gate_app().await;
    let previous = entry.inner_fiber().unwrap();
    let (entered, release) = gate.arm();
    let module = app.files.write("plugin-disabled-hmr.rs", plugin_disabled_hmr(&gate, "second-init"));
    let reloaded = tokio::spawn(app.reconcile(&module));
    within(entered.notified()).await;

    let (first_hold, first_held) = watch::channel(false);
    let (second_hold, second_held) = watch::channel(false);
    let patches = Arc::new(AtomicUsize::new(0));
    let hook = {
        let (patches, live) = (patches.clone(), entry.id());
        app.loader
            .on_patch_context(&app.root, move |patched, next| {
                let held = match patched.id() == live {
                    true => match patches.fetch_add(1, SeqCst) + 1 {
                        1 => Some(first_held.clone()),
                        2 => Some(second_held.clone()),
                        _ => None,
                    },
                    false => None,
                };
                async move {
                    if let Some(mut held) = held {
                        let _ = held.wait_for(|released| *released).await;
                    }
                    next.await
                }
            })
            .unwrap()
    };
    within(app.loader.update("live", json!({ "disabled": true }), None, None)).await.unwrap();
    let loader = app.loader.clone();
    let updating = tokio::spawn(async move { loader.update("live", json!({ "disabled": false }), None, None).await });
    eventually(|| patches.load(SeqCst) == 1).await;
    // Joining through `init()` observes its completion before the outer
    // update. TS has started the plugin's apply by the time it continues and
    // applies the disable synchronously; here the test waits for both, so
    // that the enable is a second start.
    let reenabled = tokio::spawn({
        let (entry, loader) = (entry.clone(), app.loader.clone());
        async move {
            entry.init().await;
            let installed = entry.inner_fiber();
            if let Some(installed) = &installed {
                let _ = installed.settled().await;
            }
            loader.update("live", json!({ "disabled": true }), None, None).await.unwrap();
            let enabled = loader.update("live", json!({ "disabled": false }), None, None).await;
            (installed, enabled)
        }
    });

    release.send(true).unwrap();
    // hmr was already waiting for this unload before the test joins it
    let _ = within(previous.settled()).await;
    first_hold.send(true).unwrap();
    eventually(|| patches.load(SeqCst) == 2).await;
    second_hold.send(true).unwrap();
    within(updating).await.unwrap().unwrap();
    let (installed, enabled) = within(reenabled).await.unwrap();
    enabled.unwrap();
    within(reloaded).await.unwrap();
    if let Some(installed) = installed {
        let _ = within(installed.settled()).await;
    }
    app.loader.settled().await;
    hook.dispose().await.unwrap();

    assert_eq!(patches.load(SeqCst), 2);
    assert_eq!(app.query("value"), Some(json!("second-init")));
    // one per re-enable, none from hmr
    assert_eq!(gate.applies.load(SeqCst), 2);
    assert_eq!(gate.live(), vec![entry.inner_fiber().unwrap()]);
}

#[tokio::test]
async fn rebuilds_a_child_of_an_active_host_that_reused_the_plugin() {
    let (app, gate, entry) = gate_app().await;
    let previous = entry.inner_fiber().unwrap();
    let host = app.load("host", "plugin-host.rs", plugin_host(&gate), Value::Null).await;
    let host_root = host.inner_fiber().unwrap();
    eventually(|| gate.live().len() == 2).await;
    let applies = gate.applies.load(SeqCst);

    let (entered, release) = gate.arm();
    let module = app.files.write("plugin-disabled-hmr.rs", plugin_disabled_hmr(&gate, "from-host"));
    let reloaded = tokio::spawn(app.reconcile(&module));
    within(entered.notified()).await;
    eventually(|| gate.acquired.load(SeqCst) == 0).await;
    release.send(true).unwrap();
    within(reloaded).await.unwrap();
    eventually(|| app.query("value") == Some(json!("from-host"))).await;

    // both the entry root and the child of the host are rebuilt in place
    assert_eq!(gate.applies.load(SeqCst) - applies, 2);
    assert_eq!(entry.inner_fiber(), Some(previous));
    assert_eq!(gate.live().len(), 2);
    // rebuilding the child does not take over the host's entry root
    assert_eq!(host.inner_fiber(), Some(host_root.clone()));

    within(app.loader.update("host", json!({ "disabled": true }), None, None)).await.unwrap();
    let _ = within(host_root.settled()).await;
    assert!(host_root.is_removed());
    assert_eq!(gate.live(), vec![entry.inner_fiber().unwrap()]);
}

#[tokio::test]
async fn skips_a_child_whose_host_is_replaced_during_unload() {
    let (app, gate, _) = gate_app().await;
    let host = app.load("host", "plugin-host.rs", plugin_host(&gate), Value::Null).await;
    let host_root = host.inner_fiber().unwrap();
    eventually(|| gate.live().len() == 2).await;

    let (entered, release) = gate.arm();
    let module = app.files.write("plugin-disabled-hmr.rs", plugin_disabled_hmr(&gate, "host-replaced"));
    let reloaded = tokio::spawn(app.reconcile(&module));
    within(entered.notified()).await;
    eventually(|| gate.acquired.load(SeqCst) == 0).await;

    within(app.loader.update("host", json!({ "disabled": true }), None, None)).await.unwrap();
    within(app.loader.update("host", json!({ "disabled": false }), None, None)).await.unwrap();
    assert!(host_root.is_removed());
    assert!(host.inner_fiber().is_some_and(|fiber| !fiber.is_removed()));

    release.send(true).unwrap();
    let result = within(reloaded).await.unwrap();
    // the stale child is not rebuilt under the disposed host
    assert!(no_failures(&[&result]));
    assert_eq!(app.errors(), Vec::<String>::new());
    // the entry root and the child mounted by the new host
    eventually(|| gate.live().len() == 2).await;
}
