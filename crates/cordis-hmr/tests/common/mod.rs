use std::path::{Path, PathBuf};

use cordis_core::{Context, Fiber, Result, Svc};
use cordis_hmr::{GuestService, Hmr, HmrExt, ModuleGraph, ModuleSource, ReconcileResult};
use cordis_loader::{EntryHandle, Loader};
use serde_json::{Value, json};

/// A fresh directory for the files of one test.
pub fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("hmr").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub struct App {
    pub root: Context,
    pub loader: Loader,
    pub hmr: Svc<Hmr>,
}

impl App {
    pub async fn setup(source: impl ModuleSource) -> Self {
        let root = Context::new();
        let graph = ModuleGraph::new(source);
        root.plugin_def(&Loader::plugin(graph.clone()), Value::Null).unwrap().settled().await.unwrap();
        root.plugin_def(&Hmr::plugin(graph), Value::Null).unwrap().settled().await.unwrap();
        let loader = (*root.get::<Loader>().unwrap()).clone();
        let hmr = root.get::<Hmr>().unwrap();
        Self { root, loader, hmr }
    }

    /// Creates an entry for the module at `path` and waits for it.
    pub async fn load(&self, path: &Path, config: Value) -> EntryHandle {
        let options = json!({ "name": path.to_str().unwrap(), "config": config });
        let id = self.loader.create(options, None, None).await.unwrap();
        self.loader.settled().await;
        self.loader.entries().into_iter().find(|entry| entry.id() == id).unwrap()
    }

    pub async fn reconcile(&self, paths: &[&Path]) -> Result<ReconcileResult> {
        let roots: Vec<PathBuf> = paths.iter().map(|path| path.to_path_buf()).collect();
        let result = self.hmr.reconcile(move || async move { Ok(roots) }).await;
        self.loader.settled().await;
        result
    }

    pub fn view(&self, key: &str) -> Value {
        self.root.view(key).unwrap()
    }

    pub fn service(&self, key: &str) -> Svc<GuestService> {
        self.root.get_as::<GuestService>(key).unwrap()
    }

    pub fn logs(&self) -> Vec<String> {
        self.root.log_buffer().into_iter().map(|message| message.text).collect()
    }
}

pub fn fiber(entry: &EntryHandle) -> Fiber {
    entry.inner_fiber().unwrap()
}
