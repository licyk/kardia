use std::fmt;

use kardia_core::{Inject, Issue, PathSegment};
use serde_json::{Map, Value};

/// Separator of composite entry ids across trees (`include:inner:id`).
pub const SEP: char = ':';

/// The key marking an expression object (`{ "__jsExpr": "..." }`, YAML `!!js`).
pub const EXPR_KEY: &str = "__jsExpr";

pub fn is_expr(value: &Value) -> bool {
    value.as_object().is_some_and(|map| map.contains_key(EXPR_KEY))
}

pub fn expr_source(value: &Value) -> Option<&str> {
    value.as_object()?.get(EXPR_KEY)?.as_str()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disabled {
    Bool(bool),
    Expr(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IsolateLabel {
    /// `true`: a realm private to the entry (`svc#id`).
    Private,
    /// A realm shared by every entry using the label (`svc@label`).
    Shared(String),
}

/// A typed view of validated entry options (the JSON object stays the
/// source of truth, so unknown keys survive write-back).
#[derive(Debug, Clone, PartialEq)]
pub struct EntryOptions {
    pub id: String,
    pub name: String,
    pub config: Value,
    pub group: bool,
    pub disabled: Option<Disabled>,
    pub inject: Inject,
    pub intercept: Map<String, Value>,
    pub isolate: Vec<(String, IsolateLabel)>,
}

impl EntryOptions {
    /// Reads options already accepted by [`validate_options`].
    pub fn read(value: &Value) -> Self {
        let get = |key: &str| value.get(key).filter(|v| !v.is_null());
        let text = |key: &str| get(key).and_then(Value::as_str).unwrap_or_default().to_string();
        Self {
            id: text("id"),
            name: text("name"),
            config: get("config").cloned().unwrap_or(Value::Null),
            group: get("group").and_then(Value::as_bool).unwrap_or(false),
            disabled: get("disabled").map(|value| match expr_source(value) {
                Some(source) => Disabled::Expr(source.to_string()),
                None => Disabled::Bool(value.as_bool().unwrap_or(false)),
            }),
            inject: get("inject").map(read_inject).unwrap_or_default(),
            intercept: get("intercept").and_then(Value::as_object).cloned().unwrap_or_default(),
            isolate: get("isolate")
                .and_then(Value::as_object)
                .map(|map| {
                    map.iter()
                        .map(|(name, label)| {
                            let label = match label.as_str() {
                                Some(label) => IsolateLabel::Shared(label.to_string()),
                                None => IsolateLabel::Private,
                            };
                            (name.clone(), label)
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

/// `["a", "b"]` or `{ "a": meta }` (`null` meta means none).
fn read_inject(value: &Value) -> Inject {
    match value {
        Value::Array(names) => names.iter().filter_map(Value::as_str).fold(Inject::new(), Inject::with),
        Value::Object(map) => map.iter().fold(Inject::new(), |inject, (name, meta)| match meta {
            Value::Null => inject.with(name.as_str()),
            meta => inject.with_meta(name.as_str(), meta.clone()),
        }),
        _ => Inject::new(),
    }
}

/// Merges a patch into options: `null` deletes a key.
pub fn merge(target: &mut Value, patch: &Map<String, Value>) {
    let Value::Object(map) = target else { return };
    for (key, value) in patch {
        if value.is_null() {
            map.shift_remove(key);
        } else {
            map.insert(key.clone(), value.clone());
        }
    }
}

/// Reorders keys for stable output: `id`, `name`, the rest alphabetically,
/// `config` last.
pub fn sort_keys(value: &mut Value) {
    let Value::Object(map) = value else { return };
    let mut entries: Vec<(String, Value)> = std::mem::take(map).into_iter().collect();
    let rank = |key: &str| match key {
        "id" => 0,
        "name" => 1,
        "config" => 3,
        _ => 2,
    };
    entries.sort_by(|(a, _), (b, _)| {
        rank(a).cmp(&rank(b)).then_with(|| if rank(a) == 2 { a.cmp(b) } else { std::cmp::Ordering::Equal })
    });
    map.extend(entries);
}

/// Keys whose values differ between two option objects.
pub fn diff_keys(a: &Value, b: &Value) -> Vec<String> {
    let empty = Map::new();
    let (a, b) = (a.as_object().unwrap_or(&empty), b.as_object().unwrap_or(&empty));
    let mut keys: Vec<String> = a.keys().chain(b.keys().filter(|key| !a.contains_key(*key))).cloned().collect();
    keys.retain(|key| a.get(key) != b.get(key));
    keys
}

/// Validates entry options at the boundary. Unknown keys are tolerated.
/// `deep` also validates the children of a group (runtime API input);
/// entries read from a source are checked one by one as their group creates them.
pub fn validate_options(options: &Value, deep: bool) -> Vec<Issue> {
    let Some(map) = options.as_object().filter(|_| !is_expr(options)) else {
        return vec![Issue::new("entry must be an object")];
    };
    let mut issues = Vec::new();
    let mut check = |key: &str, expected: &str, valid: &dyn Fn(&Value) -> bool, required: bool| match map.get(key) {
        None | Some(Value::Null) => {
            if required {
                issues.push(Issue::new(format!("{key} is required")).at([PathSegment::Key(key.into())]));
            }
        }
        Some(value) if !valid(value) => {
            issues.push(Issue::new(format!("{key} must be {expected}")).at([PathSegment::Key(key.into())]));
        }
        _ => {}
    };
    let plain = |value: &Value| value.is_object() && !is_expr(value);
    check("id", "a string", &Value::is_string, false);
    check("name", "a string", &Value::is_string, true);
    check("group", "a boolean", &Value::is_boolean, false);
    check("disabled", "a boolean", &|value| value.is_boolean() || expr_source(value).is_some(), false);
    let is_group = map.get("group") == Some(&Value::Bool(true));
    if is_group {
        check("config", "an array", &Value::is_array, true);
    }
    check("inject", "an array or an object", &|value| value.is_array() || plain(value), false);
    check("intercept", "an object", &plain, false);
    check("isolate", "an object", &plain, false);
    if let Some(id) = map.get("id").and_then(Value::as_str)
        && id.contains(SEP)
    {
        issues.push(Issue::new(format!("id must not contain \"{SEP}\"")).at([PathSegment::Key("id".into())]));
    }
    if let Some(Value::Array(names)) = map.get("inject") {
        for (index, name) in names.iter().enumerate() {
            if !name.is_string() {
                issues.push(
                    Issue::new(format!("inject.{index} must be a string"))
                        .at([PathSegment::Key("inject".into()), PathSegment::Index(index)]),
                );
            }
        }
    }
    if let Some(isolate) = map.get("isolate").filter(|value| plain(value)).and_then(Value::as_object) {
        for (name, label) in isolate {
            if label != &Value::Bool(true) && !label.is_string() {
                issues.push(
                    Issue::new(format!("isolate.{name} must be true or a string"))
                        .at([PathSegment::Key("isolate".into()), PathSegment::Key(name.clone())]),
                );
            }
        }
    }
    if deep
        && is_group
        && let Some(Value::Array(children)) = map.get("config")
    {
        for (index, child) in children.iter().enumerate() {
            for issue in validate_options(child, true) {
                let path = [PathSegment::Key("config".into()), PathSegment::Index(index)].into_iter().chain(issue.path);
                issues.push(Issue::new(issue.message).at(path));
            }
        }
    }
    issues
}

/// Rejected entry options.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct EntryValidationError {
    pub issues: Vec<Issue>,
}

impl fmt::Display for EntryValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid entry options:")?;
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn messages(options: Value) -> Vec<String> {
        validate_options(&options, false).into_iter().map(|issue| issue.message).collect()
    }

    #[test]
    fn accepts_valid_options() {
        assert!(messages(json!({ "name": "foo" })).is_empty());
        assert!(
            messages(json!({
                "id": "a", "name": "foo", "group": false, "disabled": { "__jsExpr": "true" },
                "inject": ["bar"], "intercept": { "bar": {} }, "isolate": { "bar": true, "baz": "label" },
                "config": { "__jsExpr": "whatever" },
            }))
            .is_empty()
        );
        assert!(messages(json!({ "name": "foo", "group": null, "disabled": null, "inject": null })).is_empty());
        assert!(messages(json!({ "name": "foo", "inject": { "bar": { "required": true } } })).is_empty());
        assert!(messages(json!({ "name": "group", "group": true, "config": [] })).is_empty());
    }

    #[test]
    fn rejects_missing_or_invalid_fields() {
        assert_eq!(messages(Value::Null), vec!["entry must be an object"]);
        assert_eq!(messages(json!(["foo"])), vec!["entry must be an object"]);
        assert_eq!(messages(json!({})), vec!["name is required"]);
        assert_eq!(messages(json!({ "id": 1, "name": "foo" })), vec!["id must be a string"]);
        assert_eq!(messages(json!({ "id": "a:b", "name": "foo" })), vec!["id must not contain \":\""]);
        assert_eq!(messages(json!({ "name": "g", "group": true })), vec!["config is required"]);
        assert_eq!(messages(json!({ "name": "g", "group": true, "config": {} })), vec!["config must be an array"]);
        assert_eq!(messages(json!({ "name": "foo", "inject": ["bar", 1] })), vec!["inject.1 must be a string"]);
        assert_eq!(messages(json!({ "name": "foo", "inject": "bar" })), vec!["inject must be an array or an object"]);
        assert_eq!(messages(json!({ "name": "foo", "intercept": [] })), vec!["intercept must be an object"]);
        assert_eq!(
            messages(json!({ "name": "foo", "isolate": { "bar": false } })),
            vec!["isolate.bar must be true or a string"]
        );
    }

    #[test]
    fn accepts_an_expression_only_for_disabled() {
        let expr = json!({ "__jsExpr": "true" });
        assert_eq!(messages(json!({ "name": "foo", "disabled": "yes" })), vec!["disabled must be a boolean"]);
        assert_eq!(messages(json!({ "name": "foo", "disabled": { "__jsExpr": 1 } })).len(), 1);
        let issues = validate_options(
            &json!({ "name": expr, "group": expr, "inject": expr, "intercept": expr, "isolate": expr }),
            false,
        );
        let paths: Vec<_> = issues.iter().map(Issue::format_path).collect();
        assert_eq!(paths, vec!["name", "group", "inject", "intercept", "isolate"]);
    }

    #[test]
    fn validates_group_children_deeply() {
        let options = json!({ "name": "g", "group": true, "config": [{ "name": 1 }] });
        assert!(validate_options(&options, false).is_empty());
        let issues = validate_options(&options, true);
        assert_eq!(issues[0].format_path(), "config.0.name");
        let error = EntryValidationError { issues };
        assert_eq!(error.to_string(), "invalid entry options:\n  - name must be a string (at config.0.name)");
    }

    #[test]
    fn merges_sorts_and_diffs() {
        let mut options = json!({ "config": 1, "name": "foo", "zeta": 1, "id": "a", "alpha": 2 });
        merge(&mut options, json!({ "zeta": null, "beta": 3 }).as_object().unwrap());
        sort_keys(&mut options);
        let keys: Vec<_> = options.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, vec!["id", "name", "alpha", "beta", "config"]);
        assert_eq!(
            diff_keys(&options, &json!({ "id": "a", "name": "foo", "alpha": 2, "config": 2 })),
            vec!["beta", "config"]
        );
    }
}
