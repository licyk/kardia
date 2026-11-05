//! Navigation in nested entry lists (JSON values; group members live in
//! the group's `config`).

use serde_json::Value;

pub(crate) fn id_of(options: &Value) -> Option<&str> {
    options.get("id").and_then(Value::as_str).filter(|id| !id.is_empty())
}

pub(crate) fn group_list(options: &Value) -> Option<&Vec<Value>> {
    (options.get("group") == Some(&Value::Bool(true))).then(|| options.get("config")?.as_array())?
}

fn group_list_mut(options: &mut Value) -> Option<&mut Vec<Value>> {
    if options.get("group") != Some(&Value::Bool(true)) {
        return None;
    }
    options.get_mut("config")?.as_array_mut()
}

/// Where an entry is, and the group (`None`: the list itself) it is in.
#[derive(Debug, Clone, PartialEq)]
pub struct Flat {
    pub parent: Option<String>,
    pub position: usize,
    pub options: Value,
}

/// Indexes every entry (nested group members included) by id.
pub fn flatten(data: &[Value]) -> indexmap::IndexMap<String, Flat> {
    fn walk(data: &[Value], parent: Option<&str>, result: &mut indexmap::IndexMap<String, Flat>) {
        for (position, options) in data.iter().enumerate() {
            if let Some(id) = id_of(options) {
                let flat = Flat { parent: parent.map(String::from), position, options: options.clone() };
                result.insert(id.to_string(), flat);
            }
            if let Some(list) = group_list(options) {
                walk(list, id_of(options), result);
            }
        }
    }
    let mut result = indexmap::IndexMap::new();
    walk(data, None, &mut result);
    result
}

/// Index path of an entry: `[2, 0]` is the first member of the third entry's group.
pub(crate) fn find(data: &[Value], id: &str) -> Option<Vec<usize>> {
    for (index, options) in data.iter().enumerate() {
        if id_of(options) == Some(id) {
            return Some(vec![index]);
        }
        if let Some(list) = group_list(options)
            && let Some(mut path) = find(list, id)
        {
            path.insert(0, index);
            return Some(path);
        }
    }
    None
}

pub(crate) fn get_mut<'a>(data: &'a mut [Value], path: &[usize]) -> Option<&'a mut Value> {
    let (first, rest) = path.split_first()?;
    let options = data.get_mut(*first)?;
    if rest.is_empty() {
        return Some(options);
    }
    get_mut(group_list_mut(options)?, rest)
}

/// The list containing the entry at `path`.
fn list_mut<'a>(data: &'a mut Vec<Value>, path: &[usize]) -> Option<&'a mut Vec<Value>> {
    match path.split_last() {
        Some((_, [])) => Some(data),
        Some((_, parent)) => group_list_mut(get_mut(data, parent)?),
        None => None,
    }
}

pub(crate) fn entry_mut<'a>(data: &'a mut [Value], id: &str) -> Option<&'a mut Value> {
    let path = find(data, id)?;
    get_mut(data, &path)
}

/// Removes an entry, returning its options.
pub(crate) fn detach(data: &mut Vec<Value>, id: &str) -> Option<Value> {
    let path = find(data, id)?;
    let index = *path.last()?;
    Some(list_mut(data, &path)?.remove(index))
}

/// Inserts under `parent` (a group id, or `None` for the list itself) at
/// `position`, clamped. Returns false when the group does not exist.
pub(crate) fn place(data: &mut Vec<Value>, options: Value, parent: Option<&str>, position: usize) -> bool {
    let list = match parent {
        None => data,
        Some(parent) => {
            let Some(group) = entry_mut(data, parent) else { return false };
            if group.get("group") != Some(&Value::Bool(true)) {
                return false;
            }
            let config =
                group.as_object_mut().expect("entries are objects").entry("config").or_insert(Value::Array(Vec::new()));
            if !config.is_array() {
                *config = Value::Array(Vec::new());
            }
            config.as_array_mut().expect("just made an array")
        }
    };
    list.insert(position.min(list.len()), options);
    true
}
