//! Port of `packages/hmr/tests/reconcile.spec.ts` against an in-memory module source.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use cordis_core::{Context, Error, Event, FiberState, Inject, PluginDef, Result, Svc};
use cordis_hmr::{Exports, Hmr, HmrExt, Imports, ModuleCode, ModuleGraph, ModuleSource};
use cordis_loader::Loader;
use parking_lot::Mutex;
use serde_json::{Value, json};

/// Asks the plugin `name` for its value.
struct Query(&'static str);

impl Event for Query {
    type Output = Option<Value>;
}

/// Module code by path; writing a module changes what its next load evaluates.
#[derive(Clone, Default)]
struct Files(Arc<Mutex<HashMap<String, ModuleCode>>>);

impl ModuleSource for Files {
    fn load(&self, id: &str) -> Result<ModuleCode> {
        self.0.lock().get(id).cloned().ok_or_else(|| Error::msg(format!("cannot find module {id}")))
    }
}

fn path(name: &str) -> String {
    format!("/app/{name}")
}

impl Files {
    fn write(&self, name: &str, code: ModuleCode) -> String {
        let id = path(name);
        self.0.lock().insert(id.clone(), code);
        id
    }
}

/// A plugin answering `Query(name)` with a value computed from its imports.
fn plugin_with(
    name: &'static str,
    deps: &[&str],
    value: impl Fn(&Imports) -> Value + Send + Sync + 'static,
    configure: impl Fn(PluginDef) -> PluginDef + Send + Sync + 'static,
) -> ModuleCode {
    let deps: Vec<String> = deps.iter().map(|dep| path(dep)).collect();
    ModuleCode::new(deps, move |imports| {
        let value = value(imports);
        let def = PluginDef::new(move |ctx, _| {
            let value = value.clone();
            async move {
                ctx.on(move |query: &Query| (query.0 == name).then(|| value.clone()))?;
                Ok(())
            }
        })
        .name(name);
        Ok(Exports::Plugin(configure(def)))
    })
}

fn plugin(name: &'static str, value: &'static str) -> ModuleCode {
    plugin_with(name, &[], move |_| json!(value), |def| def)
}

fn dependent(name: &'static str, dep: &'static str) -> ModuleCode {
    let id = path(dep);
    plugin_with(name, &[dep], move |imports| imports.value(&id).unwrap_or_default(), |def| def)
}

fn value(value: Value) -> ModuleCode {
    ModuleCode::new(vec![], move |_| Ok(Exports::Value(value.clone())))
}

fn throws(deps: &[&str], message: &'static str) -> ModuleCode {
    ModuleCode::new(deps.iter().map(|dep| path(dep)).collect(), move |_| Err(Error::msg(message)))
}

struct App {
    root: Context,
    files: Files,
    graph: ModuleGraph,
    loader: Loader,
    hmr: Svc<Hmr>,
}

impl App {
    async fn setup() -> Self {
        let root = Context::new();
        let files = Files::default();
        let graph = ModuleGraph::new(files.clone());
        root.plugin_def(&Loader::plugin(graph.clone()), Value::Null).unwrap().settled().await.unwrap();
        root.plugin_def(&Hmr::plugin(graph.clone()), Value::Null).unwrap().settled().await.unwrap();
        let loader = (*root.get::<Loader>().unwrap()).clone();
        let hmr = root.get::<Hmr>().unwrap();
        Self { root, files, graph, loader, hmr }
    }

    /// Creates an entry for the module `name` and waits for it.
    async fn load(&self, name: &str, code: ModuleCode) -> String {
        let id = self.files.write(name, code);
        let entry = self.loader.create(json!({ "name": id }), None, None).await.unwrap();
        self.loader.settled().await;
        let _ = entry;
        id
    }

    fn query(&self, name: &'static str) -> Option<Value> {
        self.root.bail(&Query(name)).flatten()
    }

    async fn reconcile(&self, roots: &[&str]) -> Result<cordis_hmr::ReconcileResult> {
        let roots: Vec<PathBuf> = roots.iter().map(PathBuf::from).collect();
        self.hmr.reconcile(move || async move { Ok(roots) }).await
    }
}

#[tokio::test]
async fn replaces_an_entry_plugin_in_place() {
    let app = App::setup().await;
    let disposed = Arc::new(Mutex::new(false));
    let d = disposed.clone();
    let id = app
        .load(
            "entry.rs",
            ModuleCode::new(vec![], move |_| {
                let d = d.clone();
                Ok(Exports::Plugin(PluginDef::new(move |ctx, _| {
                    let d = d.clone();
                    async move {
                        ctx.on(|query: &Query| (query.0 == "entry").then(|| json!("v1")))?;
                        ctx.on_dispose(move || *d.lock() = true)?;
                        Ok(())
                    }
                })))
            }),
        )
        .await;
    let entry = app.loader.entries()[0].clone();
    let inner = entry.inner_fiber().unwrap();
    assert_eq!(app.query("entry"), Some(json!("v1")));

    app.files.write("entry.rs", plugin("entry", "v2"));
    let result = app.reconcile(&[&id]).await.unwrap();
    assert_eq!(result.settled_fibers, vec![inner.clone()]);
    assert_eq!(inner.state(), FiberState::Active);
    assert!(result.import_failures.is_empty());
    assert!(*disposed.lock());
    // the entry keeps its fiber, which now runs the new plugin
    assert_eq!(entry.inner_fiber(), Some(inner));
    assert!(!entry.is_disabled());
    assert_eq!(app.query("entry"), Some(json!("v2")));
}

#[tokio::test]
async fn imports_a_module_again_after_it_failed_in_an_aborted_attempt() {
    let app = App::setup().await;
    let id = app.load("added-entry.rs", plugin("added", "v1")).await;
    app.files.write("added-dep.rs", throws(&[], "added v1"));
    app.files.write("added-entry.rs", plugin_with("added", &["added-dep.rs"], |_| json!("v2"), |def| def));
    assert_eq!(app.reconcile(&[&id]).await.unwrap().import_failures.len(), 1);

    // only the entry is reconciled again, as the caller knows no more
    app.files.write("added-dep.rs", value(json!(null)));
    let result = app.reconcile(&[&id]).await.unwrap();
    assert!(result.import_failures.is_empty());
    assert_eq!(app.query("added"), Some(json!("v2")));
}

#[tokio::test]
async fn replaces_a_service_and_its_consumers_follow() {
    let app = App::setup().await;
    let service = |version: &'static str| {
        ModuleCode::new(vec![], move |_| {
            Ok(Exports::Plugin(PluginDef::new(move |ctx, _| async move {
                ctx.provide_as("reconcileService", version)?;
                Ok(())
            })))
        })
    };
    let id = app.load("service.rs", service("v1")).await;
    assert_eq!(*app.root.get_as::<&str>("reconcileService").unwrap(), "v1");
    app.files.write("service.rs", service("v2"));
    app.reconcile(&[&id]).await.unwrap();
    assert_eq!(*app.root.get_as::<&str>("reconcileService").unwrap(), "v2");
}

