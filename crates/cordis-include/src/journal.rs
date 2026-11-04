//! Tree-side changes that have not reached the file yet (`journal.ts`).

use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::tree::{Flat, detach, entry_mut, flatten, place};

/// Per-key delta; `None` deletes the key.
pub type Changes = IndexMap<String, Option<Value>>;

#[derive(Debug, Clone, PartialEq)]
pub enum Record {
    Remove,
    Upsert {
        /// The entry did not exist when the tree first reported it.
        created: bool,
        /// The group the tree placed the entry in (`Some(None)`: the root),
        /// or `None` when it was not relocated.
        parent: Option<Option<String>>,
        /// Index within `parent`; only meaningful alongside a `parent`.
        position: usize,
        changes: Changes,
    },
}

/// Records by entry id, in report order.
pub type Journal = IndexMap<String, Record>;

pub fn diff_options(legacy: &Value, options: &Value) -> Changes {
    let empty = Map::new();
    let (legacy, options) = (legacy.as_object().unwrap_or(&empty), options.as_object().unwrap_or(&empty));
    let mut changes = Changes::new();
    for key in legacy.keys().chain(options.keys()) {
        if key == "id" || changes.contains_key(key) || legacy.get(key) == options.get(key) {
            continue;
        }
        changes.insert(key.clone(), options.get(key).cloned());
    }
    changes
}

/// Folds a later record into an earlier one for the same entry.
pub fn merge_records(older: Option<Record>, newer: Record) -> Option<Record> {
    let Some(older) = older else { return Some(newer) };
    match (older, newer) {
        // an entry created and removed before ever being written leaves no trace
        (Record::Upsert { created: true, .. }, Record::Remove) => None,
        (_, Record::Remove) => Some(Record::Remove),
        (Record::Remove, Record::Upsert { parent, position, changes, .. }) => {
            Some(Record::Upsert { created: false, parent, position, changes })
        }
        (
            Record::Upsert { created, parent: old_parent, position: old_position, changes: mut merged },
            Record::Upsert { parent, position, changes, .. },
        ) => {
            merged.extend(changes);
            let (parent, position) = if parent.is_some() { (parent, position) } else { (old_parent, old_position) };
            Some(Record::Upsert { created, parent, position, changes: merged })
        }
    }
}

pub fn merge(journal: &mut Journal, id: &str, record: Record) {
    if let Some(merged) = merge_records(journal.shift_remove(id), record) {
        journal.insert(id.to_string(), merged);
    }
}

/// Records one change reported by the tree. `parent` is the group the
/// entry now lives in (`None`: the root) and `position` its index there.
pub fn record(
    journal: &mut Journal,
    id: &str,
    options: Option<&Value>,
    legacy: Option<&Value>,
    moved: bool,
    parent: Option<String>,
    position: usize,
) {
    let record = match (options, legacy) {
        (None, _) => Record::Remove,
        (Some(options), None) => {
            let changes = options
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(key, _)| *key != "id")
                .map(|(key, value)| (key.clone(), Some(value.clone())))
                .collect();
            Record::Upsert { created: true, parent: Some(parent), position, changes }
        }
        (Some(options), Some(legacy)) => Record::Upsert {
            created: false,
            parent: moved.then_some(parent),
            position,
            changes: diff_options(legacy, options),
        },
    };
    merge(journal, id, record);
}

pub fn apply_changes(options: &mut Value, changes: &Changes, mut filter: impl FnMut(&str) -> bool) {
    let Some(map) = options.as_object_mut() else { return };
    for (key, value) in changes {
        if !filter(key) {
            continue;
        }
        match value {
            None => {
                map.shift_remove(key);
            }
            Some(value) => {
                map.insert(key.clone(), value.clone());
            }
        }
    }
}

/// Overlays a journal onto `data`. `filter(id, key)` restricts which changes
/// apply; `key` is `None` when asking about the entry as a whole.
pub fn apply_journal(
    mut data: Vec<Value>,
    journal: &Journal,
    filter: impl Fn(&str, Option<&str>) -> bool,
    warn: &dyn Fn(String),
) -> Vec<Value> {
    for (id, record) in journal {
        if !filter(id, None) {
            continue;
        }
        let Record::Upsert { parent, position, changes, .. } = record else {
            detach(&mut data, id);
            continue;
        };
        let Some(flat) = flatten(&data).shift_remove(id) else {
            let mut options = Value::Object(Map::from_iter([("id".to_string(), Value::String(id.clone()))]));
            apply_changes(&mut options, changes, |key| filter(id, Some(key)));
            let parent = parent.clone().flatten();
            if !place(&mut data, options, parent.as_deref(), *position) {
                warn(format!("cannot place entry {id}: group {} not found", parent.unwrap_or_default()));
            }
            continue;
        };
        if let Some(options) = entry_mut(&mut data, id) {
            apply_changes(options, changes, |key| filter(id, Some(key)));
        }
        if let Some(target) = parent
            && *target != flat.parent
        {
            let options = detach(&mut data, id).expect("found above");
            if !place(&mut data, options.clone(), target.as_deref(), *position) {
                warn(format!("cannot move entry {id}: group {} not found", target.clone().unwrap_or_default()));
                place(&mut data, options, flat.parent.as_deref(), flat.position);
            }
        }
    }
    data
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub id: String,
    pub reason: String,
}

