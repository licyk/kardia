//! Port of `packages/hmr/tests/watch.spec.ts`, with the real `fs` watch.

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::SeqCst;
use std::time::Duration;

use common::scratch;
use kardia_core::{Context, Error, Event, PluginDef, Result, Svc};
use kardia_fs::FsPlugin;
use kardia_hmr::{Exports, Hmr, ModuleCode, ModuleGraph, ModuleSource};
use kardia_loader::{Catalog, Loader, TreeBackend};
use parking_lot::Mutex;
use serde_json::{Value, json};

struct Query(&'static str);

impl Event for Query {
    type Output = Option<Value>;
}

/// Module code by path. Writing a module also writes its file, which the
/// watch reports.
#[derive(Clone, Default)]
struct Files(Arc<Mutex<HashMap<String, ModuleCode>>>);

impl ModuleSource for Files {
    fn load(&self, id: &str) -> Result<ModuleCode> {
        self.0.lock().get(id).cloned().ok_or_else(|| Error::msg(format!("cannot find module {id}")))
    }
}

impl Files {
    fn write(&self, path: &Path, code: ModuleCode) {
        let id = path.to_str().unwrap().to_string();
        let version = {
            let mut modules = self.0.lock();
            modules.insert(id, code);
            modules.len()
        };
        std::fs::write(path, format!("// {version} {:?}", std::time::SystemTime::now())).unwrap();
    }
}

/// A plugin answering `Query(name)` with `value`, or with the value of `dep`.
fn plugin(name: &'static str, value: &'static str, dep: Option<&Path>) -> ModuleCode {
    let deps: Vec<String> = dep.iter().map(|dep| dep.to_str().unwrap().to_string()).collect();
    let dep = deps.first().cloned();
    ModuleCode::new(deps, move |imports| {
        let value = dep.as_ref().map_or(json!(value), |dep| imports.value(dep).unwrap_or_default());
        Ok(Exports::Plugin(PluginDef::new(move |ctx, _| {
            let value = value.clone();
            async move {
                ctx.on(move |query: &Query| (query.0 == name).then(|| value.clone()))?;
                Ok(())
            }
        })))
    })
}

fn throws(message: &'static str) -> ModuleCode {
    ModuleCode::new(vec![], move |_| Err(Error::msg(message)))
}

fn value(value: &'static str) -> ModuleCode {
    ModuleCode::new(vec![], move |_| Ok(Exports::Value(json!(value))))
}

struct App {
    root: Context,
    graph: ModuleGraph,
    loader: Loader,
    hmr: Svc<Hmr>,
}

impl App {
    /// Write the files before, so that the watch does not see them created.
    async fn setup(files: &Files, base: &Path, watch: Value) -> Self {
        let root = Context::new();
        root.set_log_buffer_size(1000);
        let graph = ModuleGraph::new(files.clone());
        root.plugin_def(&Loader::plugin(graph.clone()), Value::Null).unwrap().settled().await.unwrap();
        root.plugin::<FsPlugin>(()).unwrap().settled().await.unwrap();
        let config = json!({ "base": base, "watch": watch });
        // batches are logged at the debug level
        let verbose = root.intercept("logger", json!({ "level": 3 }));
        verbose.plugin_def(&Hmr::plugin(graph.clone()), config).unwrap().settled().await.unwrap();
        let loader = (*root.get::<Loader>().unwrap()).clone();
        let hmr = root.get::<Hmr>().unwrap();
        Self { root, graph, loader, hmr }
    }

    async fn load(&self, path: &Path) {
        self.loader.create(json!({ "name": path.to_str().unwrap() }), None, None).await.unwrap();
        self.loader.settled().await;
    }

    fn query(&self, name: &'static str) -> Option<Value> {
        self.root.bail(&Query(name)).flatten()
    }

    fn logs(&self) -> Vec<String> {
        self.root.log_buffer().into_iter().map(|message| message.text).collect()
    }

    fn batches(&self) -> usize {
        self.logs().iter().filter(|log| log.starts_with("reconcile ")).count()
    }
}

fn watching() -> Value {
    json!({ "enabled": true, "debounce": 100 })
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

/// The base directory of hmr loaded without a loader entry.
async fn base_without_entry(config: Value) -> PathBuf {
    let root = Context::new();
    let graph = ModuleGraph::new(Files::default());
    root.plugin_def(&Hmr::plugin(graph), config).unwrap().settled().await.unwrap();
    root.get::<Hmr>().unwrap().base_dir().to_path_buf()
}

#[tokio::test]
async fn defaults_to_the_working_directory_without_a_context_directory() {
    assert_eq!(base_without_entry(json!({})).await, std::env::current_dir().unwrap());
}

#[tokio::test]
async fn accepts_a_relative_path_without_a_context_directory() {
    let cwd = std::env::current_dir().unwrap();
    assert_eq!(base_without_entry(json!({ "base": "../hmr base" })).await, cwd.parent().unwrap().join("hmr base"));
}

#[tokio::test]
async fn accepts_an_absolute_path_without_a_context_directory() {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join("base with spaces # %");
    assert_eq!(base_without_entry(json!({ "base": base })).await, base);
}

/// A tree read from a config file in `0`.
struct FileTree(PathBuf);

impl TreeBackend for FileTree {
    fn base_dir(&self) -> Option<PathBuf> {
        Some(self.0.clone())
    }
}

/// The base directory of hmr loaded by an entry of a config file in `dir`
/// (the context directory of the reference).
async fn base_of_entry(dir: &Path, config: Value) -> PathBuf {
    let root = Context::new();
    let catalog = Catalog::new();
    catalog.register_def("hmr", Hmr::plugin(ModuleGraph::new(Files::default())));
    let loader = Loader::plugin_with(Arc::new(catalog), Arc::new(FileTree(dir.to_path_buf())));
    root.plugin_def(&loader, Value::Null).unwrap().settled().await.unwrap();
    let loader = (*root.get::<Loader>().unwrap()).clone();
    loader.create(json!({ "name": "hmr", "config": config }), None, None).await.unwrap();
    loader.settled().await;
    root.get::<Hmr>().unwrap().base_dir().to_path_buf()
}

#[tokio::test]
async fn resolves_a_relative_base_against_the_context() {
    let dir = scratch("watch-entry-base");
    assert_eq!(base_of_entry(&dir, json!({ "base": "./plugins" })).await, dir.join("plugins"));
}

#[tokio::test]
async fn defaults_to_the_context_directory() {
    let dir = scratch("watch-entry-default");
    assert_eq!(base_of_entry(&dir, json!({})).await, dir);
}

#[tokio::test]
async fn watches_files_in_base() {
    let dir = scratch("watch-files");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("other")).unwrap();
    let files = Files::default();
    let (watched, ignored) = (dir.join("src/watched.rs"), dir.join("other/ignored.rs"));
    files.write(&watched, plugin("watched", "v1", None));
    files.write(&ignored, plugin("ignored", "v1", None));
    let config = json!({ "enabled": true, "debounce": 100, "files": ["src", "!**/.*"] });
    let app = App::setup(&files, &dir, config).await;
    assert!(app.logs().contains(&format!("watching [\"src\", \"!**/.*\"] in {}", dir.display())));
    app.load(&watched).await;
    app.load(&ignored).await;

    files.write(&ignored, plugin("ignored", "v2", None));
    files.write(&watched, plugin("watched", "v2", None));
    eventually(|| app.query("watched") == Some(json!("v2"))).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(app.query("ignored"), Some(json!("v1")));
}

#[tokio::test]
async fn watches_only_when_enabled_with_files() {
    let dir = scratch("watch-disabled");
    for watch in [json!({}), json!({ "enabled": true, "files": [] })] {
        let app = App::setup(&Files::default(), &dir, watch).await;
        assert!(!app.logs().iter().any(|log| log.starts_with("watching")));
    }
    let app = App::setup(&Files::default(), &dir, watching()).await;
    assert!(app.logs().iter().any(|log| log.starts_with("watching")));
}

#[tokio::test]
async fn reconciles_the_changes_of_one_debounce_window_as_a_batch() {
    let dir = scratch("watch-batch");
    let files = Files::default();
    let (a, b) = (dir.join("batch-a.rs"), dir.join("batch-b.rs"));
    files.write(&a, plugin("a", "v1", None));
    files.write(&b, plugin("b", "v1", None));
    let app = App::setup(&files, &dir, watching()).await;
    app.load(&a).await;
    app.load(&b).await;

    files.write(&a, plugin("a", "v2", None));
    files.write(&b, plugin("b", "v2", None));
    eventually(|| app.query("a") == Some(json!("v2")) && app.query("b") == Some(json!("v2"))).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(app.batches(), 1);
}

#[tokio::test]
async fn retries_the_changes_of_a_failed_batch_with_the_next_one() {
    let dir = scratch("watch-retry");
    let files = Files::default();
    let (dep, path) = (dir.join("retry-dep.rs"), dir.join("retry.rs"));
    files.write(&dep, value("v1"));
    files.write(&path, plugin("retry", "", Some(&dep)));
    let app = App::setup(&files, &dir, watching()).await;
    app.load(&path).await;
    assert_eq!(app.query("retry"), Some(json!("v1")));

    // one window: the dependency changes, but the plugin no longer imports
    files.write(&dep, value("v2"));
    files.write(&path, throws("retry is broken"));
    eventually(|| app.logs().iter().any(|log| log.contains("retry is broken"))).await;
    assert_eq!(app.query("retry"), Some(json!("v1")));

    // the next window only repairs the plugin
    files.write(&path, plugin("retry", "", Some(&dep)));
    eventually(|| app.query("retry") == Some(json!("v2"))).await;
}

#[tokio::test]
async fn only_reports_a_change_to_a_host_module() {
    let dir = scratch("watch-host");
    let files = Files::default();
    let path = dir.join("host.rs");
    files.write(&path, plugin("host", "v1", None));
    let app = App::setup(&files, &dir, watching()).await;
    app.load(&path).await;
    app.graph.mark_host([path.to_str().unwrap()]);

    files.write(&path, plugin("host", "v2", None));
    eventually(|| app.logs().iter().any(|log| log == "skipped host modules:\n- host.rs")).await;
    assert_eq!(app.query("host"), Some(json!("v1")));
    assert_eq!(app.hmr.base_dir(), dir);
}

// The first batch is held inside a (synchronous) module load, which
// `block_in_place` allows on a multi-thread runtime only.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retries_the_changes_of_a_failed_batch_with_a_batch_queued_behind_it() {
    let dir = scratch("watch-queued");
    let files = Files::default();
    let (dep, path) = (dir.join("queued-dep.rs"), dir.join("queued.rs"));
    files.write(&dep, value("v1"));
    files.write(&path, plugin("queued", "", Some(&dep)));
    let app = App::setup(&files, &dir, watching()).await;
    app.load(&path).await;

    // the first batch holds while importing the plugin, and then fails
    let entered = Arc::new(AtomicBool::new(false));
    let (release, hold) = std::sync::mpsc::channel::<()>();
    let (hold, e) = (Mutex::new(hold), entered.clone());
    files.write(&dep, value("v2"));
    files.write(
        &path,
        ModuleCode::new(vec![dep.to_str().unwrap().into()], move |_| {
            e.store(true, SeqCst);
            tokio::task::block_in_place(|| hold.lock().recv()).unwrap();
            Ok(Exports::Value(json!("not a plugin")))
        }),
    );
    eventually(|| entered.load(SeqCst)).await;

    // the repair arrives while the first batch is still running
    files.write(&path, plugin("queued", "", Some(&dep)));
    tokio::time::sleep(Duration::from_millis(300)).await;
    release.send(()).unwrap();
    eventually(|| app.query("queued") == Some(json!("v2"))).await;
    assert!(app.logs().iter().any(|log| log.starts_with("invalid plugin at")));
}
