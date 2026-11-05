//! The metadata a module declares: its dependencies, plugin and exported value.

use serde::Serialize;
use serde_json::Value;

/// The metadata of a module.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Meta {
    /// Module paths, relative to this module.
    pub deps: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin: Option<PluginMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PluginMeta {
    pub name: Option<String>,
    /// Services the plugin reads; it starts once they are available.
    pub inject: Vec<String>,
    /// Services the plugin provides.
    pub provide: Vec<String>,
}

impl Meta {
    /// A module exporting a plugin.
    pub fn plugin(name: impl Into<String>) -> Self {
        Self { plugin: Some(PluginMeta { name: Some(name.into()), ..Default::default() }), ..Default::default() }
    }

    /// A module exporting a value, which dependent modules receive as an import.
    pub fn value(value: Value) -> Self {
        Self { value: Some(value), ..Default::default() }
    }

    pub fn dep(mut self, path: impl Into<String>) -> Self {
        self.deps.push(path.into());
        self
    }

    pub fn inject(mut self, key: impl Into<String>) -> Self {
        self.plugin.get_or_insert_default().inject.push(key.into());
        self
    }

    pub fn provide(mut self, key: impl Into<String>) -> Self {
        self.plugin.get_or_insert_default().provide.push(key.into());
        self
    }
}
