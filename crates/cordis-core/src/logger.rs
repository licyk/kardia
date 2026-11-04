use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::context::Context;
use crate::effect::EffectHandle;
use crate::error::{AggregateError, Error, Result};
use crate::id_table::IdTable;
use crate::key::FiberId;
use crate::root::RootInner;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(into = "u8", try_from = "u8")]
pub enum Level {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
}

impl From<Level> for u8 {
    fn from(level: Level) -> u8 {
        level as u8
    }
}

impl TryFrom<u8> for Level {
    type Error = String;
    fn try_from(value: u8) -> Result<Self, String> {
        Ok(match value {
            0 => Level::Error,
            1 => Level::Warn,
            2 => Level::Info,
            3 => Level::Debug,
            _ => return Err(format!("invalid log level {value}")),
        })
    }
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Sequence number, shared by all exporters receiving the same call.
    pub sn: u64,
    /// Milliseconds since the Unix epoch.
    pub ts: u64,
    pub name: String,
    pub level: Level,
    pub text: String,
    pub fiber: FiberId,
}

/// A log sink. Registering one is an effect of the registering fiber.
pub trait Exporter: Send + Sync + 'static {
    fn export(&self, message: &Message);

    /// The most verbose level exported for logger `name`; `None` defers to
    /// the logger's own level (default: info).
    fn level(&self, _name: &str) -> Option<Level> {
        None
    }
}

pub(crate) struct LoggerState {
    exporters: IdTable<Arc<dyn Exporter>>,
    buffer: VecDeque<Message>,
    buffer_size: usize,
    sn: u64,
}

impl Default for LoggerState {
    fn default() -> Self {
        Self { exporters: IdTable::new(), buffer: VecDeque::new(), buffer_size: 1000, sn: 0 }
    }
}

#[derive(Default, Deserialize)]
struct LoggerIntercept {
    name: Option<String>,
    level: Option<Level>,
}

/// A named logger bound to the fiber it was created for.
#[derive(Clone)]
pub struct Logger {
    root: Arc<RootInner>,
    name: String,
    level: Option<Level>,
    fiber: FiberId,
}

impl Logger {
    pub(crate) fn internal(root: &Arc<RootInner>, name: String, fiber: FiberId) -> Self {
        Self { root: root.clone(), name: hyphenate(&name), level: None, fiber }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn error(&self, text: impl fmt::Display) {
        self.log(Level::Error, text);
    }

    pub fn warn(&self, text: impl fmt::Display) {
        self.log(Level::Warn, text);
    }

    pub fn info(&self, text: impl fmt::Display) {
        self.log(Level::Info, text);
    }

    pub fn debug(&self, text: impl fmt::Display) {
        self.log(Level::Debug, text);
    }

    /// Logs an error; an aggregate error is logged member by member.
    pub fn log_error(&self, error: &Error) {
        self.log_error_at(Level::Error, error);
    }

    pub fn log_error_at(&self, level: Level, error: &Error) {
        if let Some(aggregate) = error.downcast_ref::<AggregateError>()
            && !aggregate.errors.is_empty()
        {
            for error in &aggregate.errors {
                self.log_error_at(level, error);
            }
            return;
        }
        self.log(level, error);
    }

    pub fn log(&self, level: Level, text: impl fmt::Display) {
        let (sn, exporters) = {
            let mut state = self.root.logger.lock();
            state.sn += 1;
            (state.sn, state.exporters.values().cloned().collect::<Vec<_>>())
        };
        let message = Message {
            sn,
            ts: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
            name: self.name.clone(),
            level,
            text: text.to_string(),
            fiber: self.fiber,
        };
        if level <= self.level.unwrap_or(Level::Info) {
            let mut state = self.root.logger.lock();
            if state.buffer_size > 0 {
                while state.buffer.len() >= state.buffer_size {
                    state.buffer.pop_front();
                }
                state.buffer.push_back(message.clone());
            }
        }
        for exporter in exporters {
            if level <= exporter.level(&self.name).or(self.level).unwrap_or(Level::Info) {
                exporter.export(&message);
            }
        }
    }
}

impl Context {
    /// The logger of this context: named by `intercept("logger", { name })`
    /// or after the fiber's plugin.
    pub fn logger(&self) -> Logger {
        let meta: LoggerIntercept = self.intercept_meta("logger");
        let name = meta.name.unwrap_or_else(|| hyphenate(&self.root.with(|st| st.fiber_name(self.fiber))));
        Logger { root: self.root.clone(), name, level: meta.level, fiber: self.fiber }
    }

    pub fn logger_named(&self, name: impl Into<String>) -> Logger {
        let meta: LoggerIntercept = self.intercept_meta("logger");
        Logger { root: self.root.clone(), name: name.into(), level: meta.level, fiber: self.fiber }
    }

    /// Registers an exporter as an effect of the current fiber.
    pub fn add_exporter(&self, exporter: Arc<dyn Exporter>) -> Result<EffectHandle> {
        let root = Arc::downgrade(&self.root);
        self.effect_sync("ctx.logger.exporter()", |scope| {
            let id = self.root.logger.lock().exporters.insert(exporter);
            scope.defer_fn(move || {
                if let Some(root) = root.upgrade() {
                    let removed = root.logger.lock().exporters.remove(id);
                    drop(removed);
                }
            });
            Ok(())
        })
    }

    /// The most recent messages (bounded ring buffer, oldest first).
    pub fn log_buffer(&self) -> Vec<Message> {
        self.root.logger.lock().buffer.iter().cloned().collect()
    }

    pub fn set_log_buffer_size(&self, size: usize) {
        let mut state = self.root.logger.lock();
        state.buffer_size = size;
        while state.buffer.len() > size {
            state.buffer.pop_front();
        }
    }
}

/// `fooBar` → `foo-bar`, `Qux` → `qux`, `snake_case` → `snake-case`.
pub fn hyphenate(source: &str) -> String {
    let mut result = String::with_capacity(source.len() + 4);
    let mut prev_upper = true;
    for (index, ch) in source.chars().enumerate() {
        if ch == '_' {
            result.push('-');
            prev_upper = false;
        } else if ch.is_ascii_uppercase() {
            if index > 0 && !prev_upper {
                result.push('-');
            }
            result.push(ch.to_ascii_lowercase());
            prev_upper = true;
        } else {
            result.push(ch);
            prev_upper = false;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::hyphenate;

    #[test]
    fn hyphenates() {
        assert_eq!(hyphenate("fooBar"), "foo-bar");
        assert_eq!(hyphenate("Qux"), "qux");
        assert_eq!(hyphenate("foo:driver"), "foo:driver");
        assert_eq!(hyphenate("snake_case"), "snake-case");
    }
}
