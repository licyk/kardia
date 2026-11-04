//! Port of `packages/loader/tests/evaluate.spec.ts` and the expression
//! cases of `index.spec.ts`.

mod common;

use std::sync::Arc;

use common::{Counter, Mock, settle};
use cordis_core::{Error, FiberState, PluginDef};
use parking_lot::Mutex;
use serde_json::{Value, json};

/// Provides its config under its own name.
fn provider(name: &'static str) -> PluginDef {
    PluginDef::new(move |ctx, config| async move {
        ctx.provide_as(name, config)?;
        Ok(())
    })
}

#[derive(Clone, Default)]
struct Seen {
    intercept: Arc<Mutex<Option<Value>>>,
    config: Arc<Mutex<Option<Value>>>,
}

struct Fixture {
    mock: Mock,
    worker: Counter,
    seen: Seen,
}

async fn fixture() -> Fixture {
    let mock = Mock::boot().await;
    mock.mock("settings", provider("settings"));
    mock.mock("env", provider("env"));
    let (worker, seen) = (Counter::default(), Seen::default());
    let (w, s) = (worker.clone(), seen.clone());
    mock.mock(
        "worker",
        PluginDef::new(move |ctx, config| {
            w.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            *s.intercept.lock() = ctx.intercepts("http").last().cloned();
            *s.config.lock() = Some(config);
            async { Ok(()) }
        }),
    );
    Fixture { mock, worker, seen }
}

fn settings(config: Value) -> Value {
    json!({ "id": "settings", "name": "settings", "config": config })
}

fn calls(counter: &Counter) -> usize {
    counter.calls.load(std::sync::atomic::Ordering::SeqCst)
}

#[tokio::test]
async fn evaluates_a_disabled_expression_instead_of_treating_the_node_as_truthy() {
    let fx = fixture().await;
    fx.mock.read(json!([{ "id": "a", "name": "worker", "disabled": { "__jsExpr": "false" } }, { "id": "b", "name": "worker", "disabled": { "__jsExpr": "true" } }])).await;
    assert!(!fx.mock.loader.resolve("a").unwrap().is_disabled());
    assert!(fx.mock.loader.resolve("b").unwrap().is_disabled());
    // no outer fiber without inject
    assert!(fx.mock.loader.resolve("a").unwrap().outer_fiber().is_none());
    assert_eq!(calls(&fx.worker), 1);
}

#[tokio::test]
async fn nests_the_inner_fiber_under_the_outer_fiber() {
    let fx = fixture().await;
    fx.mock.read(json!([settings(json!({})), { "id": "worker", "name": "worker", "inject": ["settings"] }])).await;
    let entry = fx.mock.loader.resolve("worker").unwrap();
    let (outer, inner) = (entry.outer_fiber().unwrap(), entry.inner_fiber().unwrap());
    assert_eq!(inner.parent(), Some(outer.clone()));
    assert_eq!(outer.inject().keys().map(ToString::to_string).collect::<Vec<_>>(), vec!["settings"]);
    assert!(inner.inject().is_empty());
}

#[tokio::test]
async fn evaluates_disabled_with_the_entry_inject() {
    let fx = fixture().await;
    let worker = json!({ "id": "worker", "name": "worker", "inject": ["settings"], "disabled": { "__jsExpr": "!settings.enabled" } });
    fx.mock.read(json!([settings(json!({ "enabled": false })), worker])).await;
    let entry = fx.mock.loader.resolve("worker").unwrap();
    assert!(entry.is_disabled());
    assert_eq!(entry.outer_fiber().unwrap().state(), FiberState::Active);
    assert!(entry.inner_fiber().is_none());
    fx.mock.loader.update("settings", json!({ "config": { "enabled": true } }), None, None).await.unwrap();
    fx.mock.loader.settled().await;
    assert!(!entry.is_disabled());
    assert_eq!(calls(&fx.worker), 1);
}

