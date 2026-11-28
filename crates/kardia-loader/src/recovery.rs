use std::future::Future;
use std::time::Duration;

use kardia_core::{Backoff, Error, Failure, Issue, PathSegment, RecoveryAction, RecoveryPolicy, Result};
use serde_json::Value;

use crate::Loader;

/// Entry-level recovery configuration. Missing configuration keeps the entry failed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RecoveryConfig {
    #[default]
    Manual,
    Retry {
        delay_ms: u64,
        /// How many restarts are attempted before the entry stays failed.
        max_retries: Option<u32>,
        backoff: Backoff,
    },
    Delays {
        /// The wait before the n-th automatic restart; the list's end is the limit.
        delays_ms: Vec<u64>,
        /// How long one restart is counted before it no longer uses up the next delay.
        window_ms: Option<u64>,
    },
    Host {
        handler: String,
    },
}

impl RecoveryConfig {
    pub(crate) fn read(value: Option<&Value>) -> Self {
        let Some(value) = value else { return Self::Manual };
        match value["mode"].as_str() {
            Some("retry") => Self::Retry {
                delay_ms: value["delayMs"].as_u64().unwrap_or(1000),
                max_retries: value["maxRetries"].as_u64().map(|limit| limit.min(u32::MAX as u64) as u32),
                backoff: match value["backoff"].as_str() {
                    Some("exponential") => Backoff::Exponential,
                    _ => Backoff::Fixed,
                },
            },
            Some("host") => Self::Host { handler: value["handler"].as_str().unwrap_or_default().into() },
            Some("delays") => Self::Delays {
                delays_ms: value["delayMs"]
                    .as_array()
                    .map(|delays| delays.iter().filter_map(Value::as_u64).collect())
                    .unwrap_or_default(),
                window_ms: value["windowMs"].as_u64(),
            },
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
            RecoveryConfig::Retry { delay_ms, max_retries, backoff } => {
                Ok(RecoveryPolicy::retry_with(Duration::from_millis(*delay_ms), *max_retries, *backoff))
            }
            RecoveryConfig::Delays { delays_ms, window_ms } => Ok(RecoveryPolicy::retry_delays(
                delays_ms.iter().map(|delay| Duration::from_millis(*delay)).collect::<Vec<_>>(),
                window_ms.map(Duration::from_millis),
            )),
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
            let mut issues = Vec::new();
            if value.get("delayMs").is_some_and(|v| v.as_u64().is_none_or(|n| n == 0)) {
                issues.extend(issue("delayMs must be a positive integer", Some("delayMs")));
            }
            if value.get("maxRetries").is_some_and(|v| v.as_u64().is_none()) {
                issues.extend(issue("maxRetries must be a non-negative integer", Some("maxRetries")));
            }
            if value.get("backoff").is_some_and(|v| !matches!(v.as_str(), Some("fixed" | "exponential"))) {
                issues.extend(issue("backoff must be fixed or exponential", Some("backoff")));
            }
            issues
        }
        Some("host") => {
            if value["handler"].as_str().is_none_or(str::is_empty) {
                issue("handler must name a registered host callback", Some("handler"))
            } else {
                Vec::new()
            }
        }
        Some("delays") => {
            let mut issues = Vec::new();
            let delays = value["delayMs"].as_array();
            if delays.is_none_or(|delays| {
                delays.is_empty() || delays.iter().any(|delay| delay.as_u64().is_none_or(|n| n == 0))
            }) {
                issues.extend(issue("delayMs must be a non-empty list of positive integers", Some("delayMs")));
            }
            if value.get("windowMs").is_some_and(|v| v.as_u64().is_none_or(|n| n == 0)) {
                issues.extend(issue("windowMs must be a positive integer", Some("windowMs")));
            }
            issues
        }
        _ => issue("mode must be manual, retry, delays or host", Some("mode")),
    }
}
