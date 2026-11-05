//! Port of `packages/loader/tests/evaluate.spec.ts` and the expression
//! cases of `index.spec.ts`.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use common::{Counter, Gate, Mock, settle};
use kardia_core::{Disposer, Error, Fiber, FiberState, Inject, PluginDef};
use kardia_loader::OUTER_TAG;
use parking_lot::Mutex;
use serde::Serialize;
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

fn worker(extra: Value) -> Value {
    let mut options = json!({ "id": "worker", "name": "worker" });
    options.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    options
}

fn removed(fiber: Option<Fiber>) -> bool {
    fiber.is_none_or(|fiber| fiber.is_removed())
}

/// `settings` as a JSON view counting its reads (an expression evaluation
/// reads it once); `enabled` can be flipped without a reload.
#[derive(Clone, Default)]
struct Probe {
    reads: Arc<AtomicUsize>,
    enabled: Arc<AtomicBool>,
}

impl Serialize for Probe {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        json!({ "enabled": self.enabled.load(Ordering::SeqCst) }).serialize(serializer)
    }
}

impl Probe {
    fn register(fx: &Fixture, enabled: bool) -> Self {
        let probe = Self::default();
        probe.enabled.store(enabled, Ordering::SeqCst);
        let p = probe.clone();
        fx.mock.mock(
            "probe",
            PluginDef::new(move |ctx, _| {
                let probe = p.clone();
                async move {
                    ctx.provide_serialized_as("settings", probe)?;
                    Ok(())
                }
            }),
        );
        probe
    }

    fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}

fn probe_entry() -> Value {
    json!({ "id": "settings", "name": "probe" })
}

#[tokio::test]
async fn decides_disabled_before_the_plugin_dependencies() {
    let fx = fixture().await;
    fx.mock.mock("needy", PluginDef::new(|_, _| async { Ok(()) }).inject(["http"]));
    let needy = json!({ "id": "needy", "name": "needy", "inject": ["settings"], "disabled": { "__jsExpr": "!settings.enabled" } });
    fx.mock.read(json!([settings(json!({ "enabled": false })), needy])).await;
    let entry = fx.mock.loader.resolve("needy").unwrap();
    assert_eq!(entry.outer_fiber().unwrap().state(), FiberState::Active);
    assert!(entry.inner_fiber().is_none());
    fx.mock.loader.update("settings", json!({ "config": { "enabled": true } }), None, None).await.unwrap();
    fx.mock.loader.settled().await;
    assert_eq!(entry.inner_fiber().unwrap().state(), FiberState::Pending);
}

#[tokio::test]
async fn keeps_an_inject_value_on_the_outer_fiber_when_the_plugin_declares_the_service_itself() {
    let fx = fixture().await;
    let own = Counter::default();
    fx.mock.mock("own", own.def().inject(Inject::new().with_meta("settings", json!({ "own": 1 }))));
    let entry = json!({ "id": "own", "name": "own", "inject": { "settings": { "namespace": "ns" } } });
    fx.mock.read(json!([settings(json!({})), entry])).await;
    let entry = fx.mock.loader.resolve("own").unwrap();
    assert_eq!(own.take().0, 1);
    assert_eq!(entry.outer_fiber().unwrap().inject().meta("settings"), Some(&json!({ "namespace": "ns" })));
    // the plugin keeps its own declaration; the entry configures it through `intercept`
    assert_eq!(entry.inner_fiber().unwrap().inject().meta("settings"), Some(&json!({ "own": 1 })));
}

#[tokio::test]
async fn rebuilds_the_entry_when_inject_changes() {
    let fx = fixture().await;
    let entries =
        json!([settings(json!({})), { "id": "env", "name": "env" }, worker(json!({ "inject": ["settings"] }))]);
    fx.mock.read(entries).await;
    let entry = fx.mock.loader.resolve("worker").unwrap();
    let outer = entry.outer_fiber().unwrap();
    fx.mock.loader.update("worker", json!({ "inject": ["settings", "env"] }), None, None).await.unwrap();
    fx.mock.loader.settled().await;
    assert!(outer.is_removed());
    let keys: Vec<String> = entry.outer_fiber().unwrap().inject().keys().map(ToString::to_string).collect();
    assert_eq!(keys, vec!["settings", "env"]);
    fx.mock.loader.update("worker", json!({ "inject": null }), None, None).await.unwrap();
    fx.mock.loader.settled().await;
    assert!(entry.outer_fiber().is_none());
    assert_ne!(entry.inner_fiber().unwrap().parent().unwrap().name(), OUTER_TAG);
    assert_eq!(calls(&fx.worker), 3);
}

