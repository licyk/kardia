#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use cordis_core::{Context, Event, Exporter, Level, Message, PluginDef};
use cordis_fs::FsPlugin;
use cordis_include::{Include, IncludePlugin, yaml};
use cordis_loader::{Loader, builtins};
use parking_lot::Mutex;
use serde_json::{Value, json};

/// Asks the running `config-plugin` instance tagged `tag` for its config.
pub struct ConfigQuery(pub String);

impl Event for ConfigQuery {
    type Output = Option<Value>;
}

/// `config-plugin`: answers [`ConfigQuery`] for its own tag.
pub fn config_plugin() -> PluginDef {
    PluginDef::new(|ctx, config| async move {
        ctx.on(move |query: &ConfigQuery| (config["tag"].as_str() == Some(&query.0)).then(|| config.clone()))?;
        Ok(())
    })
}

pub fn plugin(id: &str, value: i64) -> Value {
    json!({ "id": id, "name": "config-plugin", "config": { "tag": id, "value": value } })
}

pub fn group(id: &str, config: Vec<Value>) -> Value {
    json!({ "id": id, "name": "group", "group": true, "config": config })
}

pub fn fixture(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cordis-include-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

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
    messages: Messages,
}

impl App {
    /// Boots a loader with one include over a fresh fixture file.
    pub async fn setup(name: &str, data: impl Into<Content>, config: Value) -> Self {
        let filename = fixture(name);
        std::fs::write(&filename, data.into().0).unwrap();
        Self::boot(filename, config).await
    }

    pub async fn boot(filename: PathBuf, config: Value) -> Self {
        let root = Context::new();
        let messages = Messages::default();
        root.add_exporter(Arc::new(messages.clone())).unwrap();
        root.plugin::<FsPlugin>(()).unwrap().settled().await.unwrap();
        let catalog = builtins();
        catalog.register::<IncludePlugin>();
        catalog.register_def("config-plugin", config_plugin());
        root.plugin_def(&Loader::plugin(catalog), Value::Null).unwrap().settled().await.unwrap();
        let loader = (*root.get::<Loader>().unwrap()).clone();
        let mut include = json!({ "path": filename.to_string_lossy() });
        include.as_object_mut().unwrap().extend(config.as_object().cloned().unwrap_or_default());
        let id = loader.create(json!({ "name": "include", "config": include }), None, None).await.unwrap();
        if let Some(fiber) = loader.resolve(&id).unwrap().inner_fiber() {
            let _ = fiber.settled().await;
        }
        let app = Self { root, loader, id, filename, messages };
        app.settle().await;
        app
    }

    pub fn include(&self) -> Include {
        Include::of(&self.loader.resolve(&self.id).unwrap().subtree().unwrap()).unwrap()
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

    pub fn text(&self) -> String {
        std::fs::read_to_string(&self.filename).unwrap()
    }

    pub fn read(&self) -> Vec<Value> {
        match yaml::parse(&self.text()).unwrap().0 {
            Value::Array(entries) => entries,
            other => panic!("not an array: {other}"),
        }
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

/// File content: YAML text, or entries dumped as YAML.
pub struct Content(pub String);

impl From<&str> for Content {
    fn from(text: &str) -> Self {
        Self(text.into())
    }
}

impl From<String> for Content {
    fn from(text: String) -> Self {
        Self(text)
    }
}

impl From<Vec<Value>> for Content {
    fn from(entries: Vec<Value>) -> Self {
        Self(yaml::dump(&Value::Array(entries)))
    }
}