#[tokio::test]
async fn waits_for_the_entry_inject_while_it_is_missing() {
    let fx = fixture().await;
    let worker = json!({ "id": "worker", "name": "worker", "inject": ["settings"], "disabled": { "__jsExpr": "!settings.enabled" } });
    fx.mock.read(json!([worker])).await;
    let entry = fx.mock.loader.resolve("worker").unwrap();
    assert_eq!(entry.outer_fiber().unwrap().state(), FiberState::Pending);
    assert!(entry.is_disabled());
    fx.mock.loader.create(settings(json!({ "enabled": true })), None, None).await.unwrap();
    fx.mock.loader.settled().await;
    assert!(!entry.is_disabled());
    assert_eq!(calls(&fx.worker), 1);
}

#[tokio::test]
async fn treats_a_failing_expression_as_disabled() {
    let fx = fixture().await;
    let worker = json!({ "id": "worker", "name": "worker", "inject": ["settings"], "disabled": { "__jsExpr": "settings.missing.value" } });
    fx.mock.read(json!([settings(json!({})), worker])).await;
    assert!(fx.mock.loader.resolve("worker").unwrap().inner_fiber().is_none());
    assert_eq!(calls(&fx.worker), 0);
    assert!(fx.mock.root.log_buffer().iter().any(|m| m.text.contains("cannot evaluate `settings.missing.value`")));
}

#[tokio::test]
async fn evaluates_inject_values_in_the_parent_context() {
    let fx = fixture().await;
    let inject = json!({ "settings": { "namespace": { "__jsExpr": "env.namespace" } } });
    fx.mock
        .read(json!([
            { "id": "env", "name": "env", "config": { "namespace": "ns" } },
            settings(json!({})),
            { "id": "group", "name": "group", "group": true, "inject": ["env"], "config": [{ "id": "worker", "name": "worker", "inject": inject }] },
        ]))
        .await;
    let entry = fx.mock.loader.resolve("worker").unwrap();
    assert_eq!(entry.outer_fiber().unwrap().inject().meta("settings"), Some(&json!({ "namespace": "ns" })));
    assert_eq!(entry.options()["inject"], inject);
    assert_eq!(calls(&fx.worker), 1);
}

#[tokio::test]
async fn cannot_read_a_sibling_or_the_entry_inject_from_an_inject_value() {
    let fx = fixture().await;
    let worker = json!({ "id": "worker", "name": "worker", "inject": { "settings": true, "other": { "__jsExpr": "settings.namespace" } } });
    fx.mock.read(json!([settings(json!({ "namespace": "ns" })), worker])).await;
    let entry = fx.mock.loader.resolve("worker").unwrap();
    assert!(entry.outer_fiber().is_none() && entry.inner_fiber().is_none());

    let worker = json!({ "id": "worker", "name": "worker", "inject": { "settings": { "namespace": { "__jsExpr": "env.namespace" } } } });
    fx.mock
        .read(json!([{ "id": "env", "name": "env", "config": { "namespace": "ns" } }, settings(json!({})), worker]))
        .await;
    assert!(fx.mock.loader.resolve("worker").unwrap().outer_fiber().is_none());
    assert_eq!(calls(&fx.worker), 0);
}

#[tokio::test]
async fn evaluates_intercept_with_the_entry_inject_for_the_plugin_only_and_updates_it_in_place() {
    let fx = fixture().await;
    let worker = json!({ "id": "worker", "name": "worker", "inject": ["settings"], "intercept": { "http": { "proxy": { "__jsExpr": "settings.proxy" } } } });
    fx.mock.read(json!([settings(json!({ "proxy": "p1" })), worker])).await;
    let entry = fx.mock.loader.resolve("worker").unwrap();
    assert_eq!(*fx.seen.intercept.lock(), Some(json!({ "proxy": "p1" })));
    assert!(entry.outer_fiber().unwrap().context().intercepts("http").is_empty());
    assert_eq!(entry.options()["intercept"], json!({ "http": { "proxy": { "__jsExpr": "settings.proxy" } } }));

    let inner = entry.inner_fiber().unwrap();
    let patch = json!({ "intercept": { "http": { "proxy": { "__jsExpr": "settings.proxy + '!'" } } } });
    fx.mock.loader.update("worker", patch, None, None).await.unwrap();
    assert_eq!(inner.context().intercepts("http"), vec![json!({ "proxy": "p1!" })]);
    fx.mock.loader.update("worker", json!({ "intercept": null }), None, None).await.unwrap();
    assert!(inner.context().intercepts("http").is_empty());
    assert_eq!(calls(&fx.worker), 1);
}