#[tokio::test]
async fn disposes_the_outer_fiber_when_the_plugin_disposes_itself() {
    let fx = fixture().await;
    fx.mock.read(json!([settings(json!({})), worker(json!({ "inject": ["settings"] }))])).await;
    let entry = fx.mock.loader.resolve("worker").unwrap();
    let outer = entry.outer_fiber().unwrap();
    entry.inner_fiber().unwrap().dispose().await.unwrap();
    fx.mock.loader.settled().await;
    assert_eq!(entry.options()["disabled"], json!(true));
    assert!(outer.is_removed());
    assert!(entry.outer_fiber().is_none());
}

#[tokio::test]
async fn disposes_both_fibers_when_the_entry_is_removed() {
    let fx = fixture().await;
    fx.mock.read(json!([settings(json!({})), worker(json!({ "inject": ["settings"] }))])).await;
    let entry = fx.mock.loader.resolve("worker").unwrap();
    let (outer, inner) = (entry.outer_fiber().unwrap(), entry.inner_fiber().unwrap());
    fx.mock.loader.remove("worker").unwrap();
    fx.mock.loader.settled().await;
    assert!(outer.is_removed() && inner.is_removed());
}

#[tokio::test]
async fn keeps_the_entry_isolate_without_an_inject() {
    let fx = fixture().await;
    let child = worker(json!({ "isolate": { "settings": "other" }, "disabled": { "__jsExpr": "!settings.enabled" } }));
    let group = json!({ "id": "group", "name": "group", "group": true, "inject": ["settings"], "config": [child] });
    fx.mock.read(json!([settings(json!({ "enabled": true })), group])).await;
    assert!(fx.mock.loader.resolve("worker").unwrap().is_disabled());
    assert_eq!(calls(&fx.worker), 0);
}

#[tokio::test]
async fn rebuilds_without_a_write_back_when_inject_is_added() {
    let fx = fixture().await;
    fx.mock.read(json!([settings(json!({})), worker(json!({}))])).await;
    fx.mock.take_changes();
    fx.mock.loader.update("worker", json!({ "inject": ["settings"] }), None, None).await.unwrap();
    fx.mock.loader.settled().await;
    let entry = fx.mock.loader.resolve("worker").unwrap();
    assert!(entry.options().get("disabled").is_none());
    assert!(fx.mock.take_changes().iter().all(|c| c.options.as_ref().is_none_or(|o| o.get("disabled").is_none())));
    assert_eq!(entry.inner_fiber().unwrap().state(), FiberState::Active);
    assert_eq!(calls(&fx.worker), 2);
}

#[tokio::test]
async fn drops_an_activation_whose_dependency_is_lost_during_the_import() {
    let fx = fixture().await;
    fx.mock.read(json!([settings(json!({}))])).await;
    // the second resolution is the outer fiber's
    let (entered, release) = fx.mock.hold_after("worker", 1, false);
    let loader = fx.mock.loader.clone();
    let creating =
        tokio::spawn(async move { loader.create(worker(json!({ "inject": ["settings"] })), None, None).await });
    entered.wait().await;
    fx.mock.loader.remove("settings").unwrap();
    release.open();
    creating.await.unwrap().unwrap();
    fx.mock.loader.settled().await;
    assert_eq!(calls(&fx.worker), 0);
    assert_eq!(fx.mock.loader.resolve("worker").unwrap().outer_fiber().unwrap().state(), FiberState::Pending);
}

