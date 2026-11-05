//! Patches in the include's own configuration (`patch.ts`):
//! `{ insert: [...] }` / `{ id, insert: [...] }` appends entries (to the
//! root or a group); `{ id, name?, ...keys }` overrides keys of an entry.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::journal::{Journal, Record, apply_changes};
use crate::tree::{detach, find, flatten, get_mut, group_list, id_of, place};

/// Who supplies an entry, or one key of it, in the patched tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    File,
    /// A patch overriding the key.
    Patch(usize),
    /// An entry written in `patches[index][key]` (`insert` or `config`).
    Embedded(usize, &'static str),
}

/// Where every entry and key of a patched tree comes from.
#[derive(Debug, Clone, Default)]
pub struct PatchIndex {
    owners: HashMap<String, Owner>,
    keys: HashMap<String, HashMap<String, usize>>,
}

impl PatchIndex {
    pub fn entry(&self, id: Option<&str>) -> Owner {
        id.and_then(|id| self.owners.get(id)).copied().unwrap_or(Owner::File)
    }

    pub fn key(&self, id: &str, key: &str) -> Owner {
        match self.keys.get(id).and_then(|keys| keys.get(key)) {
            Some(index) => Owner::Patch(*index),
            None => self.entry(Some(id)),
        }
    }

    pub fn file_owned(&self, id: &str, key: Option<&str>) -> bool {
        match key {
            None => self.entry(Some(id)) == Owner::File,
            Some(key) => self.key(id, key) == Owner::File,
        }
    }
}

/// Gives every entry (nested groups included) an id, in place.
pub fn ensure_ids(entries: &mut [Value], used: &mut dyn FnMut(&str) -> bool) {
    for entry in entries {
        if id_of(entry).is_none()
            && let Some(map) = entry.as_object_mut()
        {
            let id = random_id(used);
            let rest = std::mem::take(map);
            map.insert("id".into(), Value::String(id));
            map.extend(rest);
        }
        if entry.get("group") == Some(&Value::Bool(true))
            && let Some(Value::Array(list)) = entry.get_mut("config")
        {
            ensure_ids(list, used);
        }
    }
}

/// Runtime changes are routed back to an inserted entry by its id, so every insert needs one.
pub fn ensure_insert_ids(patches: &mut [Value], used: &mut dyn FnMut(&str) -> bool) {
    for patch in patches {
        if let Some(Value::Array(insert)) = patch.get_mut("insert") {
            ensure_ids(insert, used);
        }
    }
}

pub(crate) fn random_id(used: &mut dyn FnMut(&str) -> bool) -> String {
    use std::hash::{BuildHasher, RandomState};
    loop {
        let id = format!("{:08x}", RandomState::new().hash_one(0u8) as u32);
        if !used(&id) {
            return id;
        }
    }
}

pub struct Patched {
    pub data: Vec<Value>,
    pub index: PatchIndex,
}

/// Overlays `patches` onto a copy of `data`, building the ownership index
/// in the same pass (a patch that does not apply owns nothing). Anonymous
/// entries of a group config a patch supplies get their ids on the patch
/// itself, so that they keep them on the next application.
pub fn apply_patches(data: &[Value], patches: &mut [Value], warn: &dyn Fn(String)) -> Patched {
    let mut data = data.to_vec();
    let mut index = PatchIndex::default();
    // where each mounted entry lives in `data`
    let mut paths: HashMap<String, Vec<usize>> = HashMap::new();
    mount(&data, &[], Owner::File, &mut index, &mut paths);

    for (position, raw) in patches.iter_mut().enumerate() {
        let Some(patch) = raw.as_object() else { continue };
        let id = patch.get("id").and_then(Value::as_str);
        let target = id.and_then(|id| paths.get(id).cloned());

        if let Some(insert) = patch.get("insert") {
            let insert = insert.as_array().cloned().unwrap_or_default();
            let list_path = match (id, &target) {
                (Some(id), None) => {
                    warn(format!("patch insert: entry {id} not found"));
                    continue;
                }
                (Some(id), Some(path)) => {
                    let group = get_mut(&mut data, path).expect("mounted");
                    if group.get("group") != Some(&Value::Bool(true)) {
                        warn(format!("patch insert: entry {id} is not a group"));
                        continue;
                    }
                    let config =
                        group.as_object_mut().expect("object").entry("config").or_insert(Value::Array(Vec::new()));
                    if !config.is_array() {
                        *config = Value::Array(Vec::new());
                    }
                    let list = config.as_array_mut().expect("array");
                    let start = list.len();
                    list.extend(insert.iter().cloned());
                    Some((path.clone(), start))
                }
                (None, _) => {
                    let start = data.len();
                    data.extend(insert.iter().cloned());
                    Some((Vec::new(), start))
                }
            };
            if let Some((parent, start)) = list_path {
                for (offset, options) in insert.iter().enumerate() {
                    let mut path = parent.clone();
                    path.push(start + offset);
                    mount(
                        std::slice::from_ref(options),
                        &path[..path.len() - 1],
                        Owner::Embedded(position, "insert"),
                        &mut index,
                        &mut paths,
                    );
                    remap(&mut paths, options, &path);
                }
            }
            continue;
        }

        let Some(id) = id.map(String::from) else {
            warn("patch: id is required for non-insert patches".into());
            continue;
        };
        let Some(path) = target else {
            warn(format!("patch: entry {id} not found"));
            continue;
        };
        let options = get_mut(&mut data, &path).expect("mounted");
        if let Some(name) = patch.get("name").and_then(Value::as_str)
            && options.get("name").and_then(Value::as_str) != Some(name)
        {
            let actual = options.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
            warn(format!("patch: name mismatch for {id} (expected {actual}, got {name}), skipping"));
            continue;
        }
        let replaces_config = patch.contains_key("config") && group_list(options).is_some();
        // the entries of a group config the patch replaces go away with it
        if replaces_config {
            for member in flatten(group_list(options).expect("checked")).keys() {
                index.owners.remove(member);
                index.keys.remove(member);
                paths.remove(member);
            }
            if let Some(Value::Array(list)) = raw.get_mut("config") {
                ensure_ids(list, &mut |id| paths.contains_key(id));
            }
        }
        let patch = raw.as_object().expect("checked above");
        let overridden = index.keys.entry(id).or_default();
        let map = options.as_object_mut().expect("object");
        for (key, value) in patch.iter().filter(|(key, _)| *key != "id" && *key != "name" && *key != "insert") {
            map.insert(key.clone(), value.clone());
            overridden.insert(key.clone(), position);
        }
        // entries of a group config the patch supplies can be patched in turn
        if patch.contains_key("config") && group_list(options).is_some() {
            let members = group_list(options).expect("checked").clone();
            let mut member_path = path.clone();
            member_path.push(0);
            for (offset, member) in members.iter().enumerate() {
                *member_path.last_mut().expect("pushed") = offset;
                mount(std::slice::from_ref(member), &path, Owner::Embedded(position, "config"), &mut index, &mut paths);
                remap(&mut paths, member, &member_path);
            }
        }
    }
    Patched { data, index }
}