#[tokio::test]
async fn replaces_a_plugin_when_one_of_its_dependencies_changes() {
    let app = App::setup().await;
    let dep = app.files.write("dep.rs", value(json!("v1")));
    app.files.write("common.rs", value(json!("common")));
    let dep_id = path("dep.rs");
    app.load(
        "dependent.rs",
        plugin_with("dependent", &["dep.rs", "common.rs"], move |i| i.value(&dep_id).unwrap(), |d| d),
    )
    .await;
    // shares an unchanged module with the dependent plugin, but not the changed one
    let applied = Arc::new(Mutex::new(0));
    let a = applied.clone();
    app.load(
        "sibling.rs",
        ModuleCode::new(vec![path("common.rs")], move |_| {
            let a = a.clone();
            Ok(Exports::Plugin(PluginDef::new(move |_, _| {
                *a.lock() += 1;
                async { Ok(()) }
            })))
        }),
    )
    .await;
    assert_eq!(*applied.lock(), 1);

    app.files.write("dep.rs", value(json!("v2")));
    let result = app.reconcile(&[&dep]).await.unwrap();
    assert_eq!(result.settled_fibers.len(), 1);
    assert_eq!(app.query("dependent"), Some(json!("v2")));
    assert_eq!(*applied.lock(), 1);
}