/// `Entry.refresh` has no counterpart; an update without changes re-evaluates `inject` as well.
#[tokio::test]
async fn re_evaluates_a_failing_inject_value_on_refresh() {
    let fx = fixture().await;
    fx.mock.read(json!([worker(json!({ "inject": { "missing": { "__jsExpr": "doesNotExist.value" } } }))])).await;
    fx.mock.loader.update("worker", json!({}), None, None).await.unwrap();
    fx.mock.loader.settled().await;
    let entry = fx.mock.loader.resolve("worker").unwrap();
    assert!(entry.inner_fiber().is_none());
    assert!(entry.is_disabled());
    assert_eq!(calls(&fx.worker), 0);
}

#[tokio::test]
async fn caches_the_verdict_independently_of_reads_and_service_mutations() {
    for group in [false, true] {
        let fx = fixture().await;
        let probe = Probe::register(&fx, true);
        fx.mock.read(json!([probe_entry()])).await;
        let mut owner = json!({
            "id": "owner", "name": if group { "group" } else { "worker" }, "group": group,
            "inject": ["settings"], "disabled": { "__jsExpr": "!settings.enabled" },
        });
        if group {
            owner["config"] = json!([{ "id": "child", "name": "worker" }, { "id": "sibling", "name": "worker" }]);
        }
        fx.mock.loader.create(owner, None, None).await.unwrap();
        fx.mock.loader.settled().await;
        let owner = fx.mock.loader.resolve("owner").unwrap();
        let entry = fx.mock.loader.resolve(if group { "child" } else { "owner" }).unwrap();
        assert_eq!(probe.reads(), 1, "group: {group}");

        probe.enabled.store(false, Ordering::SeqCst);
        for _ in 0..3 {
            assert!(!entry.is_disabled());
        }
        assert_eq!(probe.reads(), 1);
        assert_eq!(entry.inner_fiber().unwrap().state(), FiberState::Active);

        // an explicit update re-evaluates; the group itself still runs
        let config = owner.options().get("config").cloned().unwrap_or(Value::Null);
        fx.mock.loader.update("owner", json!({ "config": config }), None, None).await.unwrap();
        fx.mock.loader.settled().await;
        assert!(entry.is_disabled());
        assert!(removed(entry.inner_fiber()));
        if group {
            assert_eq!(owner.inner_fiber().unwrap().state(), FiberState::Active);
        }
    }
}

#[tokio::test]
async fn does_not_repeat_a_failing_disabled_expression_when_its_verdict_is_read() {
    let fx = fixture().await;
    let probe = Probe::register(&fx, true);
    let entry = worker(json!({ "inject": ["settings"], "disabled": { "__jsExpr": "settings.missing.value" } }));
    fx.mock.read(json!([probe_entry(), entry])).await;
    for _ in 0..3 {
        assert!(fx.mock.loader.resolve("worker").unwrap().is_disabled());
    }
    assert_eq!(probe.reads(), 1);
}

#[tokio::test]
async fn evaluates_a_changed_disabled_expression_once_in_either_direction() {
    let fx = fixture().await;
    let probe = Probe::register(&fx, true);
    let disabled = |expr: &str| json!({ "disabled": { "__jsExpr": expr } });
    let entry = worker(json!({ "inject": ["settings"], "disabled": { "__jsExpr": "!settings.enabled" } }));
    fx.mock.read(json!([probe_entry(), entry])).await;
    let entry = fx.mock.loader.resolve("worker").unwrap();
    assert_eq!(probe.reads(), 1);

    // a failing expression disables the plugin and is reported once
    fx.mock.loader.update("worker", disabled("settings.missing.value"), None, None).await.unwrap();
    fx.mock.loader.settled().await;
    assert_eq!(probe.reads(), 2);
    assert!(removed(entry.inner_fiber()));

    fx.mock.loader.update("worker", disabled("!settings.enabled"), None, None).await.unwrap();
    fx.mock.loader.settled().await;
    assert_eq!(probe.reads(), 3);
    assert_eq!(entry.inner_fiber().unwrap().state(), FiberState::Active);
    assert_eq!(calls(&fx.worker), 2);
}