/// Registers the entries of `list` (whose container is at `parent`) and their members.
fn mount(
    list: &[Value],
    parent: &[usize],
    owner: Owner,
    index: &mut PatchIndex,
    paths: &mut HashMap<String, Vec<usize>>,
) {
    for (position, options) in list.iter().enumerate() {
        let mut path = parent.to_vec();
        path.push(position);
        if let Some(id) = id_of(options) {
            index.owners.insert(id.to_string(), owner);
            // earlier overrides were aimed at the entry this one replaces
            index.keys.remove(id);
            paths.insert(id.to_string(), path.clone());
        }
        if let Some(members) = group_list(options) {
            mount(members, &path, owner, index, paths);
        }
    }
}

/// Fixes the path of an entry mounted from a one-element slice, and of its members.
fn remap(paths: &mut HashMap<String, Vec<usize>>, options: &Value, path: &[usize]) {
    if let Some(id) = id_of(options) {
        paths.insert(id.to_string(), path.to_vec());
    }
    if let Some(members) = group_list(options) {
        for (position, member) in members.iter().enumerate() {
            let mut member_path = path.to_vec();
            member_path.push(position);
            remap(paths, member, &member_path);
        }
    }
}

/// Routes a journal to its owners: file-owned changes go into `data`,
/// patch-owned ones into `patches`. Returns whether any patch changed.
pub fn route_journal(journal: &Journal, data: &mut Vec<Value>, patches: &mut [Value], warn: &dyn Fn(String)) -> bool {
    let mut index = apply_patches(data, patches, &|_| {}).index;
    let mut patched = false;
    for (id, record) in journal {
        let owner = index.entry(Some(id));
        let embedded = matches!(owner, Owner::Embedded(..));
        let base = match owner {
            Owner::Embedded(i, _) => patches[i].get("id").and_then(Value::as_str).map(String::from),
            _ => None,
        };
        let Record::Upsert { parent, position, changes, .. } = record else {
            detach(container(data, patches, owner), id);
            patched |= embedded;
            index = apply_patches(data, patches, &|_| {}).index;
            continue;
        };
        let current = flatten(container(data, patches, owner)).shift_remove(id);
        let Some(current) = current else {
            let mut options = Value::Object(Map::from_iter([("id".to_string(), Value::String(id.clone()))]));
            apply_changes(&mut options, changes, |_| true);
            patched |= place_at(data, patches, &index, options, parent.clone().flatten(), *position, warn);
            index = apply_patches(data, patches, &|_| {}).index;
            continue;
        };
        // an entry at the top of a patch list belongs to the group the patch targets
        let effective = current.parent.clone().or(base);
        if let Some(target) = parent
            && effective != *target
        {
            // moving across owners: carry the full options over
            let mut options = detach(container(data, patches, owner), id).expect("found above");
            patched |= embedded;
            apply_changes(&mut options, changes, |_| true);
            patched |= place_at(data, patches, &index, options, target.clone(), *position, warn);
            index = apply_patches(data, patches, &|_| {}).index;
            continue;
        }
        let mut patch_changes: HashMap<usize, Map<String, Value>> = HashMap::new();
        let list = container(data, patches, owner);
        let path = find(list, id).expect("found above");
        let options = get_mut(list, &path).expect("found above");
        apply_changes(options, changes, |key| match index.key(id, key) {
            Owner::Patch(i) => {
                let value = changes[key].clone().unwrap_or(Value::Null);
                patch_changes.entry(i).or_default().insert(key.to_string(), value);
                false
            }
            _ => true,
        });
        patched |= embedded;
        for (i, changes) in patch_changes {
            if let Some(patch) = patches[i].as_object_mut() {
                for (key, value) in changes {
                    if value.is_null() {
                        patch.shift_remove(&key);
                    } else {
                        patch.insert(key, value);
                    }
                }
            }
            patched = true;
        }
    }
    patched
}

