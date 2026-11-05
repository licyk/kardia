//! Port of `packages/hmr/tests/watch.spec.ts`, with the real `fs` watch.

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::scratch;
use cordis_core::{Context, Error, Event, PluginDef, Result, Svc};
use cordis_fs::FsPlugin;
use cordis_hmr::{Exports, Hmr, ModuleCode, ModuleGraph, ModuleSource};
use cordis_loader::{Catalog, Loader, TreeBackend};
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

#[tokio::test]
async fn resolves_a_relative_base_against_the_working_directory() {
    let root = Context::new();
    let graph = ModuleGraph::new(Files::default());
    root.plugin_def(&Hmr::plugin(graph), json!({ "base": ".." })).unwrap().settled().await.unwrap();
    let cwd = std::env::current_dir().unwrap();
    assert_eq!(root.get::<Hmr>().unwrap().base_dir(), cwd.parent().unwrap());
}

/// A tree read from a config file in `0`.
struct FileTree(PathBuf);

impl TreeBackend for FileTree {
    fn base_dir(&self) -> Option<PathBuf> {
        Some(self.0.clone())
    }
}

#[tokio::test]
async fn resolves_a_relative_base_against_the_config_file_of_its_entry() {
    let dir = scratch("watch-entry-base");
    let root = Context::new();
    let catalog = Catalog::new();
    catalog.register_def("hmr", Hmr::plugin(ModuleGraph::new(Files::default())));
    let loader = Loader::plugin_with(Arc::new(catalog), Arc::new(FileTree(dir.clone())));
    root.plugin_def(&loader, Value::Null).unwrap().settled().await.unwrap();
    let loader = (*root.get::<Loader>().unwrap()).clone();
    loader.create(json!({ "name": "hmr", "config": { "base": "./plugins" } }), None, None).await.unwrap();
    loader.settled().await;
    assert_eq!(root.get::<Hmr>().unwrap().base_dir(), dir.join("plugins"));
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
