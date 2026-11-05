//! Errors of entries pointed at the places in their sources they refer to
//! (`file:line:column`), followed by a frame for the entry and each of its
//! ancestors that a tree backend can locate.

use std::cell::Cell;
use std::fmt::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;

use kardia_core::{Error, Fiber, Issue, PathSegment, ValidationError};
use serde_json::Value;

use crate::catalog::ModuleNotFound;
use crate::expr::ExprError;
use crate::loader::{EntryId, GroupId, Loader, LoaderState, TreeBackend};
use crate::options::EntryValidationError;

/// A position in a source file (1-based line and column, in chars).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub file: PathBuf,
    pub line: usize,
    pub column: usize,
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.file.display(), self.line, self.column)
    }
}

/// The value at `path` inside `value`.
pub fn value_at<'a>(value: &'a Value, path: &[PathSegment]) -> Option<&'a Value> {
    path.iter().try_fold(value, |value, segment| match segment {
        PathSegment::Key(key) => value.get(key),
        PathSegment::Index(index) => value.get(index),
    })
}

impl Loader {
    /// Logs an error of a registered entry.
    pub(crate) fn report(&self, entry: EntryId, error: &Error) {
        let source = self.with(|st| st.entries.get(&entry).map(|data| (data.parent, data.options.clone())));
        let error = match source {
            Some((group, options)) => self.locate_error(group, &options, error),
            None => error.clone(),
        };
        self.0.ctx.logger().log_error(&error);
    }

    /// A failure to load the config written in an entry's options; failures
    /// of configs passed at runtime stay as they are.
    pub(crate) fn locate_config_error(&self, fiber: &Fiber, source: &Value, error: Error) -> Error {
        let Some(entry) = self.own_entry(fiber) else { return error };
        let written = self.with(|st| {
            let data = st.entries.get(&entry)?;
            (data.read().config == *source).then(|| (data.parent, data.options.clone()))
        });
        match written {
            Some((group, options)) => self.locate_error(group, &options, &error),
            None => error,
        }
    }

    /// `error` of the entry with `options` in `group`; unchanged if no part
    /// of it can be located.
    pub(crate) fn locate_error(&self, group: GroupId, options: &Value, error: &Error) -> Error {
        let Some(id) = options.get("id").and_then(Value::as_str) else { return error.clone() };
        let chain = self.with(|st| st.chain(group, id));
        let Some((backend, id)) = chain.first() else { return error.clone() };
        let located = Cell::new(false);
        let found = |location: Option<Location>| {
            located.set(located.get() || location.is_some());
            location
        };
        let at = |path: &[PathSegment], value: Option<&Value>| found(backend.locate(id, path, value));
        let mut text = if let Some(rejected) = error.downcast_ref::<EntryValidationError>() {
            format!("invalid entry options:{}", issues(&rejected.issues, &[], options, at))
        } else if let Some(rejected) = error.downcast_ref::<ValidationError>() {
            let prefix = [PathSegment::Key("config".into())];
            format!("invalid config:{}", issues(&rejected.issues, &prefix, options, at))
        } else if error.is::<ModuleNotFound>() {
            match at(&[PathSegment::Key("name".into())], options.get("name")) {
                Some(location) => format!("{error} (at name, {location})"),
                None => error.to_string(),
            }
        } else if let Some(failed) = error.downcast_ref::<ExprError>() {
            let label = failed.path.iter().map(ToString::to_string).collect::<Vec<_>>().join(".");
            match found(backend.locate_expr(id, &failed.path, &failed.expr, failed.offset)) {
                Some(location) => format!("{error}\n    at {label} ({location})"),
                None => format!("{error}\n    at {label}"),
            }
        } else {
            return error.clone();
        };
        for (backend, id) in &chain {
            if let Some(location) = found(backend.locate(id, &[], None)) {
                let _ = write!(text, "\n    at {id} ({location})");
            }
        }
        if located.get() { Error::msg(text) } else { error.clone() }
    }
}

/// Validation issues, each with the place of the value it refers to.
fn issues(
    issues: &[Issue],
    prefix: &[PathSegment],
    options: &Value,
    at: impl Fn(&[PathSegment], Option<&Value>) -> Option<Location>,
) -> String {
    let mut text = String::new();
    for issue in issues {
        let path: Vec<PathSegment> = prefix.iter().chain(&issue.path).cloned().collect();
        let _ = match (issue.path.is_empty(), at(&path, value_at(options, &path))) {
            (true, None) => write!(text, "\n  - {}", issue.message),
            (true, Some(location)) => write!(text, "\n  - {} (at {location})", issue.message),
            (false, None) => write!(text, "\n  - {} (at {})", issue.message, issue.format_path()),
            (false, Some(location)) => write!(text, "\n  - {} (at {}, {location})", issue.message, issue.format_path()),
        };
    }
    text
}

impl LoaderState {
    /// The backends and local ids of the entry `id` in `group` and of its
    /// ancestors, innermost first.
    fn chain(&self, mut group: GroupId, id: &str) -> Vec<(Arc<dyn TreeBackend>, String)> {
        let mut chain = Vec::new();
        let mut id = id.to_string();
        while let Some(data) = self.groups.get(&group) {
            chain.push((self.trees[&data.tree].backend.clone(), id));
            let Some(owner) = data.owner.and_then(|owner| self.entries.get(&owner)) else { break };
            id = owner.local_id();
            group = owner.parent;
        }
        chain
    }
}