/// The list an entry owned by `owner` is written in.
fn container<'a>(data: &'a mut Vec<Value>, patches: &'a mut [Value], owner: Owner) -> &'a mut Vec<Value> {
    match owner {
        Owner::Embedded(i, key) => {
            let patch = patches[i].as_object_mut().expect("patches are objects");
            let list = patch.entry(key).or_insert(Value::Array(Vec::new()));
            if !list.is_array() {
                *list = Value::Array(Vec::new());
            }
            list.as_array_mut().expect("array")
        }
        _ => data,
    }
}

/// Places an entry under `parent`, in the list a patch supplies for it if
/// any. Returns whether a patch changed.
fn place_at(
    data: &mut Vec<Value>,
    patches: &mut [Value],
    index: &PatchIndex,
    options: Value,
    parent: Option<String>,
    position: usize,
    warn: &dyn Fn(String),
) -> bool {
    if let Some(parent) = &parent {
        match index.key(parent, "config") {
            Owner::File => {}
            Owner::Patch(i) => {
                if let Some(Value::Array(list)) = patches[i].get_mut("config") {
                    list.insert(position.min(list.len()), options);
                    return true;
                }
            }
            owner @ Owner::Embedded(..) => {
                let list = container(data, patches, owner);
                if place(list, options.clone(), Some(parent), position) {
                    return true;
                }
            }
        }
    }
    if !place(data, options, parent.as_deref(), position) {
        warn(format!("cannot place entry: group {} not found", parent.unwrap_or_default()));
    }
    false
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn inserts_and_overrides() {
        let data = vec![
            json!({ "id": "a", "name": "foo" }),
            json!({ "id": "g", "name": "group", "group": true, "config": [] }),
        ];
        let mut patches = vec![
            json!({ "insert": [{ "id": "x", "name": "bar" }] }),
            json!({ "id": "g", "insert": [{ "id": "y", "name": "baz" }] }),
            json!({ "id": "a", "config": { "k": 1 } }),
            json!({ "id": "y", "disabled": true }),
            json!({ "id": "a", "name": "wrong", "config": 2 }),
        ];
        let warnings = std::sync::Mutex::new(Vec::new());
        let Patched { data, index } = apply_patches(&data, &mut patches, &|w| warnings.lock().unwrap().push(w));
        assert_eq!(
            data,
            vec![
                json!({ "id": "a", "name": "foo", "config": { "k": 1 } }),
                json!({ "id": "g", "name": "group", "group": true, "config": [{ "id": "y", "name": "baz", "disabled": true }] }),
                json!({ "id": "x", "name": "bar" }),
            ]
        );
        assert_eq!(warnings.lock().unwrap().len(), 1);
        assert_eq!(index.entry(Some("a")), Owner::File);
        assert_eq!(index.key("a", "config"), Owner::Patch(2));
        assert_eq!(index.entry(Some("x")), Owner::Embedded(0, "insert"));
        assert_eq!(index.key("y", "disabled"), Owner::Patch(3));
        assert!(index.file_owned("a", Some("name")) && !index.file_owned("x", None));
    }

    #[test]
    fn routes_changes_to_their_owners() {
        let mut data = vec![json!({ "id": "a", "name": "foo" })];
        let mut patches = vec![json!({ "insert": [{ "id": "x", "name": "bar" }] }), json!({ "id": "a", "config": 1 })];
        let mut journal = Journal::new();
        let change = |changes: Value| Record::Upsert {
            created: false,
            parent: None,
            position: 0,
            changes: changes.as_object().unwrap().iter().map(|(k, v)| (k.clone(), Some(v.clone()))).collect(),
        };
        journal.insert("a".into(), change(json!({ "config": 2, "disabled": true })));
        journal.insert("x".into(), change(json!({ "config": 3 })));
        assert!(route_journal(&journal, &mut data, &mut patches, &|_| {}));
        assert_eq!(data, vec![json!({ "id": "a", "name": "foo", "disabled": true })]);
        assert_eq!(
            patches,
            vec![json!({ "insert": [{ "id": "x", "name": "bar", "config": 3 }] }), json!({ "id": "a", "config": 2 })]
        );
    }
}