#[tokio::test]
async fn replaces_the_inner_fiber_of_an_entry_with_an_injection_scope_in_place() {
    let app = App::setup().await;
    app.root.provide_as("reconcileScope", ()).unwrap();
    let id = app.files.write("scoped.rs", plugin("scoped", "v1"));
    let entry = app.loader.create(json!({ "name": id, "inject": ["reconcileScope"] }), None, None).await.unwrap();
    app.loader.settled().await;
    let entry = app.loader.resolve(&entry).unwrap();
    let (outer, inner) = (entry.outer_fiber().unwrap(), entry.inner_fiber().unwrap());
    assert_eq!(app.query("scoped"), Some(json!("v1")));

    app.files.write("scoped.rs", plugin("scoped", "v2"));
    let result = app.reconcile(&[&id]).await.unwrap();
    assert_eq!(result.settled_fibers, vec![inner.clone()]);
    assert_eq!(entry.outer_fiber(), Some(outer));
    assert_eq!(entry.inner_fiber(), Some(inner));
    assert_eq!(app.query("scoped"), Some(json!("v2")));
}

#[tokio::test]
async fn replaces_the_plugins_below_a_directory_root_only() {
    let app = App::setup().await;
    app.files.write("root/dep.rs", value(json!("v1")));
    app.load("root-dependent.rs", dependent("root", "root/dep.rs")).await;
    // a directory whose name extends that of the root is not below it
    app.files.write("root-sibling/dep.rs", value(json!("v1")));
    app.load("root-sibling-dependent.rs", dependent("sibling", "root-sibling/dep.rs")).await;

    app.files.write("root/dep.rs", value(json!("v2")));
    app.files.write("root-sibling/dep.rs", value(json!("v2")));
    let result = app.reconcile(&["/app/root/"]).await.unwrap();
    assert_eq!(result.settled_fibers.len(), 1);
    assert_eq!(app.query("root"), Some(json!("v2")));
    assert_eq!(app.query("sibling"), Some(json!("v1")));
}

#[tokio::test]
async fn rejects_relative_roots_and_the_error_of_the_callback() {
    let app = App::setup().await;
    let id = app.load("callback.rs", plugin("callback", "v1")).await;
    app.files.write("callback.rs", plugin("callback", "v2"));
    assert!(app.reconcile(&["./callback.rs"]).await.is_err());
    let error = app.hmr.reconcile(|| async { Err(Error::msg("callback failed")) }).await.unwrap_err();
    assert_eq!(error.to_string(), "callback failed");
    assert_eq!(app.query("callback"), Some(json!("v1")));
    let _ = id;
}