#[tokio::test]
async fn invalidates_the_verdict_before_an_asynchronous_dependency_unload() {
    let fx = fixture().await;
    let (entered, release) = (Gate::new(), Gate::new());
    let during_unload = Arc::new(Mutex::new(None));
    let (loader, e, r, d) = (fx.mock.loader.clone(), entered.clone(), release.clone(), during_unload.clone());
    fx.mock.mock(
        "held",
        PluginDef::new(move |ctx, _| {
            let (loader, entered, release, during_unload) = (loader.clone(), e.clone(), r.clone(), d.clone());
            async move {
                ctx.defer(
                    "held",
                    Disposer::future(move || {
                        *during_unload.lock() = Some(loader.resolve("held").unwrap().is_disabled());
                        entered.open();
                        async move { release.wait().await }
                    }),
                )?;
                Ok(())
            }
        }),
    );
    let held = json!({ "id": "held", "name": "held", "inject": ["settings"], "disabled": { "__jsExpr": "!settings.enabled" } });
    fx.mock.read(json!([settings(json!({ "enabled": true })), held])).await;
    let entry = fx.mock.loader.resolve("held").unwrap();
    let outer = entry.outer_fiber();
    fx.mock.loader.remove("settings").unwrap();
    entered.wait().await;
    assert!(entry.is_disabled());
    assert_eq!(*during_unload.lock(), Some(true));
    assert_eq!(entry.outer_fiber(), outer);
    assert_eq!(entry.options()["disabled"], json!({ "__jsExpr": "!settings.enabled" }));
    release.open();
    fx.mock.loader.settled().await;

    // an update while pending keeps the outer fiber
    fx.mock.loader.update("held", json!({ "config": { "value": 1 } }), None, None).await.unwrap();
    assert_eq!(entry.outer_fiber(), outer);
    fx.mock.loader.create(settings(json!({ "enabled": true })), None, None).await.unwrap();
    fx.mock.loader.settled().await;
    assert!(!entry.is_disabled());
    assert_eq!(entry.inner_fiber().unwrap().state(), FiberState::Active);
}

#[tokio::test]
async fn keeps_a_disabled_group_verdict_undecided_when_its_dependency_disappears() {
    let fx = fixture().await;
    let owner = json!({
        "id": "owner", "name": "group", "group": true, "inject": ["settings"],
        "disabled": { "__jsExpr": "!settings.enabled" }, "config": [{ "id": "child", "name": "worker" }],
    });
    fx.mock.read(json!([settings(json!({ "enabled": false })), owner])).await;
    let owner = fx.mock.loader.resolve("owner").unwrap();
    fx.mock.loader.remove("settings").unwrap();
    fx.mock.loader.settled().await;
    assert!(!owner.is_disabled());
    assert_eq!(owner.options()["disabled"], json!({ "__jsExpr": "!settings.enabled" }));
    fx.mock.loader.create(settings(json!({ "enabled": true })), None, None).await.unwrap();
    fx.mock.loader.settled().await;
    assert!(!fx.mock.loader.resolve("child").unwrap().is_disabled());
    assert_eq!(calls(&fx.worker), 1);
}

#[tokio::test]
async fn re_evaluates_disabled_and_inject_in_the_new_parent_context_after_a_move() {
    for field in ["disabled", "inject"] {
        let fx = fixture().await;
        let child = match field {
            "disabled" => worker(json!({ "disabled": { "__jsExpr": "!env.enabled" } })),
            _ => worker(json!({
                "inject": { "settings": { "namespace": { "__jsExpr": "env.namespace" } } },
                "disabled": { "__jsExpr": "false" },
            })),
        };
        let mut entries = vec![settings(json!({}))];
        for id in ["a", "b"] {
            let config = if id == "a" { json!([child]) } else { json!([]) };
            entries.push(json!({ "id": id, "name": "group", "group": true, "inject": ["env"], "isolate": { "env": id }, "config": config }));
        }
        for id in ["a", "b"] {
            let config = json!({ "enabled": id == "a", "namespace": id });
            entries
                .push(json!({ "id": format!("env-{id}"), "name": "env", "isolate": { "env": id }, "config": config }));
        }
        fx.mock.read(Value::Array(entries)).await;
        let entry = fx.mock.loader.resolve("worker").unwrap();
        assert_eq!(entry.inner_fiber().unwrap().state(), FiberState::Active, "{field}");
        fx.mock.loader.update("worker", json!({}), Some(Some("b")), None).await.unwrap();
        fx.mock.loader.settled().await;
        if field == "disabled" {
            assert!(entry.is_disabled());
            assert!(removed(entry.inner_fiber()));
        } else {
            assert_eq!(entry.outer_fiber().unwrap().inject().meta("settings"), Some(&json!({ "namespace": "b" })));
            assert_eq!(entry.inner_fiber().unwrap().state(), FiberState::Active);
        }
    }
}