#[tokio::test]
async fn keeps_the_options_when_a_dependency_is_lost_and_recovered() {
    let fx = fixture().await;
    let worker = json!({ "id": "worker", "name": "worker", "inject": ["settings"], "disabled": { "__jsExpr": "!settings.enabled" } });
    fx.mock.read(json!([settings(json!({ "enabled": true })), worker])).await;
    let entry = fx.mock.loader.resolve("worker").unwrap();
    let (outer, inner) = (entry.outer_fiber(), entry.inner_fiber().unwrap());
    fx.mock.loader.remove("settings").unwrap();
    fx.mock.loader.settled().await;
    assert!(inner.is_removed());
    assert_eq!(entry.outer_fiber(), outer);
    assert_eq!(entry.options()["disabled"], json!({ "__jsExpr": "!settings.enabled" }));
    fx.mock.loader.create(settings(json!({ "enabled": true })), None, None).await.unwrap();
    fx.mock.loader.settled().await;
    assert_eq!(entry.inner_fiber().unwrap().state(), FiberState::Active);
    assert_eq!(calls(&fx.worker), 2);
}

#[tokio::test]
async fn reloads_the_outer_fiber_when_the_verdict_changes() {
    let fx = fixture().await;
    let worker = json!({ "id": "worker", "name": "worker", "inject": ["settings"], "disabled": { "__jsExpr": "!settings.enabled" } });
    fx.mock.read(json!([settings(json!({ "enabled": true })), worker])).await;
    let entry = fx.mock.loader.resolve("worker").unwrap();
    let outer = entry.outer_fiber();
    let update = |expr: &str| fx.mock.loader.update("worker", json!({ "disabled": { "__jsExpr": expr } }), None, None);
    update("settings.enabled").await.unwrap();
    settle().await;
    assert!(entry.inner_fiber().unwrap().is_removed());
    assert_eq!(entry.outer_fiber(), outer);
    update("!settings.enabled").await.unwrap();
    update("settings.enabled && false").await.unwrap();
    fx.mock.loader.settled().await;
    assert!(!entry.inner_fiber().unwrap().is_removed());
    assert_eq!(calls(&fx.worker), 2);
}

#[tokio::test]
async fn keeps_a_group_structural_and_passes_its_verdict_to_the_children() {
    let fx = fixture().await;
    let group = json!({
        "id": "group", "name": "group", "group": true, "inject": ["settings"],
        "disabled": { "__jsExpr": "!settings.enabled" }, "config": [{ "id": "worker", "name": "worker" }],
    });
    fx.mock.read(json!([settings(json!({ "enabled": false })), group])).await;
    assert_eq!(fx.mock.loader.resolve("group").unwrap().inner_fiber().unwrap().state(), FiberState::Active);
    assert!(fx.mock.loader.resolve("worker").unwrap().inner_fiber().is_none());
    fx.mock.loader.update("group", json!({ "disabled": { "__jsExpr": "false" } }), None, None).await.unwrap();
    fx.mock.loader.settled().await;
    assert_eq!(fx.mock.fiber("worker").state(), FiberState::Active);
    assert_eq!(fx.mock.loader.resolve("group").unwrap().options()["disabled"], json!({ "__jsExpr": "false" }));
}

