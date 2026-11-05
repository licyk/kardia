//! Test plugins and the entry values that configure them.

use std::sync::Arc;

use kardia_core::{Event, Issue, PathSegment, PluginDef, ValidationError};
use parking_lot::Mutex;
use serde_json::{Value, json};

/// Asks the running `config-plugin` instance tagged `tag` for its config.
pub struct ConfigQuery(pub String);

impl Event for ConfigQuery {
    type Output = Option<Value>;
}

/// Collects the tag of every running `config-plugin` instance.
pub struct Running(pub Arc<Mutex<Vec<String>>>);

impl Event for Running {
    type Output = ();
}

/// `config-plugin`: answers [`ConfigQuery`] for its own tag and records
/// every apply in `applied`, so tests can count restarts.
pub fn config_plugin(applied: Arc<Mutex<Vec<String>>>) -> PluginDef {
    PluginDef::new(move |ctx, config| {
        let applied = applied.clone();
        async move {
            let tag = config["tag"].as_str().unwrap_or_default().to_string();
            applied.lock().push(tag.clone());
            let own = config.clone();
            ctx.on(move |query: &ConfigQuery| (own["tag"].as_str() == Some(&query.0)).then(|| own.clone()))?;
            ctx.on(move |running: &Running| running.0.lock().push(tag.clone()))?;
            Ok(())
        }
    })
}

/// `schema-plugin`: requires a numeric `port` and reports every issue it finds.
pub fn schema_plugin() -> PluginDef {
    PluginDef::new(|_, _| async { Ok(()) }).validate_with(|value| {
        if value.is_null() {
            return Err(ValidationError::message("config is required"));
        }
        let mut issues = Vec::new();
        if !value["port"].is_number() {
            issues.push(Issue::new("port must be a number").at([PathSegment::Key("port".into())]));
        }
        if let Some(index) = value["hosts"].as_array().and_then(|hosts| hosts.iter().position(|host| !host.is_string()))
        {
            issues.push(
                Issue::new("host must be a string").at([PathSegment::Key("hosts".into()), PathSegment::Index(index)]),
            );
        }
        if issues.is_empty() { Ok(value.clone()) } else { Err(ValidationError::new(issues)) }
    })
}

pub fn plugin(id: &str, value: i64) -> Value {
    json!({ "id": id, "name": "config-plugin", "config": { "tag": id, "value": value } })
}

pub fn group(id: &str, config: Vec<Value>) -> Value {
    json!({ "id": id, "name": "group", "group": true, "config": config })
}
