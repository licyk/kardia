use std::future::Future;
use std::time::Duration;

use kardia_core::{Error, Failure, Issue, PathSegment, RecoveryAction, RecoveryPolicy, Result};
use serde_json::Value;

use crate::Loader;

/// Entry-level recovery configuration. Missing configuration keeps the entry failed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RecoveryConfig {
    #[default]
    Manual,
    Retry {
        delay_ms: u64,
    },
    Host {
        handler: String,
    },
}

impl RecoveryConfig {
    pub(crate) fn read(value: Option<&Value>) -> Self {
        let Some(value) = value else { return Self::Manual };
        match value["mode"].as_str() {
            Some("retry") => Self::Retry { delay_ms: value["delayMs"].as_u64().unwrap_or(1000) },
            Some("host") => Self::Host { handler: value["handler"].as_str().unwrap_or_default().into() },
            _ => Self::Manual,
        }
    }
}

impl Loader {
    /// Registers a host callback before entries select it by name. Existing policies
    /// change when their entry's recovery option is updated.
    pub fn register_recovery_handler<F, Fut>(&self, name: impl Into<String>, handler: F)
    where
        F: Fn(Failure) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<RecoveryAction>> + Send + 'static,
    {
        self.0.recovery_handlers.lock().insert(name.into(), RecoveryPolicy::host(handler));
    }

    pub(crate) fn recovery_policy(&self, config: &RecoveryConfig) -> Result<RecoveryPolicy> {
        match config {
            RecoveryConfig::Manual => Ok(RecoveryPolicy::Manual),
            RecoveryConfig::Retry { delay_ms } => Ok(RecoveryPolicy::Retry { delay: Duration::from_millis(*delay_ms) }),
            RecoveryConfig::Host { handler } => self
                .0
                .recovery_handlers
                .lock()
                .get(handler)
                .cloned()
                .ok_or_else(|| Error::msg(format!("unknown recovery handler: {handler}"))),
        }
    }
}

pub(crate) fn validate(value: &Value) -> Vec<Issue> {
    let issue = |message: &str, key: Option<&str>| {
        let mut path = vec![PathSegment::Key("recovery".into())];
        if let Some(key) = key {
            path.push(PathSegment::Key(key.into()));
        }
        vec![Issue::new(message).at(path)]
    };
    if !value.is_object() {
        return issue("recovery must be an object", None);
    }
    match value["mode"].as_str() {
        Some("manual") => Vec::new(),
        Some("retry") => {
            if value.get("delayMs").is_some_and(|v| v.as_u64().is_none_or(|n| n == 0)) {
                issue("delayMs must be a positive integer", Some("delayMs"))
            } else {
                Vec::new()
            }
        }
        Some("host") => {
            if value["handler"].as_str().is_none_or(str::is_empty) {
                issue("handler must name a registered host callback", Some("handler"))
            } else {
                Vec::new()
            }
        }
        _ => issue("mode must be manual, retry or host", Some("mode")),
    }
}
