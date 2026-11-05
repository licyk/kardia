//! Fixture files: their paths, their content, and reading their entries back.

use std::path::PathBuf;

use kardia_include::yaml;
use serde_json::Value;

/// A path in this test binary's own directory; test names keep files apart.
pub fn fixture(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("kardia-include").join(env!("CARGO_CRATE_NAME"));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

/// Writes a fixture, replacing a file a failed run may have left read-only.
pub fn write_fixture(name: &str, data: impl Into<Content>) -> PathBuf {
    let filename = fixture(name);
    let _ = std::fs::remove_file(&filename);
    std::fs::write(&filename, data.into().0).unwrap();
    filename
}

/// The entries of a YAML / JSON file.
pub fn read(filename: &std::path::Path) -> Vec<Value> {
    match yaml::parse(&std::fs::read_to_string(filename).unwrap()).unwrap().0 {
        Value::Array(entries) => entries,
        other => panic!("not an array: {other}"),
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
