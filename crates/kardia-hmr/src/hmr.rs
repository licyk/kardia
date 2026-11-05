//! Hot reconciliation of changed modules (`packages/hmr/src/index.ts`).
//!
//! This follows the reference code rather than the paper's Algorithm 10:
//! changed modules and their importers are evicted, every registered plugin
//! whose closure reaches them is imported again (stage 1, the only fallible
//! step), plugins sharing an evicted module form units that commit or abort
//! together, committed plugins are replaced in place through
//! `registry.replace` (stage 2, so fibers keep their identity, parent,
//! config and loader entry), and their fibers settle concurrently (stage 3).

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use kardia_core::{Context, Error, Fiber, FiberId, PluginDef, Result, RuntimeKey, Service, Svc};
use kardia_loader::Loader;
use futures::future::join_all;
use parking_lot::Mutex;
use serde::Deserialize;

use crate::graph::{Backup, Exports, ModuleGraph, ModuleId};
use crate::watch::{self, WatchConfig};

/// The hot-reload service (`"hmr"`).
pub struct Hmr {
    graph: ModuleGraph,
    base: PathBuf,
    ctx: Context,
    /// Transactions run one at a time.
    queue: tokio::sync::Mutex<()>,
    /// Fibers being settled by the running transaction.
    settling: Mutex<HashSet<FiberId>>,
}

impl Service for Hmr {
    const NAME: &'static str = "hmr";
}

tokio::task_local! {
    static TRANSACTION: ();
}

/// The running state is brought in line with the changes, as far as they apply.
#[derive(Debug, Default)]
pub struct ReconcileResult {
    pub settled_fibers: Vec<Fiber>,
    pub import_failures: Vec<ImportFailure>,
    /// Changed host modules, which only a restart applies; the other changes are applied.
    pub skipped_host_modules: Vec<ModuleId>,
}

/// A module of a unit failed to import, or does not export a plugin. Nothing
/// in the unit is applied and its module cache entries are restored; the
/// other units of the transaction are not affected.
#[derive(Debug)]
pub struct ImportFailure {
    /// The evicted modules of the unit, whose cache entries are restored.
    pub modules: Vec<ModuleId>,
    pub error: Error,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HmrConfig {
    /// The base directory, relative to the directory of the config file that
    /// loads hmr (else the working directory), and defaulting to it.
    /// `watch.files` and logged paths are relative to it.
    pub base: Option<PathBuf>,
    pub watch: WatchConfig,
}

impl Hmr {
    /// The hmr plugin, reloading modules of `graph` (configured by [`HmrConfig`]).
    pub fn plugin(graph: ModuleGraph) -> PluginDef {
        PluginDef::typed(move |ctx, config: Arc<HmrConfig>| {
            let graph = graph.clone();
            async move {
                let base = config.base.as_deref().unwrap_or(Path::new("."));
                let base = normalize(&std::path::absolute(entry_dir(&ctx).join(base))?);
                let hmr = Hmr {
                    graph,
                    base: base.clone(),
                    ctx: ctx.clone(),
                    queue: Default::default(),
                    settling: Default::default(),
                };
                ctx.provide(hmr)?;
                if config.watch.enabled && !config.watch.files.is_empty() {
                    let watch = watch::start(&ctx, base, config.watch.clone())?;
                    // set up before this settles if `fs` is there; a failure is logged
                    let _ = watch.settled().await;
                }
                Ok(())
            }
        })
        .name("hmr")
        .provides([Hmr::NAME])
    }

    pub fn graph(&self) -> &ModuleGraph {
        &self.graph
    }