#[tokio::test]
async fn disposes_the_plugin_when_intercept_stops_evaluating() {
    let fx = fixture().await;
    let proxy = |expr: &str| json!({ "http": { "proxy": { "__jsExpr": expr } } });
    let worker =
        json!({ "id": "worker", "name": "worker", "inject": ["settings"], "intercept": proxy("settings.proxy") });
    fx.mock.read(json!([settings(json!({ "proxy": "p1" })), worker])).await;
    let entry = fx.mock.loader.resolve("worker").unwrap();
    fx.mock.loader.update("worker", json!({ "intercept": proxy("settings.missing.value") }), None, None).await.unwrap();
    fx.mock.loader.settled().await;
    assert!(entry.is_disabled());
    assert!(entry.inner_fiber().unwrap().is_removed());
    // a later update does not restart the plugin with the previous table
    fx.mock.loader.update("worker", json!({ "config": { "tag": "v2" } }), None, None).await.unwrap();
    fx.mock.loader.settled().await;
    assert!(entry.inner_fiber().unwrap().is_removed());
    assert_eq!(calls(&fx.worker), 1);
    fx.mock.loader.update("worker", json!({ "intercept": proxy("settings.proxy") }), None, None).await.unwrap();
    fx.mock.loader.settled().await;
    assert!(!entry.is_disabled());
    assert_eq!(entry.inner_fiber().unwrap().state(), FiberState::Active);
    assert_eq!(calls(&fx.worker), 2);
}

#[tokio::test]
async fn evaluates_config_with_the_plugin_inject_on_every_load() {
    let fx = fixture().await;
    let configs = Arc::new(Mutex::new(Vec::new()));
    let c = configs.clone();
    fx.mock.mock(
        "own",
        PluginDef::new(move |_, config| {
            c.lock().push(config);
            async { Ok(()) }
        })
        .inject(["settings"]),
    );
    let own = json!({ "id": "own", "name": "own", "config": { "value": { "__jsExpr": "settings.value" } } });
    fx.mock.read(json!([settings(json!({ "value": 1 })), own])).await;
    assert_eq!(*configs.lock(), vec![json!({ "value": 1 })]);
    // re-evaluated when the dependency reloads; the source keeps the expression
    fx.mock.loader.update("settings", json!({ "config": { "value": 2 } }), None, None).await.unwrap();
    fx.mock.loader.settled().await;
    assert_eq!(*configs.lock(), vec![json!({ "value": 1 }), json!({ "value": 2 })]);
    assert_eq!(fx.mock.fiber("own").config(), json!({ "value": { "__jsExpr": "settings.value" } }));
}

#[tokio::test]
async fn cannot_read_a_service_outside_every_inject_in_config() {
    let fx = fixture().await;
    let worker = json!({ "id": "worker", "name": "worker", "config": { "value": { "__jsExpr": "settings.value" } } });
    fx.mock.read(json!([settings(json!({ "value": 1 })), worker])).await;
    assert_eq!(fx.mock.fiber("worker").state(), FiberState::Failed);
    assert_eq!(calls(&fx.worker), 0);
    assert!(fx.seen.config.lock().is_none());
}

#[tokio::test]
async fn recovers_a_failed_entry_through_update() {
    let fx = fixture().await;
    let fail = Arc::new(Mutex::new(true));
    let configs = Arc::new(Mutex::new(Vec::new()));
    let (f, c) = (fail.clone(), configs.clone());
    fx.mock.mock(
        "consumer",
        PluginDef::new(move |_, config| {
            c.lock().push(config);
            let fail = *f.lock();
            async move { if fail { Err(Error::msg("boom")) } else { Ok(()) } }
        }),
    );
    let consumer =
        json!({ "id": "1", "name": "consumer", "inject": ["foo"], "config": { "value": { "__jsExpr": "foo.value" } } });
    fx.mock.mock(
        "provider",
        PluginDef::new(|ctx, _| async move {
            ctx.provide_as("foo", json!({ "value": 1 }))?;
            Ok(())
        }),
    );
    fx.mock.read(json!([consumer, { "id": "2", "name": "provider" }])).await;
    assert_eq!(fx.mock.fiber("1").state(), FiberState::Failed);
    *fail.lock() = false;
    let patch = json!({ "config": { "value": { "__jsExpr": "foo.value" }, "fixed": true } });
    let _ = fx.mock.loader.update("1", patch, None, None).await;
    fx.mock.loader.settled().await;
    assert_eq!(fx.mock.fiber("1").state(), FiberState::Active);
    assert_eq!(configs.lock().last(), Some(&json!({ "value": 1, "fixed": true })));
}
