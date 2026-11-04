use std::collections::HashSet;
use std::sync::Arc;

use cordis_core::{Error, PluginDef, Result};
use cordis_loader::Resolver;
use futures::FutureExt;
use futures::future::BoxFuture;
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::Value;

/// A module identity: an absolute path (`/app/plugins/foo.rs`).
pub type ModuleId = String;

/// What a module exports.
#[derive(Clone)]
pub enum Exports {
    None,
    Plugin(PluginDef),
    Value(Value),
}

/// The dependencies' exports, available to a module while it evaluates.
pub struct Imports<'a> {
    graph: &'a ModuleGraph,
    deps: &'a [ModuleId],
}

impl Imports<'_> {
    pub fn get(&self, id: &str) -> Option<Exports> {
        self.deps.iter().any(|dep| dep == id).then(|| self.graph.exports(id)).flatten()
    }

    pub fn value(&self, id: &str) -> Option<Value> {
        match self.get(id)? {
            Exports::Value(value) => Some(value),
            _ => None,
        }
    }
}

type Evaluate = Arc<dyn Fn(&Imports) -> Result<Exports> + Send + Sync>;

/// The code of a module: its static dependencies and how it evaluates.
#[derive(Clone)]
pub struct ModuleCode {
    pub deps: Vec<ModuleId>,
    pub evaluate: Evaluate,
}

impl ModuleCode {
    pub fn new(deps: Vec<ModuleId>, evaluate: impl Fn(&Imports) -> Result<Exports> + Send + Sync + 'static) -> Self {
        Self { deps, evaluate: Arc::new(evaluate) }
    }
}

/// Where module code comes from (an in-memory table, WASM components,
/// dynamic libraries, ...). Loading the same id again yields its current code.
pub trait ModuleSource: Send + Sync + 'static {
    fn load(&self, id: &str) -> Result<ModuleCode>;
}

struct Record {
    deps: Vec<ModuleId>,
    exports: Exports,
}

/// The host-owned module registry (what Node's module caches are to the
/// reference): the loaded modules, their edges and exports. A module that
/// fails to load is not kept, so its next import tries again.
#[derive(Clone)]
pub struct ModuleGraph(Arc<Inner>);

struct Inner {
    source: Arc<dyn ModuleSource>,
    records: Mutex<IndexMap<ModuleId, Record>>,
    hosts: Mutex<HashSet<ModuleId>>,
}

/// A saved cache entry, to put back after an aborted reload.
pub(crate) struct Backup(Option<Record>);

impl ModuleGraph {
    pub fn new(source: impl ModuleSource) -> Self {
        Self(Arc::new(Inner { source: Arc::new(source), records: Default::default(), hosts: Default::default() }))
    }

    /// Imports a module and its dependencies (cached).
    pub fn import(&self, id: &str) -> Result<Exports> {
        self.import_with(id, &mut Vec::new())
    }

    fn import_with(&self, id: &str, loading: &mut Vec<ModuleId>) -> Result<Exports> {
        if let Some(record) = self.0.records.lock().get(id) {
            return Ok(record.exports.clone());
        }
        // a cycle sees the module as not evaluated yet
        if loading.iter().any(|other| other == id) {
            return Ok(Exports::None);
        }
        loading.push(id.to_string());
        let result = (|| {
            let code = self.0.source.load(id)?;
            for dep in &code.deps {
                self.import_with(dep, loading)?;
            }
            let exports = (code.evaluate)(&Imports { graph: self, deps: &code.deps })?;
            self.0.records.lock().insert(id.to_string(), Record { deps: code.deps, exports: exports.clone() });
            Ok(exports)
        })();
        loading.pop();
        result
    }

    /// Host modules (the program itself) can only change by restarting it.
    pub fn mark_host(&self, ids: impl IntoIterator<Item = impl Into<ModuleId>>) {
        self.0.hosts.lock().extend(ids.into_iter().map(Into::into));
    }

    pub fn is_host(&self, id: &str) -> bool {
        self.0.hosts.lock().contains(id)
    }

    pub(crate) fn hosts(&self) -> HashSet<ModuleId> {
        self.0.hosts.lock().clone()
    }

    pub fn is_loaded(&self, id: &str) -> bool {
        self.0.records.lock().contains_key(id)
    }

    pub fn loaded(&self) -> Vec<ModuleId> {
        self.0.records.lock().keys().cloned().collect()
    }

    pub fn edges(&self, id: &str) -> Vec<ModuleId> {
        self.0.records.lock().get(id).map(|record| record.deps.clone()).unwrap_or_default()
    }

    pub fn exports(&self, id: &str) -> Option<Exports> {
        self.0.records.lock().get(id).map(|record| record.exports.clone())
    }

    pub(crate) fn evict(&self, id: &str) -> Backup {
        Backup(self.0.records.lock().shift_remove(id))
    }

    pub(crate) fn restore(&self, id: &str, backup: &mut Backup) {
        let mut records = self.0.records.lock();
        records.shift_remove(id);
        if let Some(record) = backup.0.take() {
            records.insert(id.to_string(), record);
        }
    }
}

impl Resolver for ModuleGraph {
    fn resolve(&self, name: &str) -> BoxFuture<'static, Result<PluginDef>> {
        let result = match self.import(name) {
            Ok(Exports::Plugin(def)) => Ok(def),
            Ok(_) => Err(Error::msg(format!("module {name} does not export a plugin"))),
            Err(error) => Err(error),
        };
        futures::future::ready(result).boxed()
    }
}
