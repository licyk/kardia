use std::collections::HashMap;
use std::sync::Arc;

use futures::FutureExt;
use futures::future::BoxFuture;
use kardia_core::{Error, Plugin, PluginDef};
use parking_lot::RwLock;

/// Turns an entry's `name` into a plugin definition (the analogue of a
/// module import). Resolution may be asynchronous.
pub trait Resolver: Send + Sync + 'static {
    fn resolve(&self, name: &str) -> BoxFuture<'static, Result<PluginDef, Error>>;
}

/// No plugin is registered under the requested name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Cannot find plugin '{name}'")]
pub struct ModuleNotFound {
    pub name: String,
}

/// A static registry of plugins by name.
#[derive(Clone, Default)]
pub struct Catalog {
    plugins: Arc<RwLock<HashMap<String, PluginDef>>>,
}

impl Catalog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a typed plugin under its `NAME`.
    pub fn register<P: Plugin>(&self) -> &Self {
        self.register_def(P::NAME, PluginDef::of::<P>())
    }

    pub fn register_def(&self, name: impl Into<String>, def: PluginDef) -> &Self {
        self.plugins.write().insert(name.into(), def);
        self
    }

    /// Makes `alias` resolve to whatever `name` resolves to.
    pub fn alias(&self, alias: impl Into<String>, name: &str) -> &Self {
        if let Some(def) = self.get(name) {
            self.register_def(alias, def);
        }
        self
    }

    pub fn get(&self, name: &str) -> Option<PluginDef> {
        self.plugins.read().get(name).cloned()
    }
}

impl Resolver for Catalog {
    fn resolve(&self, name: &str) -> BoxFuture<'static, Result<PluginDef, Error>> {
        let result = self.get(name).ok_or_else(|| ModuleNotFound { name: name.to_string() }.into());
        futures::future::ready(result).boxed()
    }
}
