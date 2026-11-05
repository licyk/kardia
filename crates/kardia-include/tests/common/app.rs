//! The test app: a loader with one include, and the exporter that records its logs.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use kardia_core::{Context, Exporter, Level, Message, PluginDef};
use kardia_fs::FsPlugin;
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

impl App {
    /// Boots a loader with one include over a fresh fixture file.
    pub async fn setup(name: &str, data: impl Into<Content>, config: Value) -> Self {
        Self::boot(write_fixture(name, data), config).await
    }

    pub async fn boot(filename: PathBuf, config: Value) -> Self {
        Self::boot_with(filename, config, PluginDef::of::<IncludePlugin>()).await
    }

    /// Boots with `include` registered as the `include` plugin.
    pub async fn boot_with(filename: PathBuf, config: Value, include: PluginDef) -> Self {
        let root = Context::new();
        let messages = Messages::default();
        root.add_exporter(Arc::new(messages.clone())).unwrap();
        root.plugin::<FsPlugin>(()).unwrap().settled().await.unwrap();
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
