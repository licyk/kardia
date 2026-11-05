use std::any::{Any, TypeId};
use std::fmt;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::Value;

/// A decoded, type-erased plugin configuration.
pub type ErasedConfig = Arc<dyn Any + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSegment {
    Key(String),
    Index(usize),
}

impl fmt::Display for PathSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathSegment::Key(key) => f.write_str(key),
            PathSegment::Index(index) => write!(f, "{index}"),
        }
    }
}

/// One validation problem, located by its path inside the configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub path: Vec<PathSegment>,
    pub message: String,
}

impl Issue {
    pub fn new(message: impl Into<String>) -> Self {
        Self { path: Vec::new(), message: message.into() }
    }

    pub fn at(mut self, path: impl IntoIterator<Item = PathSegment>) -> Self {
        self.path = path.into_iter().collect();
        self
    }

    pub fn format_path(&self) -> String {
        self.path.iter().map(ToString::to_string).collect::<Vec<_>>().join(".")
    }
}

/// A rejected configuration (the analogue of Standard Schema issues).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct ValidationError {
    pub issues: Vec<Issue>,
}

impl ValidationError {
    pub fn new(issues: Vec<Issue>) -> Self {
        Self { issues }
    }

    pub fn message(message: impl Into<String>) -> Self {
        Self::new(vec![Issue::new(message)])
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid config:")?;
        for issue in &self.issues {
            if issue.path.is_empty() {
                write!(f, "\n  - {}", issue.message)?;
            } else {
                write!(f, "\n  - {} (at {})", issue.message, issue.format_path())?;
            }
        }
        Ok(())
    }
}

/// Deserializes a configuration value into `T`, reporting the failing path.
///
/// A missing configuration (`null`) is read as an empty object, so that
/// configuration types whose fields all have defaults accept it; `()` accepts
/// anything.
pub fn decode<T: DeserializeOwned + 'static>(value: &Value) -> Result<T, ValidationError> {
    if TypeId::of::<T>() == TypeId::of::<()>() {
        return Ok(serde_json::from_value(Value::Null).expect("unit deserializes from null"));
    }
    match deserialize(value) {
        Err(_) if value.is_null() => deserialize(&Value::Object(Default::default())),
        result => result,
    }
}

fn deserialize<T: DeserializeOwned>(value: &Value) -> Result<T, ValidationError> {
    serde_path_to_error::deserialize(value).map_err(|error| {
        let path = error
            .path()
            .iter()
            .filter_map(|segment| match segment {
                serde_path_to_error::Segment::Seq { index } => Some(PathSegment::Index(*index)),
                serde_path_to_error::Segment::Map { key } => Some(PathSegment::Key(key.clone())),
                serde_path_to_error::Segment::Enum { variant } => Some(PathSegment::Key(variant.clone())),
                serde_path_to_error::Segment::Unknown => None,
            })
            .collect();
        ValidationError::new(vec![Issue { path, message: error.into_inner().to_string() }])
    })
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use serde_json::json;

    use super::*;

    #[test]
    fn formats_issues_with_paths() {
        let error = ValidationError::new(vec![
            Issue::new("not a number").at([PathSegment::Key("items".into()), PathSegment::Index(0)]),
            Issue::new("required"),
        ]);
        assert_eq!(error.to_string(), "invalid config:\n  - not a number (at items.0)\n  - required");
    }

    #[test]
    fn decodes_with_paths_and_defaults() {
        #[derive(Deserialize, Default, Debug)]
        #[serde(default)]
        struct Config {
            items: Vec<u32>,
        }
        let error = decode::<Config>(&json!({ "items": [1, "x"] })).unwrap_err();
        assert_eq!(error.issues[0].format_path(), "items.1");
        assert!(decode::<Config>(&Value::Null).unwrap().items.is_empty());
        assert!(decode::<Config>(&json!({ "unknown": true })).is_ok());
        decode::<()>(&json!({ "anything": 1 })).unwrap();
    }
}
