//! Managed realms (`config/isolate.ts`, the paper's Algorithm 7).
//!
//! `isolate: { svc: true }` gives the entry a private realm for `svc`;
//! `isolate: { svc: "label" }` a realm shared by every entry using the label.
//! When an entry's effective realms change (new isolation, or a move to
//! another group), a binding provided from inside the entry migrates to
//! the new realm without restarting its provider, if that realm is free.

use std::collections::{HashMap, HashSet};

use kardia_core::{Context, Key, RealmId};
use serde_json::Value;

use crate::loader::{EntryId, Loader, LoaderState};
use crate::options::{EntryOptions, IsolateLabel};

pub(crate) struct RealmDiff {
    key: Key,
    old: Option<RealmId>,
    new: Option<RealmId>,
    /// The provider of the binding in the old realm, if it is inside the entry.
    inside: Option<Context>,
}

impl Loader {
    /// Computes the entry's new isolation table and the realm changes it
    /// (and a move to another group) brings.
    pub(crate) fn isolate_prepare(&self, entry: EntryId) -> (HashMap<Key, RealmId>, Vec<RealmDiff>) {
        self.with(|st| {
            let Some(data) = st.entries.get(&entry) else { return Default::default() };
            let (isolate, ctx, parent) = (data.read().isolate, data.ctx.clone(), st.groups[&data.parent].ctx.clone());
            let mut table = HashMap::new();
            for (name, label) in isolate {
                let realm = st.realm(entry, &name, &label);
                table.insert(Key::new(&name), realm);
            }
            let mut names: Vec<Key> = table.keys().cloned().collect();
            names.extend(ctx.isolated_keys());
            names.extend(parent.isolated_keys());
            let mut seen = HashSet::new();
            names.retain(|name| seen.insert(name.clone()));
            let diffs = names
                .into_iter()
                .filter_map(|key| {
                    let old = ctx.realm(&key);
                    let new = table.get(&key).copied().or_else(|| parent.realm(&key));
                    if old == new {
                        return None;
                    }
                    let inside = old
                        .and_then(|realm| ctx.binding_provider(&key, realm))
                        .filter(|provider| provider.is_within(&ctx));
                    Some(RealmDiff { key, old, new, inside })
                })
                .collect();
            (table, diffs)
        })
    }

    /// After the entry has been re-attached: migrate its own bindings and
    /// re-resolve the consumers of both realms.
    pub(crate) fn isolate_finish(&self, diffs: Vec<RealmDiff>) {
        let ctx = &self.0.ctx;
        for diff in diffs {
            // a nested entry may still isolate the key for the provider
            if let (Some(old), Some(new), Some(provider)) = (diff.old, diff.new, &diff.inside)
                && provider.realm(diff.key.as_str()) == Some(new)
            {
                ctx.migrate_binding(&diff.key, old, new);
            }
            for realm in [diff.old, diff.new].into_iter().flatten() {
                ctx.refresh_consumers(&diff.key, realm);
            }
        }
    }
}

impl LoaderState {
    fn realm(&mut self, entry: EntryId, name: &str, label: &IsolateLabel) -> RealmId {
        match label {
            IsolateLabel::Private => *self
                .entries
                .get_mut(&entry)
                .expect("exists")
                .local_realms
                .entry(name.into())
                .or_insert_with(RealmId::new),
            IsolateLabel::Shared(label) => {
                *self.global_realms.entry(label.clone()).or_default().entry(name.into()).or_insert_with(RealmId::new)
            }
        }
    }

    /// Garbage-collects the shared realms `legacy` used and no entry uses any more.
    pub(crate) fn collect_realms(&mut self, entry: EntryId, legacy: &Value, active: bool) {
        let current = self.entries.get(&entry).map(|data| data.read().isolate).unwrap_or_default();
        for (name, label) in EntryOptions::read(legacy).isolate {
            let IsolateLabel::Shared(label) = label else { continue };
            if active && current.iter().any(|(n, l)| *n == name && *l == IsolateLabel::Shared(label.clone())) {
                continue;
            }
            if !self.global_realms.contains_key(&label) {
                continue;
            }
            let referenced = self.entries.iter().any(|(id, data)| {
                self.registered(*id)
                    && data.read().isolate.iter().any(|(n, l)| *n == name && *l == IsolateLabel::Shared(label.clone()))
            });
            if referenced {
                continue;
            }
            let realms = self.global_realms.get_mut(&label).expect("checked above");
            realms.remove(name.as_str());
            if realms.is_empty() {
                self.global_realms.remove(&label);
            }
        }
    }
}
