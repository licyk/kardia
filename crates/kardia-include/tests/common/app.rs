//! The test app: a loader with one include, and the exporter that records its logs.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use kardia_core::{Context, Exporter, Level, Message, PluginDef};
use kardia_fs::{Fs, FsExt, FsPlugin};
use kardia_include::{Include, IncludePlugin};
use kardia_loader::{Loader, TreeHandle, builtins};
use parking_lot::Mutex;
use serde_json::{Value, json};

use super::fixtures::{Content, read, write_fixture};
use super::plugins::{ConfigQuery, Running, config_plugin, schema_plugin};

#[derive(Clone, Default)]
struct Messages(Arc<Mutex<Vec<Message>>>);

impl Exporter for Messages {
    fn export(&self, message: &Message) {
        self.0.lock().push(message.clone());
    }

    fn level(&self, _: &str) -> Option<Level> {
        Some(Level::Debug)
    }
}

pub struct App {
    pub root: Context,
    pub loader: Loader,
    pub id: String,
    pub filename: PathBuf,
    /// Tags of every `config-plugin` apply, in order.
    pub applied: Arc<Mutex<Vec<String>>>,
    messages: Messages,
}

/// A rename that a test can make fail, so the retry of a write-back is exercised
/// against the real provider.
#[derive(Clone, Default)]
pub struct Faults(Arc<Mutex<FaultState>>);

#[derive(Default)]
pub struct FaultState {
    pub calls: usize,
    pub failures: usize,
    pub kind: Option<std::io::ErrorKind>,
    pub before_fail: Option<Box<dyn Fn() + Send>>,
}

impl Faults {
    pub fn lock(&self) -> parking_lot::MutexGuard<'_, FaultState> {
        self.0.lock()
    }
}

impl kardia_fs::Fault for Faults {
    fn before_rename(&self) -> Option<kardia_core::Error> {
        let mut state = self.0.lock();
        state.calls += 1;
        if state.failures == 0 {
            return None;
        }
        state.failures -= 1;
        if let Some(before_fail) = &state.before_fail {
            before_fail();
        }
        let kind = state.kind.unwrap_or(std::io::ErrorKind::ResourceBusy);
        // Only a busy resource is retried; any other `kind` fails the write.
        let code = match kind {
            std::io::ErrorKind::ResourceBusy => kardia_fs::FsCode::Busy,
            std::io::ErrorKind::PermissionDenied => kardia_fs::FsCode::PermissionDenied,
            _ => kardia_fs::FsCode::Unknown,
        };
        Some(kardia_fs::FsError::new(code, None, std::io::Error::from(kind).to_string()).into())
    }
}

impl App {
    /// Boots a loader with one include over a fresh fixture file.
    pub async fn setup(name: &str, data: impl Into<Content>, config: Value) -> Self {
        Self::boot(write_fixture(name, data), config).await
    }

    pub async fn boot(filename: PathBuf, config: Value) -> Self {
        Self::boot_faulty(filename, config, None).await
    }

    /// Boots with `include` registered as the `include` plugin.
    pub async fn boot_with(filename: PathBuf, config: Value, include: PluginDef) -> Self {
        Self::boot_custom(filename, config, include, None).await
    }

    /// Boots with `include` and, when given, a fault injected into the provider
    /// of `file:` that every rename of the write-back goes through.
    pub async fn boot_faulty(filename: PathBuf, config: Value, fault: Option<Faults>) -> Self {
        Self::boot_custom(filename, config, PluginDef::of::<IncludePlugin>(), fault).await
    }

    async fn boot_custom(filename: PathBuf, config: Value, include: PluginDef, fault: Option<Faults>) -> Self {
        let root = Context::new();
        let messages = Messages::default();
        root.add_exporter(Arc::new(messages.clone())).unwrap();
        root.plugin::<FsPlugin>(Default::default()).unwrap().settled().await.unwrap();
        match fault {
            Some(fault) => {
                let fs = root.get::<Fs>().unwrap();
                fs.mount("file:", Arc::new(kardia_fs::Faulty::new(Arc::new(kardia_fs::LocalFs), fault))).unwrap();
            }
            None => {
                root.plugin::<kardia_fs::LocalFsPlugin>(Default::default()).unwrap().settled().await.unwrap();
            }
        }
        let applied = Arc::new(Mutex::new(Vec::new()));
        let catalog = builtins();
        catalog.register_def("include", include);
        catalog.register_def("config-plugin", config_plugin(applied.clone()));
        catalog.register_def("schema-plugin", schema_plugin());
        root.plugin_def(&Loader::plugin(catalog), Value::Null).unwrap().settled().await.unwrap();
        let loader = (*root.get::<Loader>().unwrap()).clone();
        let mut include = json!({ "path": filename.to_string_lossy() });
        include.as_object_mut().unwrap().extend(config.as_object().cloned().unwrap_or_default());
        let id = loader.create(json!({ "name": "include", "config": include }), None, None).await.unwrap();
        if let Some(fiber) = loader.resolve(&id).unwrap().inner_fiber() {
            let _ = fiber.settled().await;
        }
        let app = Self { root, loader, id, filename, applied, messages };
        app.settle().await;
        app
    }

    pub fn include(&self) -> Include {
        Include::of(&self.tree()).unwrap()
    }

    /// The include's entry tree (its store).
    pub fn tree(&self) -> TreeHandle {
        self.loader.resolve(&self.id).unwrap().subtree().unwrap()
    }

    /// The include of the entry `local` inside this app's include.
    pub fn nested(&self, local: &str) -> Include {
        let tree = self.loader.resolve(&format!("{}:{local}", self.id)).unwrap().subtree().unwrap();
        Include::of(&tree).unwrap()
    }

    /// What a watcher triggers on a change; also waits for any write.
    pub async fn settle(&self) {
        if let Some(include) = self.try_include() {
            include.refresh().await;
        }
        self.loader.settled().await;
    }

    pub fn try_include(&self) -> Option<Include> {
        Include::of(&self.loader.resolve(&self.id).ok()?.subtree()?)
    }

    pub fn config(&self, tag: &str) -> Option<Value> {
        self.root.bail(&ConfigQuery(tag.into())).flatten()
    }

    pub fn value(&self, tag: &str) -> Option<i64> {
        self.config(tag)?["value"].as_i64()
    }

    /// Tags of the running `config-plugin` instances.
    pub fn running(&self) -> Vec<String> {
        let tags = Arc::new(Mutex::new(Vec::new()));
        self.root.emit(&Running(tags.clone()));
        tags.lock().clone()
    }

    pub fn applies(&self, tag: &str) -> usize {
        self.applied.lock().iter().filter(|applied| *applied == tag).count()
    }

    pub fn text(&self) -> String {
        std::fs::read_to_string(&self.filename).unwrap()
    }

    pub fn read(&self) -> Vec<Value> {
        read(&self.filename)
    }

    pub fn write(&self, data: impl Into<Content>) {
        std::fs::write(&self.filename, data.into().0).unwrap();
    }

    pub fn logs(&self, level: Level) -> Vec<String> {
        self.messages.0.lock().iter().filter(|m| m.level == level).map(|m| m.text.clone()).collect()
    }

    pub async fn update(&self, local: &str, patch: Value) {
        self.loader.update(&format!("{}:{local}", self.id), patch, None, None).await.unwrap();
    }

    pub async fn dispose(&self) {
        self.root.fiber().dispose().await.unwrap();
    }

    pub async fn wait_for(&self, mut condition: impl FnMut(&Self) -> bool) {
        for _ in 0..100 {
            if condition(self) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("condition not met");
    }
}