#[tokio::test]
async fn re_evaluates_the_verdict_after_changing_the_entry_isolation() {
    let fx = fixture().await;
    let providers = ["a", "b"].map(
        |id| json!({ "id": id, "name": "settings", "isolate": { "settings": id }, "config": { "enabled": id == "a" } }),
    );
    fx.mock.read(json!(providers)).await;
    let options = worker(json!({
        "inject": ["settings"], "isolate": { "settings": "a" }, "disabled": { "__jsExpr": "!settings.enabled" },
    }));
    fx.mock.loader.create(options, None, None).await.unwrap();
    fx.mock.loader.settled().await;
    let entry = fx.mock.loader.resolve("worker").unwrap();
    assert!(!entry.is_disabled());
    fx.mock.loader.update("worker", json!({ "isolate": { "settings": "b" } }), None, None).await.unwrap();
    fx.mock.loader.settled().await;
    assert!(entry.is_disabled());
    assert!(removed(entry.inner_fiber()));
    assert_eq!(entry.options()["disabled"], json!({ "__jsExpr": "!settings.enabled" }));
}

/// The `patch` window has no counterpart: patching a context that has no
/// fibers yet does not suspend.
#[tokio::test]
async fn drops_entries_removed_or_disabled_during_import() {
    let fx = fixture().await;
    fx.mock.read(json!([settings(json!({}))])).await;
    for inject in [None, Some(json!(["settings"]))] {
        for action in ["remove", "disable"] {
            let label = format!("inject: {inject:?}, action: {action}");
            let options = || match &inject {
                Some(inject) => worker(json!({ "inject": inject })),
                None => worker(json!({})),
            };
            let (entered, release) = fx.mock.hold("worker", false);
            let loader = fx.mock.loader.clone();
            let creating = tokio::spawn({
                let options = options();
                async move { loader.create(options, None, None).await }
            });
            entered.wait().await;
            let entry = fx.mock.loader.resolve("worker").unwrap();
            match action {
                "remove" => fx.mock.loader.remove("worker").unwrap(),
                _ => fx.mock.loader.update("worker", json!({ "disabled": true }), None, None).await.unwrap(),
            }
            release.open();
            creating.await.unwrap().unwrap();
            fx.mock.loader.settled().await;
            assert!(entry.outer_fiber().is_none() && entry.inner_fiber().is_none(), "{label}");
            assert_eq!(calls(&fx.worker), 0, "{label}");

            // a dropped init does not block a later create or re-enable
            match action {
                "remove" => drop(fx.mock.loader.create(options(), None, None).await.unwrap()),
                _ => fx.mock.loader.update("worker", json!({ "disabled": false }), None, None).await.unwrap(),
            }
            fx.mock.loader.settled().await;
            assert_eq!(fx.mock.fiber("worker").state(), FiberState::Active, "{label}");
            assert_eq!(calls(&fx.worker), 1, "{label}");
            fx.mock.loader.remove("worker").unwrap();
            fx.mock.loader.settled().await;
            fx.worker.calls.store(0, Ordering::SeqCst);
        }
    }
}