#[tokio::test]
async fn runs_the_callback_inside_the_transaction_and_transactions_one_at_a_time() {
    let app = App::setup().await;
    let order = Arc::new(Mutex::new(Vec::new()));
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let (nested_tx, nested_rx) = tokio::sync::oneshot::channel();
    let hmr = app.hmr.clone();
    let o = order.clone();
    let first = tokio::spawn({
        let (hmr, inner) = (app.hmr.clone(), app.hmr.clone());
        async move {
            hmr.reconcile(move || async move {
                let nested = hmr_nested(&inner).await;
                let _ = nested_tx.send(nested);
                let _ = rx.await;
                o.lock().push("callback");
                Ok(vec![])
            })
            .await
        }
    });
    let nested = nested_rx.await.unwrap();
    assert_eq!(nested, "cannot start a transaction inside another transaction");
    let o = order.clone();
    let second = tokio::spawn(async move {
        hmr.reconcile(move || async move {
            o.lock().push("second");
            Ok(vec![])
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let _ = tx.send(());
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(*order.lock(), vec!["callback", "second"]);
}

async fn hmr_nested(hmr: &Svc<Hmr>) -> String {
    match hmr.reconcile(|| async { Ok(vec![]) }).await {
        Ok(_) => "resolved".into(),
        Err(error) => error.to_string(),
    }
}

#[tokio::test]
async fn replaces_a_plugin_that_no_entry_manages() {
    let app = App::setup().await;
    let id = app.files.write("direct.rs", plugin("direct", "v1"));
    let Exports::Plugin(def) = app.graph.import(&id).unwrap() else { panic!("a plugin") };
    let direct = app.root.plugin_def(&def, Value::Null).unwrap();
    direct.settled().await.unwrap();
    app.files.write("direct.rs", plugin("direct", "v2"));
    let result = app.reconcile(&[&id]).await.unwrap();
    // the fiber returned by `ctx.plugin()` stays valid
    assert_eq!(result.settled_fibers, vec![direct.clone()]);
    assert_eq!(direct.state(), FiberState::Active);
    assert_eq!(app.query("direct"), Some(json!("v2")));
}

#[tokio::test]
async fn refreshes_a_disabled_plugin_for_when_it_is_enabled_again() {
    let app = App::setup().await;
    let dep = app.files.write("disabled-dep.rs", value(json!("v1")));
    let id = app.files.write("disabled.rs", dependent("disabled", "disabled-dep.rs"));
    let entry = app.loader.create(json!({ "name": id }), None, None).await.unwrap();
    app.loader.settled().await;
    app.loader.update(&entry, json!({ "disabled": true }), None, None).await.unwrap();
    app.files.write("disabled-dep.rs", value(json!("v2")));
    app.reconcile(&[&dep]).await.unwrap();
    app.loader.update(&entry, json!({ "disabled": false }), None, None).await.unwrap();
    app.loader.settled().await;
    assert_eq!(app.query("disabled"), Some(json!("v2")));
}

#[tokio::test]
async fn keeps_the_metadata_of_an_entry_that_re_exports_the_changed_plugin() {
    let app = App::setup().await;
    app.root.provide_as("reconcileGate", ()).unwrap();
    let implementation = app.files.write("reexport-impl.rs", plugin("reexport", "v1"));
    let impl_id = implementation.clone();
    let entry = app.files.write(
        "reexport-entry.rs",
        ModuleCode::new(vec![impl_id.clone()], move |imports| {
            let Some(Exports::Plugin(def)) = imports.get(&impl_id) else { return Err(Error::msg("no plugin")) };
            Ok(Exports::Plugin(def.inject(Inject::new().with("reconcileGate"))))
        }),
    );
    let Exports::Plugin(original) = app.graph.import(&entry).unwrap() else { panic!("a plugin") };
    app.root.plugin_def(&original, json!({})).unwrap().settled().await.unwrap();

    app.files.write("reexport-impl.rs", plugin("reexport", "v2"));
    let result = app.reconcile(&[&implementation]).await.unwrap();
    assert_eq!(result.settled_fibers.len(), 1);
    let keys: Vec<_> = result.settled_fibers[0].inject().keys().map(ToString::to_string).collect();
    assert_eq!(keys, vec!["reconcileGate"]);
    assert_eq!(app.query("reexport"), Some(json!("v2")));
    // the entry module is refreshed as well
    let Exports::Plugin(refreshed) = app.graph.import(&entry).unwrap() else { panic!("a plugin") };
    assert_ne!(refreshed.key(), original.key());
    assert_eq!(app.root.registry().get(&refreshed), result.settled_fibers[0].runtime());
}

#[tokio::test]
async fn refreshes_a_module_that_no_plugin_depends_on_for_its_next_import() {
    let app = App::setup().await;
    let orphan = app.files.write("orphan.rs", value(json!(1)));
    app.graph.import(&orphan).unwrap();
    app.files.write("orphan.rs", value(json!(2)));
    let result = app.reconcile(&[&orphan, "/app/data.txt"]).await.unwrap();
    assert!(result.settled_fibers.is_empty());
    let Exports::Value(after) = app.graph.import(&orphan).unwrap() else { panic!("a value") };
    assert_eq!(after, json!(2));
}

#[tokio::test]
async fn does_not_apply_a_unit_whose_module_fails_or_exports_no_plugin() {
    let app = App::setup().await;
    let id = app.load("abort.rs", plugin("abort", "v1")).await;
    app.files.write("abort.rs", throws(&[], "BROKEN"));
    let result = app.reconcile(&[&id]).await.unwrap();
    assert!(result.settled_fibers.is_empty());
    assert_eq!(result.import_failures.len(), 1);
    assert!(result.import_failures[0].modules.contains(&id));
    assert_eq!(app.query("abort"), Some(json!("v1")));
    // the module cache is restored, so the next change is applied normally
    app.files.write("abort.rs", plugin("abort", "v2"));
    app.reconcile(&[&id]).await.unwrap();
    assert_eq!(app.query("abort"), Some(json!("v2")));

    app.files.write("abort.rs", value(json!("not a plugin")));
    let result = app.reconcile(&[&id]).await.unwrap();
    assert_eq!(result.import_failures.len(), 1);
    assert_eq!(app.query("abort"), Some(json!("v2")));
}

#[tokio::test]
async fn applies_a_unit_when_another_unit_fails_to_import() {
    let app = App::setup().await;
    app.files.write("unit-common.rs", value(json!("common")));
    let shared = |name: &'static str, version: &'static str| {
        plugin_with(name, &["unit-common.rs"], move |_| json!(version), |d| d)
    };
    let broken = app.load("unit-broken.rs", shared("broken", "v1")).await;
    let healthy = app.load("unit-healthy.rs", shared("healthy", "v1")).await;
    // the two plugins only share an unchanged module
    app.files.write("unit-broken.rs", throws(&["unit-common.rs"], "BROKEN"));
    app.files.write("unit-healthy.rs", shared("healthy", "v2"));
    let result = app.reconcile(&[&broken, &healthy]).await.unwrap();
    assert_eq!(result.settled_fibers.len(), 1);
    assert_eq!(result.import_failures.len(), 1);
    assert!(result.import_failures[0].modules.contains(&broken));
    assert!(!result.import_failures[0].modules.contains(&healthy));
    assert_eq!(app.query("broken"), Some(json!("v1")));
    assert_eq!(app.query("healthy"), Some(json!("v2")));
    // the failed unit is retried on its own
    app.files.write("unit-broken.rs", shared("broken", "v2"));
    assert!(app.reconcile(&[&broken]).await.unwrap().import_failures.is_empty());
    assert_eq!(app.query("broken"), Some(json!("v2")));
}

#[tokio::test]
async fn aborts_plugins_that_share_a_changed_module_together() {
    let app = App::setup().await;
    let dep = app.files.write("joint-dep.rs", value(json!("v1")));
    let left = app.load("joint-left.rs", dependent("left", "joint-dep.rs")).await;
    app.load("joint-right.rs", dependent("right", "joint-dep.rs")).await;
    app.files.write("joint-dep.rs", value(json!("v2")));
    app.files.write("joint-left.rs", throws(&["joint-dep.rs"], "left failed"));
    let result = app.reconcile(&[&dep, &left]).await.unwrap();
    assert!(result.settled_fibers.is_empty());
    assert_eq!(result.import_failures.len(), 1);
    assert!(result.import_failures[0].modules.contains(&dep));
    assert_eq!(app.query("left"), Some(json!("v1")));
    assert_eq!(app.query("right"), Some(json!("v1")));
}

#[tokio::test]
async fn aborts_a_plugin_that_newly_imports_a_changed_module_of_a_failed_unit() {
    let app = App::setup().await;
    let dep = app.files.write("adopt-dep.rs", value(json!("v1")));
    let owner = app.load("adopt-owner.rs", plugin_with("owner", &["adopt-dep.rs"], |_| json!("v1"), |d| d)).await;
    let adopter = app.load("adopt-adopter.rs", plugin("adopter", "v1")).await;
    // the adopter is unrelated to the owner before the change
    app.files.write("adopt-dep.rs", value(json!("v2")));
    app.files.write("adopt-owner.rs", throws(&["adopt-dep.rs"], "owner failed"));
    app.files.write("adopt-adopter.rs", dependent("adopter", "adopt-dep.rs"));
    let result = app.reconcile(&[&dep, &owner, &adopter]).await.unwrap();
    assert!(result.settled_fibers.is_empty());
    assert_eq!(result.import_failures.len(), 1);
    assert!(result.import_failures[0].modules.contains(&dep) && result.import_failures[0].modules.contains(&adopter));
    assert_eq!(app.query("owner"), Some(json!("v1")));
    assert_eq!(app.query("adopter"), Some(json!("v1")));
}

#[tokio::test]
async fn restores_a_changed_module_outside_every_plugin_with_the_failed_unit_it_imports() {
    let app = App::setup().await;
    let dep = app.files.write("outside-dep.rs", value(json!("v1")));
    let dep_id = dep.clone();
    let derived = move |extra: &'static str| {
        let dep_id = dep_id.clone();
        ModuleCode::new(vec![dep_id.clone()], move |imports| {
            let value = imports.value(&dep_id).unwrap_or_default();
            Ok(Exports::Value(json!(format!("{}{extra}", value.as_str().unwrap_or_default()))))
        })
    };
    let outside = app.files.write("outside.rs", derived(""));
    app.graph.import(&outside).unwrap();
    let owner = app.load("outside-owner.rs", plugin_with("owner", &["outside-dep.rs"], |_| json!("v1"), |d| d)).await;
    app.files.write("outside-dep.rs", value(json!("v2")));
    app.files.write("outside.rs", derived("!"));
    app.files.write("outside-owner.rs", throws(&["outside-dep.rs"], "owner failed"));
    let result = app.reconcile(&[&dep, &outside, &owner]).await.unwrap();
    assert_eq!(result.import_failures.len(), 1);
    assert!(result.import_failures[0].modules.contains(&outside));
    let Exports::Value(value) = app.graph.import(&outside).unwrap() else { panic!("a value") };
    assert_eq!(value, json!("v1"));
}

#[tokio::test]
async fn leaves_a_fiber_failed_when_the_new_plugin_fails_to_apply() {
    let app = App::setup().await;
    let id = app.load("failed.rs", plugin("failed", "v1")).await;
    app.files.write(
        "failed.rs",
        ModuleCode::new(vec![], |_| {
            Ok(Exports::Plugin(PluginDef::new(|_, _| async { Err(Error::msg("apply failed")) })))
        }),
    );
    let result = app.reconcile(&[&id]).await.unwrap();
    assert_eq!(result.settled_fibers.len(), 1);
    assert_eq!(result.settled_fibers[0].state(), FiberState::Failed);
    assert_eq!(result.settled_fibers[0].settled().await.unwrap_err().to_string(), "apply failed");
    // the next change retries it
    app.files.write("failed.rs", plugin("failed", "v3"));
    assert_eq!(app.reconcile(&[&id]).await.unwrap().settled_fibers.len(), 1);
    assert_eq!(app.query("failed"), Some(json!("v3")));
}

#[tokio::test]
async fn skips_a_changed_host_module_below_a_root_and_applies_the_other_changes() {
    let app = App::setup().await;
    let host = app.load("host/host.rs", plugin("host", "v1")).await;
    app.load("host/other.rs", plugin("other", "v1")).await;
    app.graph.mark_host([host.clone()]);
    app.files.write("host/host.rs", plugin("host", "v2"));
    app.files.write("host/other.rs", plugin("other", "v2"));
    let result = app.reconcile(&["/app/host"]).await.unwrap();
    assert_eq!(result.skipped_host_modules, vec![host]);
    assert_eq!(app.query("host"), Some(json!("v1")));
    assert_eq!(app.query("other"), Some(json!("v2")));
}

#[tokio::test]
async fn rejects_a_transaction_started_by_a_plugin_it_reloads() {
    let app = App::setup().await;
    let outcomes = Arc::new(Mutex::new(Vec::new()));
    let nested = |version: &'static str| {
        let outcomes = outcomes.clone();
        plugin_with(
            "nested",
            &[],
            move |_| json!(version),
            move |def| {
                let outcomes = outcomes.clone();
                PluginDef::new(move |ctx, _| {
                    let outcomes = outcomes.clone();
                    async move {
                        let hmr = ctx.get::<Hmr>()?;
                        let outcome = hmr_nested(&hmr).await;
                        outcomes.lock().push(outcome);
                        Ok(())
                    }
                })
                .name(def.plugin_name().unwrap_or_default().to_string())
                .inject(["hmr"])
            },
        )
    };
    let id = app.load("nested.rs", nested("v1")).await;
    assert_eq!(*outcomes.lock(), vec!["resolved"]);
    app.files.write("nested.rs", nested("v2"));
    let result = app.reconcile(&[&id]).await.unwrap();
    assert_eq!(result.settled_fibers.len(), 1);
    assert_eq!(outcomes.lock()[1], "cannot start a transaction inside another transaction");
    // once the transaction is over, the plugin may start one
    let hmr = result.settled_fibers[0].context().get::<Hmr>().unwrap();
    assert_eq!(hmr_nested(&hmr).await, "resolved");
}