/// Three-way reconciliation of a journal against a new file state. The
/// file wins every conflict: conflicting keys are dropped from the journal
/// so that the tree follows the file, and each drop is reported.
/// `file_owned(id, key)` scopes the check to what the file expresses.
pub fn reconcile(
    journal: &mut Journal,
    base: &IndexMap<String, Flat>,
    theirs: &IndexMap<String, Flat>,
    file_owned: impl Fn(&str, Option<&str>) -> bool,
) -> Vec<Conflict> {
    let mut conflicts = Vec::new();
    let mut conflict = |id: &str, reason: String| conflicts.push(Conflict { id: id.to_string(), reason });
    let ids: Vec<String> = journal.keys().cloned().collect();
    for id in ids {
        if !file_owned(&id, None) {
            continue;
        }
        let (b, t) = (base.get(&id), theirs.get(&id));
        let record = journal.get_mut(&id).expect("listed above");
        let Record::Upsert { created, parent, changes, .. } = record else {
            match (b, t) {
                (_, None) => {}
                (Some(b), Some(t)) if b.options == t.options => continue,
                (b, Some(_)) => {
                    let reason = if b.is_some() {
                        "modified in file, removed at runtime"
                    } else {
                        "added in file, removed at runtime"
                    };
                    conflict(&id, reason.into());
                }
            }
            journal.shift_remove(&id);
            continue;
        };
        let Some(t) = t else {
            if b.is_some() {
                conflict(&id, "removed in file, modified at runtime".into());
                journal.shift_remove(&id);
            }
            // else: created at runtime, nothing in the file to conflict with
            continue;
        };
        *created = false;
        let keys: Vec<String> = changes.keys().cloned().collect();
        for key in keys {
            if !file_owned(&id, Some(&key)) {
                continue;
            }
            let base_value = b.and_then(|b| b.options.get(&key));
            let their_value = t.options.get(&key);
            if base_value == their_value || their_value == changes[&key].as_ref() {
                continue;
            }
            conflict(&id, format!("key \"{key}\" modified both in file and at runtime"));
            changes.shift_remove(&key);
        }
        if let (Some(target), Some(b)) = (parent.as_ref(), b)
            && t.parent != b.parent
            && t.parent != *target
        {
            conflict(&id, "moved both in file and at runtime".into());
            *parent = None;
        }
        let moved = parent.as_ref().is_some_and(|target| *target != t.parent);
        if changes.is_empty() && !moved {
            journal.shift_remove(&id);
        }
    }
    conflicts
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn upsert(changes: Value, parent: Option<Option<&str>>, created: bool) -> Record {
        let changes =
            changes.as_object().unwrap().iter().map(|(k, v)| (k.clone(), (!v.is_null()).then(|| v.clone()))).collect();
        Record::Upsert { created, parent: parent.map(|p| p.map(String::from)), position: 0, changes }
    }

    #[test]
    fn merges_records() {
        assert_eq!(merge_records(Some(upsert(json!({}), Some(None), true)), Record::Remove), None);
        assert_eq!(merge_records(Some(upsert(json!({}), None, false)), Record::Remove), Some(Record::Remove));
        assert_eq!(
            merge_records(Some(Record::Remove), upsert(json!({ "a": 1 }), None, true)),
            Some(upsert(json!({ "a": 1 }), None, false))
        );
        assert_eq!(
            merge_records(
                Some(upsert(json!({ "a": 1, "b": 1 }), Some(Some("g")), true)),
                upsert(json!({ "b": 2 }), None, false)
            ),
            Some(upsert(json!({ "a": 1, "b": 2 }), Some(Some("g")), true))
        );
    }

    #[test]
    fn applies_a_journal() {
        let data = vec![
            json!({ "id": "a", "name": "foo" }),
            json!({ "id": "g", "name": "group", "group": true, "config": [] }),
        ];
        let mut journal = Journal::new();
        journal.insert("a".into(), upsert(json!({ "config": { "x": 1 } }), Some(Some("g")), false));
        journal.insert("b".into(), upsert(json!({ "name": "bar" }), Some(None), true));
        let result = apply_journal(data, &journal, |_, _| true, &|_| {});
        assert_eq!(
            result,
            vec![
                json!({ "id": "b", "name": "bar" }),
                json!({ "id": "g", "name": "group", "group": true, "config": [{ "id": "a", "name": "foo", "config": { "x": 1 } }] }),
            ]
        );
    }

    #[test]
    fn the_file_wins_conflicts() {
        let base = flatten(&[json!({ "id": "a", "name": "foo", "config": 1 }), json!({ "id": "b", "name": "foo" })]);
        let theirs = flatten(&[json!({ "id": "a", "name": "foo", "config": 2 })]);
        let mut journal = Journal::new();
        journal.insert("a".into(), upsert(json!({ "config": 3, "disabled": true }), None, false));
        journal.insert("b".into(), upsert(json!({ "config": 3 }), None, false));
        journal.insert("c".into(), upsert(json!({ "name": "new" }), Some(None), true));
        let conflicts = reconcile(&mut journal, &base, &theirs, |_, _| true);
        let reasons: Vec<_> = conflicts.iter().map(|c| format!("{}: {}", c.id, c.reason)).collect();
        assert_eq!(
            reasons,
            vec!["a: key \"config\" modified both in file and at runtime", "b: removed in file, modified at runtime"]
        );
        assert_eq!(journal.keys().collect::<Vec<_>>(), vec!["a", "c"]);
        assert_eq!(journal["a"], upsert(json!({ "disabled": true }), None, false));
    }
}