#[tokio::test]
async fn drops_entries_removed_or_disabled_during_patch() {
    let fx = fixture().await;
    fx.mock.read(json!([settings(json!({}))])).await;
    for inject in [None, Some(json!(["settings"]))] {
        for action in ["remove", "disable"] {
            let label = format!("inject: {inject:?}, action: {action}");
            let options = || match &inject {
                Some(inject) => worker(json!({ "inject": inject })),
                None => worker(json!({})),
            };
            let (entered, release, hook) = fx.mock.hold_patch(false, |options| options["id"] == "worker");
            let loader = fx.mock.loader.clone();
            let creating = tokio::spawn({
                let options = options();
                async move { loader.create(options, None, None).await }
            });
            entered.wait().await;
            let entry = fx.mock.loader.resolve("worker").unwrap();
            match action {
                "remove" => fx.mock.loader.remove("worker").unwrap(),
                _ => fx.mock.loader.update("worker", json!({ "disabled": true }), None, None).await.unwrap(),
            }
            release.open();
            creating.await.unwrap().unwrap();
            hook.dispose().await.unwrap();
            fx.mock.loader.settled().await;
            assert!(entry.outer_fiber().is_none() && entry.inner_fiber().is_none(), "{label}");
            assert_eq!(calls(&fx.worker), 0, "{label}");

            // a dropped init does not block a later create or re-enable
            match action {
                "remove" => drop(fx.mock.loader.create(options(), None, None).await.unwrap()),
                _ => fx.mock.loader.update("worker", json!({ "disabled": false }), None, None).await.unwrap(),
            }
            fx.mock.loader.settled().await;
            assert_eq!(fx.mock.fiber("worker").state(), FiberState::Active, "{label}");
            assert_eq!(calls(&fx.worker), 1, "{label}");
            fx.mock.loader.remove("worker").unwrap();
            fx.mock.loader.settled().await;
            fx.worker.calls.store(0, Ordering::SeqCst);
        }
    }
}

// --- entry evaluation boundaries ---

#[tokio::test]
async fn does_not_start_the_plugin_when_intercept_cannot_be_evaluated() {
    let fx = fixture().await;
    let intercept = json!({ "http": { "proxy": { "__jsExpr": "settings.missing.value" } } });
    fx.mock.read(json!([settings(json!({})), worker(json!({ "inject": ["settings"], "intercept": intercept }))])).await;
    assert!(fx.mock.loader.resolve("worker").unwrap().is_disabled());
    assert_eq!(calls(&fx.worker), 0);
}

#[tokio::test]
async fn stops_a_group_whose_intercept_stops_evaluating_on_an_update() {
    let fx = fixture().await;
    let proxy = |expr: &str| json!({ "http": { "proxy": { "__jsExpr": expr } } });
    let group = json!({
        "id": "group", "name": "group", "group": true, "inject": ["settings"],
        "intercept": proxy("settings.proxy"), "config": [{ "id": "worker", "name": "worker" }],
    });
    fx.mock.read(json!([settings(json!({ "proxy": "p1" })), group])).await;
    let group = fx.mock.loader.resolve("group").unwrap();
    // an expression on a group reaches the plugins of its children
    assert_eq!(*fx.seen.intercept.lock(), Some(json!({ "proxy": "p1" })));
    fx.mock.loader.update("group", json!({ "intercept": proxy("settings.missing.value") }), None, None).await.unwrap();
    fx.mock.loader.settled().await;
    assert!(group.inner_fiber().unwrap().is_removed());
    assert!(fx.mock.loader.resolve("worker").is_err());
}

#[tokio::test]
async fn does_not_evaluate_intercept_while_disabled() {
    let fx = fixture().await;
    let probe = Probe::register(&fx, true);
    let entry = worker(json!({
        "inject": ["settings"], "disabled": { "__jsExpr": "true" },
        "intercept": { "http": { "enabled": { "__jsExpr": "settings.enabled" } } },
    }));
    fx.mock.read(json!([probe_entry(), entry])).await;
    assert_eq!(probe.reads(), 0);
    assert_eq!(calls(&fx.worker), 0);
}

#[tokio::test]
async fn cannot_read_a_service_outside_the_parent_context_and_the_entry_inject_in_disabled() {
    let fx = fixture().await;
    let entry = worker(json!({ "inject": ["env"], "disabled": { "__jsExpr": "!settings.enabled" } }));
    fx.mock.read(json!([settings(json!({ "enabled": true })), { "id": "env", "name": "env" }, entry])).await;
    let entry = fx.mock.loader.resolve("worker").unwrap();
    assert_eq!(entry.outer_fiber().unwrap().state(), FiberState::Active);
    assert!(entry.is_disabled());
    assert_eq!(calls(&fx.worker), 0);
}