    pub fn base_dir(&self) -> &Path {
        &self.base
    }
}

pub trait HmrExt {
    /// Runs one transaction: `roots` reports the changed files or directories
    /// (absolute paths); loaded modules at or below them are reloaded.
    fn reconcile<F, Fut>(&self, roots: F) -> impl Future<Output = Result<ReconcileResult>> + Send
    where
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<Vec<PathBuf>>> + Send;
}

impl HmrExt for Svc<Hmr> {
    async fn reconcile<F, Fut>(&self, roots: F) -> Result<ReconcileResult>
    where
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<Vec<PathBuf>>> + Send,
    {
        // from the callback (same task) or from a plugin the transaction reloads
        let nested = TRANSACTION.try_with(|_| ()).is_ok() || self.reloading(self.caller());
        if nested {
            return Err(Error::msg("cannot start a transaction inside another transaction"));
        }
        let _turn = self.queue.lock().await;
        TRANSACTION.scope((), transaction(self, roots)).await
    }
}

impl Hmr {
    /// Whether `ctx` belongs to a fiber the running transaction is settling.
    fn reloading(&self, ctx: &Context) -> bool {
        let settling = self.settling.lock();
        if settling.is_empty() {
            return false;
        }
        let mut fiber = Some(ctx.fiber());
        while let Some(current) = fiber {
            if settling.contains(&current.id()) {
                return true;
            }
            if current.is_root() {
                break;
            }
            fiber = current.parent();
        }
        false
    }
}

/// The directory that relative paths of the loader entry running `ctx`
/// resolve against (the config file's, for an included tree).
fn entry_dir(ctx: &Context) -> PathBuf {
    let entry = ctx.live::<Loader>().and_then(|loader| loader.entry_of(ctx));
    entry.map(|entry| entry.parent().tree().base_dir()).unwrap_or_default()
}

/// A registered plugin whose modules have changed.
struct Stale {
    /// The module to take the replacement from (the outermost exporter).
    url: ModuleId,
    /// Every module that exports the plugin.
    urls: Vec<ModuleId>,
    plugin: PluginDef,
    /// Its closure in the old module graph.
    modules: HashSet<ModuleId>,
}

struct Attempt {
    stale: Stale,
    /// Modules first loaded by this attempt.
    loaded: Vec<ModuleId>,
    replacement: Result<PluginDef>,
}

async fn transaction<F, Fut>(hmr: &Hmr, callback: F) -> Result<ReconcileResult>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Vec<PathBuf>>>,
{
    let mut result = ReconcileResult::default();
    let mut roots = HashSet::new();
    for root in callback().await? {
        if !root.is_absolute() {
            return Err(Error::msg(format!("invalid root {}, expected an absolute path", root.display())));
        }
        roots.insert(normalize(&root));
    }
    if roots.is_empty() {
        return Ok(result);
    }
    let graph = &hmr.graph;
    let hosts = graph.hosts();
    let mut changed = HashSet::new();
    for id in matched(graph, &roots) {
        // a change to a host module cannot be applied in place
        if hosts.contains(&id) {
            result.skipped_host_modules.push(id);
        } else {
            changed.insert(id);
        }
    }
    if changed.is_empty() {
        return Ok(result);
    }

    // the old module graph, before anything is evicted
    let loaded = graph.loaded();
    let old_edges: HashMap<ModuleId, Vec<ModuleId>> = loaded.iter().map(|id| (id.clone(), graph.edges(id))).collect();
    let old = |id: &str| old_edges.get(id).cloned().unwrap_or_default();
    let (accepted, declined) = analyze(&loaded, &old_edges, &hosts, &changed);

    // a plugin may be exported by several modules, e.g. a module re-exporting it
    let registry = hmr.ctx.registry();
    let mut exporters: Vec<(RuntimeKey, Vec<(ModuleId, PluginDef)>)> = Vec::new();
    for id in loaded.iter().filter(|id| !declined.contains(*id)) {
        let Some(Exports::Plugin(def)) = graph.exports(id) else { continue };
        let Some(runtime) = registry.get(&def) else { continue };
        match exporters.iter_mut().find(|(key, _)| *key == runtime) {
            Some((_, exported)) => exported.push((id.clone(), def)),
            None => exporters.push((runtime, vec![(id.clone(), def)])),
        }
    }

    // The closure of a plugin: its modules and the accepted modules they reach
    // through accepted modules (including another plugin's, so that a plugin
    // importing another one shares its unit).
    let mut stale_plugins = Vec::new();
    let mut evicted: HashSet<ModuleId> = accepted.clone();
    for (_, exported) in exporters {
        let own: HashSet<&ModuleId> = exported.iter().map(|(id, _)| id).collect();
        let closures: Vec<HashSet<ModuleId>> = exported
            .iter()
            .map(|(id, _)| closure([id.clone()], &old, |url| declined.contains(url) && !own.contains(&url.to_string())))
            .collect();
        let modules: HashSet<ModuleId> = closures.iter().flatten().cloned().collect();
        if !modules.iter().any(|id| accepted.contains(id)) {
            continue;
        }
        evicted.extend(modules.iter().cloned());
        // a module that no other module of the plugin imports
        let outermost = (0..exported.len())
            .find(|&i| !(0..exported.len()).any(|j| j != i && closures[j].contains(&exported[i].0)))
            .unwrap_or(0);
        stale_plugins.push(Stale {
            url: exported[outermost].0.clone(),
            urls: exported.iter().map(|(id, _)| id.clone()).collect(),
            plugin: exported[outermost].1.clone(),
            modules,
        });
    }

    let mut backups: HashMap<ModuleId, Backup> = evicted.iter().map(|id| (id.clone(), graph.evict(id))).collect();

    // Stage 1: import the new modules of every stale plugin. Validity is
    // checked here, where only the module cache has changed, so the later
    // stages need no rollback. Nothing is restored before every plugin has
    // been imported, so no new module links against an evicted old one.
    let retained: HashSet<ModuleId> = graph.loaded().into_iter().collect();
    let mut attempts = Vec::new();
    for stale in stale_plugins {
        let before: HashSet<ModuleId> = graph.loaded().into_iter().collect();
        let replacement = (|| {
            for url in stale.urls.iter().filter(|url| **url != stale.url) {
                graph.import(url)?;
            }
            match graph.import(&stale.url)? {
                Exports::Plugin(def) => Ok(def),
                _ => Err(Error::msg(format!("invalid plugin at {}, expect a plugin export", stale.url))),
            }
        })();
        if let Err(error) = &replacement {
            hmr.ctx.logger().log_error(error);
        }
        let loaded = graph.loaded().into_iter().filter(|id| !before.contains(id) && !evicted.contains(id)).collect();
        attempts.push(Attempt { stale, loaded, replacement });
    }

    // the new module graph, read before anything is restored
    let new = |id: &str| graph.edges(id);
    let units = partition(&attempts, &evicted, &new, &retained);
    let (aborted, committed): (Vec<&Unit>, Vec<&Unit>) =
        units.iter().partition(|unit| unit.attempts.iter().any(|&i| attempts[i].replacement.is_err()));

    // An evicted module outside every unit is restored with the aborted units
    // it reaches in the old graph, unless it also reaches a committed unit
    // (then neither version is consistent, and it stays evicted).
    let mut restored: HashSet<ModuleId> = aborted.iter().flat_map(|unit| unit.modules.iter().cloned()).collect();
    let mut failure_modules: Vec<HashSet<ModuleId>> = aborted.iter().map(|unit| unit.modules.clone()).collect();
    let owned: HashSet<&ModuleId> = units.iter().flat_map(|unit| unit.modules.iter()).collect();
    for id in evicted.iter().filter(|id| !owned.contains(id)) {
        let reached = closure([id.clone()], &old, |url| !evicted.contains(url));
        let overlaps = |unit: &Unit| unit.modules.iter().any(|m| reached.contains(m));
        if committed.iter().any(|unit| overlaps(unit)) {
            continue;
        }
        for (index, unit) in aborted.iter().enumerate() {
            if overlaps(unit) {
                restored.insert(id.clone());
                failure_modules[index].insert(id.clone());
            }
        }
    }

    // A module first loaded in stage 1 that reaches a restored module in the
    // new graph was linked against the new version, so it goes too.
    for id in attempts.iter().flat_map(|attempt| attempt.loaded.iter()) {
        let reached = closure([id.clone()], &new, |url| retained.contains(url));
        if reached.iter().any(|url| restored.contains(url)) {
            drop(graph.evict(id));
        }
    }
    for id in &restored {
        if let Some(backup) = backups.get_mut(id) {
            graph.restore(id, backup);
        }
    }
    for (unit, modules) in aborted.iter().zip(failure_modules) {
        let failed = unit.attempts.iter().find_map(|&i| attempts[i].replacement.as_ref().err()).expect("aborted");
        let mut modules: Vec<ModuleId> = modules.into_iter().collect();
        modules.sort();
        result.import_failures.push(ImportFailure { modules, error: failed.clone() });
    }

    // Stage 2: replace the committed plugins in place.
    let applied: HashSet<usize> = committed.iter().flat_map(|unit| unit.attempts.iter().copied()).collect();
    let mut fibers = Vec::new();
    for (index, attempt) in attempts.iter().enumerate() {
        let Ok(replacement) = &attempt.replacement else { continue };
        if !applied.contains(&index) {
            continue;
        }
        let replaced = registry.replace(&attempt.stale.plugin, replacement);
        if !replaced.is_empty() {
            hmr.ctx.logger().info(format_args!("reload plugin at {}", attempt.stale.url));
            fibers.extend(replaced);
        }
    }

    // Stage 3: wait for the fibers to settle, concurrently. No rollback: a
    // plugin failing to apply stays failed, as on a cold start.
    hmr.settling.lock().extend(fibers.iter().map(Fiber::id));
    join_all(fibers.iter().map(Fiber::settled)).await;
    hmr.settling.lock().clear();
    // a fiber below another reloaded fiber may have been disposed by it
    result.settled_fibers = fibers.into_iter().filter(|fiber| !fiber.is_removed()).collect();
    Ok(result)
}

/// Loaded modules whose path is a root or lies below one (whole segments).
fn matched(graph: &ModuleGraph, roots: &HashSet<PathBuf>) -> Vec<ModuleId> {
    graph.loaded().into_iter().filter(|id| Path::new(id).ancestors().any(|path| roots.contains(path))).collect()
}

/// Accepted: the changed modules and every module that imports an accepted
/// one (edges from host modules excluded). Declined: hosts and the rest.
fn analyze(
    loaded: &[ModuleId],
    edges: &HashMap<ModuleId, Vec<ModuleId>>,
    hosts: &HashSet<ModuleId>,
    changed: &HashSet<ModuleId>,
) -> (HashSet<ModuleId>, HashSet<ModuleId>) {
    let mut importers: HashMap<&str, Vec<&ModuleId>> = HashMap::new();
    for (id, deps) in edges {
        if hosts.contains(id) {
            continue;
        }
        for dep in deps {
            importers.entry(dep.as_str()).or_default().push(id);
        }
    }
    let mut accepted = HashSet::new();
    let mut queue: Vec<ModuleId> = changed.iter().cloned().collect();
    while let Some(id) = queue.pop() {
        if !accepted.insert(id.clone()) {
            continue;
        }
        queue.extend(importers.get(id.as_str()).into_iter().flatten().map(|id| (*id).clone()));
    }
    let mut declined = hosts.clone();
    declined.extend(loaded.iter().filter(|id| !accepted.contains(*id)).cloned());
    (accepted, declined)
}

/// Everything reachable from `roots`, not entering nodes for which `stop` holds.
fn closure(
    roots: impl IntoIterator<Item = ModuleId>,
    edges: &dyn Fn(&str) -> Vec<ModuleId>,
    stop: impl Fn(&str) -> bool,
) -> HashSet<ModuleId> {
    let mut result = HashSet::new();
    let mut queue: Vec<ModuleId> = roots.into_iter().collect();
    while let Some(id) = queue.pop() {
        if result.contains(&id) || stop(&id) {
            continue;
        }
        queue.extend(edges(&id));
        result.insert(id);
    }
    result
}

struct Unit {
    attempts: Vec<usize>,
    modules: HashSet<ModuleId>,
}

/// Connected components of the attempts, two being connected if their
/// closures in the old or the new graph share an evicted module.
fn partition(
    attempts: &[Attempt],
    evicted: &HashSet<ModuleId>,
    new: &dyn Fn(&str) -> Vec<ModuleId>,
    retained: &HashSet<ModuleId>,
) -> Vec<Unit> {
    let reaches: Vec<HashSet<ModuleId>> = attempts
        .iter()
        .map(|attempt| {
            let mut reach = closure(attempt.stale.urls.iter().cloned(), new, |url| retained.contains(url));
            reach.extend(attempt.stale.modules.iter().cloned());
            reach.retain(|id| evicted.contains(id));
            reach
        })
        .collect();
    let mut parents: Vec<usize> = (0..attempts.len()).collect();
    fn find(parents: &mut [usize], mut index: usize) -> usize {
        while parents[index] != index {
            parents[index] = parents[parents[index]];
            index = parents[index];
        }
        index
    }
    let mut owners: HashMap<&ModuleId, usize> = HashMap::new();
    for (index, reach) in reaches.iter().enumerate() {
        for id in reach {
            match owners.get(id) {
                None => {
                    owners.insert(id, index);
                }
                Some(&owner) => {
                    let (a, b) = (find(&mut parents, index), find(&mut parents, owner));
                    parents[a] = b;
                }
            }
        }
    }
    let mut units: Vec<(usize, Unit)> = Vec::new();
    for (index, reach) in reaches.into_iter().enumerate() {
        let root = find(&mut parents, index);
        let position = match units.iter().position(|(r, _)| *r == root) {
            Some(position) => position,
            None => {
                units.push((root, Unit { attempts: Vec::new(), modules: HashSet::new() }));
                units.len() - 1
            }
        };
        units[position].1.attempts.push(index);
        units[position].1.modules.extend(reach);
    }
    units.into_iter().map(|(_, unit)| unit).collect()
}

pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut normal = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normal.pop();
            }
            component => normal.push(component),
        }
    }
    normal
}
