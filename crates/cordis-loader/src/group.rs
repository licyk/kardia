use cordis_core::{Context, Disposer, Error, PluginDef, Result};
use futures::FutureExt;
use futures::future::join_all;
use serde_json::Value;

use crate::loader::{GroupData, GroupId, Loader};
use crate::options::{EntryOptions, EntryValidationError, validate_options};

/// Tag of the group plugin's definition.
pub const GROUP_TAG: &str = "loader:group";

/// The group plugin: its configuration is a list of entries, reconciled
/// in place on every update instead of restarting the plugin.
pub fn group_plugin() -> PluginDef {
    PluginDef::new(|ctx, config| async move {
        let loader = (*ctx.get::<Loader>()?).clone();
        let group = loader.new_subgroup(&ctx);
        let updates = group.clone();
        ctx.on_update_with(move |config: &Value| {
            let group = updates.clone();
            let config = entries(config);
            Some(
                async move {
                    group.update(config).await;
                    Ok(())
                }
                .boxed(),
            )
        })?;
        let stopping = group.clone();
        ctx.defer("group", Disposer::new(move || stopping.stop()))?;
        group.update(entries(&config)).await;
        Ok(())
    })
    .name("group")
    .tag(GROUP_TAG)
}

fn entries(config: &Value) -> Vec<Value> {
    config.as_array().cloned().unwrap_or_default()
}

impl Loader {
    /// The entry group of a group plugin running in `ctx`, sharing its entry's tree.
    pub(crate) fn new_subgroup(&self, ctx: &Context) -> crate::GroupHandle {
        let owner = self.entry_of_context(ctx);
        let id = self.next_id();
        self.with(|st| {
            let tree = owner
                .and_then(|owner| st.entries.get(&owner))
                .map(|data| st.groups[&data.parent].tree)
                .unwrap_or(self.0.root_tree);
            st.groups.insert(id, GroupData { ctx: ctx.clone(), tree, data: Vec::new(), owner });
            if let Some(data) = owner.and_then(|owner| st.entries.get_mut(&owner)) {
                data.subgroup = Some(id);
            }
        });
        self.group_handle(id)
    }

    /// Creates or adopts the entry for `options` in `group`.
    pub(crate) async fn group_create(&self, group: GroupId, mut options: Value) -> Result<String> {
        let issues = validate_options(&options, false);
        if !issues.is_empty() {
            return Err(self.reject(group, &options, issues));
        }
        let (entry, legacy) = self.with(|st| {
            let tree = st.groups[&group].tree;
            let id = st.ensure_id(tree, &mut options);
            let entry = match st.trees[&tree].store.get(&id) {
                Some(entry) => *entry,
                None => {
                    let entry = self.next_id();
                    st.new_entry(entry, &self.0.ctx, group);
                    st.trees.get_mut(&tree).expect("exists").store.insert(id, entry);
                    entry
                }
            };
            // the entry may be moved from another group
            let data = st.entries.get_mut(&entry).expect("registered");
            data.parent = group;
            let legacy = std::mem::replace(&mut data.options, options.clone());
            st.set_entry_options(entry, options);
            (entry, legacy)
        });
        self.entry_update(entry, legacy, true).await?;
        Ok(self.with(|st| st.composite_id(entry)))
    }

    /// Rejects invalid options; an entry already registered under the id
    /// is removed first, so the disposal is not taken for a self-dispose.
    fn reject(&self, group: GroupId, options: &Value, issues: Vec<cordis_core::Issue>) -> Error {
        let error = EntryValidationError { issues };
        let Some(id) = options.get("id").and_then(Value::as_str) else { return error.into() };
        let entry = self.with(|st| {
            let tree = st.groups[&group].tree;
            st.trees.get_mut(&tree).expect("exists").store.remove(id)
        });
        if let Some(entry) = entry {
            drop(self.dispose_entry(entry));
            let legacy = self.with(|st| st.entries.get(&entry).map(|data| data.options.clone()));
            self.partial_dispose(entry, &legacy.unwrap_or_default(), false);
        }
        error.into()
    }

    /// Removes the entry `id` of `group`; `is_dispose` keeps it in the data.
    pub(crate) fn group_remove(&self, group: GroupId, id: &str, is_dispose: bool) {
        let entry = self.with(|st| {
            let tree = st.groups[&group].tree;
            let entry = *st.trees[&tree].store.get(id)?;
            // an entry that another group already adopted belongs to that group now
            if st.entries[&entry].parent != group {
                return None;
            }
            // unregistered before disposing: the self-dispose detection checks the store
            st.trees.get_mut(&tree).expect("exists").store.remove(id);
            Some(entry)
        });
        let Some(entry) = entry else { return };
        drop(self.dispose_entry(entry));
        let legacy = self.with(|st| {
            if !is_dispose {
                let mut data = st.groups[&group].data.clone();
                data.retain(|options| options.get("id").and_then(Value::as_str) != Some(id));
                st.set_group_data(group, data);
            }
            st.entries.get(&entry).map(|data| data.options.clone()).unwrap_or_default()
        });
        self.partial_dispose(entry, &legacy, false);
    }

    /// Keyed reconciliation: entries are created / updated / removed
    /// concurrently; one bad entry does not stop the others.
    pub(crate) async fn group_update(&self, group: GroupId, mut config: Vec<Value>) {
        let keys = self.with(|st| {
            let Some(target) = st.groups.get(&group) else { return Vec::new() };
            let tree = target.tree;
            let old: Vec<String> = target.data.iter().map(|options| EntryOptions::read(options).id).collect();
            let mut keys: Vec<Key> = old.into_iter().filter(|id| !id.is_empty()).map(Key::Id).collect();
            for (index, options) in config.iter_mut().enumerate() {
                if options.is_object() && validate_options(options, false).is_empty() {
                    let id = st.ensure_id(tree, options);
                    if !keys.contains(&Key::Id(id.clone())) {
                        keys.push(Key::Id(id));
                    }
                } else if !options
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| keys.contains(&Key::Id(id.to_string())))
                {
                    // an invalid entry under a known id is reached (and rejected) by that id
                    keys.push(Key::Invalid(index));
                }
            }
            st.set_group_data(group, config.clone());
            keys
        });
        let tasks = keys.into_iter().map(|key| {
            let this = self.clone();
            let options = match &key {
                Key::Id(id) => {
                    config.iter().find(|options| options.get("id").and_then(Value::as_str) == Some(id)).cloned()
                }
                Key::Invalid(index) => config.get(*index).cloned(),
            };
            async move {
                match (options, key) {
                    (Some(options), _) => {
                        if let Err(error) = this.group_create(group, options.clone()).await {
                            this.0.ctx.logger().log_error(&this.locate_error(group, &options, &error));
                        }
                    }
                    (None, Key::Id(id)) => this.group_remove(group, &id, false),
                    (None, Key::Invalid(_)) => {}
                }
            }
        });
        join_all(tasks).await;
    }

    pub(crate) fn group_stop(&self, group: GroupId) {
        let ids: Vec<String> = self.with(|st| {
            st.groups.get(&group).map(|g| g.data.iter().map(|o| EntryOptions::read(o).id).collect()).unwrap_or_default()
        });
        for id in ids {
            self.group_remove(group, &id, true);
        }
    }
}

#[derive(PartialEq)]
enum Key {
    Id(String),
    Invalid(usize),
}
