//! Port of `packages/core/tests/fiber.spec.ts` and `update.spec.ts`.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::{Custom, Gate, Seq, advance, errors, settle, sleep, track};
use kardia_core::{Context, Disposer, Error, FiberState, Level, PluginDef, StatusChanged, ValidationError};
use serde_json::{Value, json};

fn value_of(config: &Value) -> i64 {
    config["value"].as_i64().unwrap_or_default()
}

fn waiting_consumer(root: &Context) -> kardia_core::Fiber {
    root.inject(["foo"], |ctx| async move {
        sleep(1000).await;
        ctx.defer("dispose", Disposer::future(|| sleep(1000)))?;
        Ok(())
    })
    .unwrap()
}

#[tokio::test(start_paused = true)]
async fn inertia_lock_1() {
    let root = Context::new();
    let provision = root.provide_as("foo", 1i32).unwrap();
    let fiber = waiting_consumer(&root);
    advance(400).await;
    assert_eq!(fiber.state(), FiberState::Loading);
    drop(provision.dispose());
    advance(400).await;
    assert_eq!(fiber.state(), FiberState::Loading);
    advance(400).await;
    assert_eq!(fiber.state(), FiberState::Unloading);
    root.provide_as("foo", 1i32).unwrap();
    advance(1000).await;
    assert_eq!(fiber.state(), FiberState::Loading);
    advance(1000).await;
    assert_eq!(fiber.state(), FiberState::Active);
}

#[tokio::test(start_paused = true)]
async fn inertia_lock_2() {
    let root = Context::new();
    let provision = root.provide_as("foo", 1i32).unwrap();
    let fiber = waiting_consumer(&root);
    advance(400).await;
    assert_eq!(fiber.state(), FiberState::Loading);
    drop(provision.dispose());
    advance(400).await;
    assert_eq!(fiber.state(), FiberState::Loading);
    root.provide_as("foo", 2i32).unwrap();
    advance(400).await;
    // a new binding from the same provider still supersedes the old load
    assert_eq!(fiber.state(), FiberState::Unloading);
    advance(1000).await;
    assert_eq!(fiber.state(), FiberState::Loading);
    advance(1000).await;
    assert_eq!(fiber.state(), FiberState::Active);
    assert_eq!(*fiber.context().get_as::<i32>("foo").unwrap(), 2);
}

#[tokio::test(start_paused = true)]
async fn inertia_lock_3() {
    let root = Context::new();
    let provider = root
        .plugin_def(
            &PluginDef::new(|ctx, _| async move {
                ctx.provide_as("foo", ())?;
                Ok(())
            }),
            Value::Null,
        )
        .unwrap();
    provider.settled().await.unwrap();
    let fiber = waiting_consumer(&root);
    advance(400).await;
    assert_eq!(fiber.state(), FiberState::Loading);
    advance(600).await;
    assert_eq!(fiber.state(), FiberState::Active);
    let (disposed, _) = tokio::join!(provider.dispose(), advance(1000));
    disposed.unwrap();
    assert_eq!(fiber.state(), FiberState::Pending);
}

#[tokio::test]
async fn plugin_error() {
    let root = Context::new();
    let errors = errors(&root);
    let callback = Arc::new(AtomicUsize::new(0));
    let counter = callback.clone();
    let def = PluginDef::new(move |ctx, config| {
        let counter = counter.clone();
        async move {
            ctx.on(move |_: &Custom| {
                counter.fetch_add(1, Ordering::SeqCst);
            })?;
            if config["foo"] != json!(true) {
                return Err(Error::msg("plugin error"));
            }
            Ok(())
        }
    });
    let fiber1 = root.plugin_def(&def, Value::Null).unwrap();
    let fiber2 = root.plugin_def(&def, json!({ "foo": true })).unwrap();
    settle().await;
    assert_eq!(fiber1.state(), FiberState::Failed);
    assert_eq!(fiber2.state(), FiberState::Active);
    assert_eq!(errors.get(), vec!["plugin error"]);
    root.emit(&Custom);
    assert_eq!(callback.load(Ordering::SeqCst), 1);
}

fn counting(calls: &Arc<AtomicUsize>, fail_first: usize) -> PluginDef {
    let calls = calls.clone();
    PluginDef::new(move |_, _| {
        let n = calls.fetch_add(1, Ordering::SeqCst);
        async move { if n < fail_first { Err(Error::msg("boom")) } else { Ok(()) } }
    })
}

#[tokio::test]
async fn failed_fiber_does_not_reenter_on_dependency_refresh() {
    let root = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let provision = root.provide_as("foo", 1i32).unwrap();
    let fiber = root.plugin_def(&counting(&calls, usize::MAX).inject(["foo"]), Value::Null).unwrap();
    settle().await;
    assert_eq!(fiber.state(), FiberState::Failed);
    provision.dispose().await.unwrap();
    root.provide_as("foo", 2i32).unwrap();
    settle().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(fiber.state(), FiberState::Failed);
}

#[tokio::test]
async fn restart_keeps_a_failed_fiber_failed() {
    let root = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let fiber = root.plugin_def(&counting(&calls, 1), Value::Null).unwrap();
    settle().await;
    assert_eq!(fiber.state(), FiberState::Failed);
    let _ = fiber.restart().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(fiber.state(), FiberState::Failed);
}

#[tokio::test]
async fn update_recovers_a_failed_fiber() {
    let root = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));
    root.provide_as("foo", 1i32).unwrap();
    let fiber = root.plugin_def(&counting(&calls, 1).inject(["foo"]), Value::Null).unwrap();
    settle().await;
    assert_eq!(fiber.state(), FiberState::Failed);
    fiber.update(Value::Null).unwrap().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(fiber.state(), FiberState::Active);
}

#[tokio::test]
async fn update_waits_for_asynchronous_recovery() {
    let root = Context::new();
    let gate = Gate::new();
    let g = gate.clone();
    let def = PluginDef::new(move |_, config| {
        let gate = g.clone();
        async move {
            if config["fixed"] != json!(true) {
                return Err(Error::msg("boom"));
            }
            gate.wait().await;
            Ok(())
        }
    });
    let fiber = root.plugin_def(&def, json!({ "fixed": false })).unwrap();
    assert_eq!(fiber.settled().await.unwrap_err().to_string(), "boom");
    let settled = Seq::new();
    let task = track(fiber.update(json!({ "fixed": true })).unwrap(), &settled);
    settle().await;
    assert_eq!(settled.len(), 0);
    gate.open();
    task.await.unwrap();
    assert_eq!(fiber.state(), FiberState::Active);
}

#[tokio::test]
async fn update_surfaces_a_failed_recovery_to_its_caller() {
    let root = Context::new();
    let def = PluginDef::new(|_, config| async move { Err(Error::msg(config["message"].as_str().unwrap_or(""))) });
    let fiber = root.plugin_def(&def, json!({ "message": "initial failure" })).unwrap();
    assert_eq!(fiber.settled().await.unwrap_err().to_string(), "initial failure");
    let error = fiber.update(json!({ "message": "retry failure" })).unwrap().await.unwrap_err();
    assert_eq!(error.to_string(), "retry failure");
    assert_eq!(fiber.state(), FiberState::Failed);
}

#[tokio::test]
async fn update_surfaces_a_failed_reload_to_its_caller() {
    let root = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let def = PluginDef::new(move |_, _| {
        let n = c.fetch_add(1, Ordering::SeqCst);
        async move { if n == 1 { Err(Error::msg("boom")) } else { Ok(()) } }
    });
    let fiber = root.plugin_def(&def, Value::Null).unwrap();
    fiber.settled().await.unwrap();
    assert_eq!(fiber.update(json!({})).unwrap().await.unwrap_err().to_string(), "boom");
    assert_eq!(fiber.state(), FiberState::Failed);
}

#[tokio::test]
async fn update_does_not_require_awaiting_its_result() {
    let root = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let def = PluginDef::new(move |_, _| {
        let n = c.fetch_add(1, Ordering::SeqCst);
        async move { if n == 1 { Err(Error::msg("boom")) } else { Ok(()) } }
    });
    let fiber = root.plugin_def(&def, Value::Null).unwrap();
    fiber.settled().await.unwrap();
    drop(fiber.update(json!({})).unwrap());
    settle().await;
    assert_eq!(fiber.state(), FiberState::Failed);
}

#[tokio::test]
async fn dispose_error() {
    let root = Context::new();
    let errors = errors(&root);
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let def = PluginDef::new(move |ctx, _| {
        let calls = c.clone();
        async move {
            ctx.defer(
                "dispose",
                Disposer::fallible(move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Err(Error::msg("test"))
                }),
            )?;
            Ok(())
        }
    });
    let fiber = root.plugin_def(&def, Value::Null).unwrap();
    fiber.settled().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    fiber.dispose().await.unwrap();
    settle().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(errors.get(), vec!["test"]);
}

fn record_transitions(root: &Context, fiber: &kardia_core::Fiber) -> Seq<(FiberState, FiberState)> {
    let transitions = Seq::new();
    let (seq, target) = (transitions.clone(), fiber.clone());
    root.on(move |event: &StatusChanged| {
        if event.fiber == target {
            seq.push((event.old, event.new));
        }
    })
    .unwrap();
    transitions
}

#[tokio::test]
async fn settles_a_pending_fiber_to_disposed_on_dispose() {
    let root = Context::new();
    let fiber = root.inject(["never-provided"], |_| async { Ok(()) }).unwrap();
    let transitions = record_transitions(&root, &fiber);
    assert_eq!(fiber.state(), FiberState::Pending);
    fiber.dispose().await.unwrap();
    assert!(fiber.is_removed());
    assert_eq!(fiber.state(), FiberState::Disposed);
    assert_eq!(transitions.get(), vec![(FiberState::Pending, FiberState::Disposed)]);
}

#[tokio::test]
async fn settles_a_failed_fiber_to_disposed_on_dispose() {
    let root = Context::new();
    let fiber = root.plugin_def(&PluginDef::new(|_, _| async { Err(Error::msg("boom")) }), Value::Null).unwrap();
    assert_eq!(fiber.settled().await.unwrap_err().to_string(), "boom");
    assert_eq!(fiber.state(), FiberState::Failed);
    let transitions = record_transitions(&root, &fiber);
    fiber.dispose().await.unwrap();
    assert_eq!(fiber.state(), FiberState::Disposed);
    assert_eq!(transitions.get(), vec![(FiberState::Failed, FiberState::Disposed)]);
}

#[tokio::test]
async fn keeps_the_active_dispose_sequence_stable() {
    let root = Context::new();
    let fiber = root.plugin_def(&PluginDef::new(|_, _| async { Ok(()) }), Value::Null).unwrap();
    fiber.settled().await.unwrap();
    let transitions = record_transitions(&root, &fiber);
    fiber.dispose().await.unwrap();
    assert_eq!(fiber.state(), FiberState::Disposed);
    assert_eq!(
        transitions.get(),
        vec![(FiberState::Active, FiberState::Unloading), (FiberState::Unloading, FiberState::Disposed)]
    );
}

fn recording(applied: &Seq<Value>) -> PluginDef {
    let applied = applied.clone();
    PluginDef::new(move |_, config| {
        applied.push(config);
        async { Ok(()) }
    })
}

#[tokio::test]
async fn update_config() {
    let root = Context::new();
    let applied = Seq::new();
    let fiber = root.plugin_def(&recording(&applied), json!({ "msg": "hello" })).unwrap();
    fiber.settled().await.unwrap();
    assert_eq!(applied.get(), vec![json!({ "msg": "hello" })]);
    fiber.update(json!({ "msg": "world" })).unwrap().await.unwrap();
    fiber.update(json!({ "msg": "!!!" })).unwrap().await.unwrap();
    assert_eq!(applied.get(), vec![json!({ "msg": "hello" }), json!({ "msg": "world" }), json!({ "msg": "!!!" })]);
}

#[tokio::test]
async fn restart() {
    let root = Context::new();
    let applied = Seq::new();
    let fiber = root.plugin_def(&recording(&applied), Value::Null).unwrap();
    fiber.settled().await.unwrap();
    fiber.restart().await.unwrap();
    assert_eq!(applied.len(), 2);
    assert_eq!(fiber.state(), FiberState::Active);
}

#[tokio::test]
async fn update_config_while_injected_service_reloads() {
    let root = Context::new();
    let applied = Seq::new();
    let provider = root
        .plugin_def(
            &PluginDef::new(|ctx, config| async move {
                ctx.provide_as("provider", value_of(&config))?;
                Ok(())
            }),
            json!({ "value": 1 }),
        )
        .unwrap();
    let a = applied.clone();
    let consumer_def = PluginDef::new(move |ctx, config| {
        let applied = a.clone();
        async move {
            let provider = ctx.get_as::<i64>("provider")?;
            applied.push((*provider, config["mode"].as_str().unwrap().to_string()));
            Ok(())
        }
    })
    .inject(["provider"]);
    let consumer = root.plugin_def(&consumer_def, json!({ "mode": "old" })).unwrap();
    provider.settled().await.unwrap();
    consumer.settled().await.unwrap();

    let p = provider.update(json!({ "value": 2 })).unwrap();
    let c = consumer.update(json!({ "mode": "new" })).unwrap();
    let (p, c) = tokio::join!(p, c);
    p.unwrap();
    c.unwrap();
    // the consumer reloads as soon as the provider is back
    consumer.settled().await.unwrap();
    assert_eq!(applied.get(), vec![(1, "old".to_string()), (2, "new".to_string())]);
    assert_eq!(consumer.config(), json!({ "mode": "new" }));
    assert_eq!(consumer.state(), FiberState::Active);
}

fn validated(applied: &Seq<Value>) -> PluginDef {
    recording(applied).validate_with(|value| {
        if value["ok"] == json!(true) {
            let mut value = value.clone();
            value["resolved"] = json!(true);
            Ok(value)
        } else {
            Err(ValidationError::message("not ok"))
        }
    })
}

#[tokio::test]
async fn validation_fails_on_first_load() {
    let root = Context::new();
    let applied = Seq::new();
    let fiber = root.plugin_def(&validated(&applied), json!({ "ok": false })).unwrap();
    settle().await;
    assert_eq!(applied.len(), 0);
    assert_eq!(fiber.state(), FiberState::Failed);
    assert_eq!(fiber.error().unwrap().to_string(), "invalid config:\n  - not ok");
}

#[tokio::test]
async fn validation_keeps_a_running_plugin_on_update() {
    let root = Context::new();
    let applied = Seq::new();
    let fiber = root.plugin_def(&validated(&applied), json!({ "ok": true })).unwrap();
    fiber.settled().await.unwrap();
    assert_eq!(applied.len(), 1);
    let error = fiber.update(json!({ "ok": false })).err().unwrap();
    assert!(error.to_string().contains("invalid config"));
    settle().await;
    assert_eq!(applied.len(), 1);
    assert_eq!(fiber.state(), FiberState::Active);
    assert_eq!(fiber.config(), json!({ "ok": true }));
}

#[tokio::test]
async fn validation_passes_the_resolved_config_to_update_hooks() {
    let root = Context::new();
    let (applied, received) = (Seq::new(), Seq::new());
    let (a, r) = (applied.clone(), received.clone());
    let def = PluginDef::new(move |ctx, config| {
        a.push(config);
        let received = r.clone();
        async move {
            ctx.on_update(move |config: &Value| {
                received.push(config.clone());
                false
            })?;
            Ok(())
        }
    })
    .validate_with(|value| {
        let mut value = value.clone();
        value["resolved"] = json!(true);
        Ok(value)
    });
    let fiber = root.plugin_def(&def, json!({ "ok": true })).unwrap();
    fiber.settled().await.unwrap();
    assert_eq!(applied.get(), vec![json!({ "ok": true, "resolved": true })]);
    fiber.update(json!({ "ok": true, "extra": 1 })).unwrap().await.unwrap();
    assert_eq!(received.get(), vec![json!({ "ok": true, "extra": 1, "resolved": true })]);
    assert_eq!(fiber.config(), json!({ "ok": true, "extra": 1 }));
    assert_eq!(applied.len(), 2);
}

#[tokio::test]
async fn update_hook_keeps_the_source_config() {
    let root = Context::new();
    let (applied, received) = (Seq::new(), Seq::new());
    let (a, r) = (applied.clone(), received.clone());
    let def = PluginDef::new(move |ctx, config| {
        a.push(config);
        let received = r.clone();
        async move {
            ctx.on_update(move |config: &Value| {
                received.push(config.clone());
                true
            })?;
            Ok(())
        }
    });
    let fiber = root.plugin_def(&def, json!({ "ok": true })).unwrap();
    fiber.settled().await.unwrap();
    fiber.update(json!({ "ok": true, "extra": 1 })).unwrap().await.unwrap();
    assert_eq!(received.get(), vec![json!({ "ok": true, "extra": 1 })]);
    assert_eq!(applied.len(), 1);
    assert_eq!(fiber.config(), json!({ "ok": true, "extra": 1 }));
    fiber.restart().await.unwrap();
    assert_eq!(applied.get(), vec![json!({ "ok": true }), json!({ "ok": true, "extra": 1 })]);
}

// --- update.spec.ts ---

async fn concurrent_updates(inject: bool) {
    let root = Context::new();
    root.provide_as("foo", 1i32).unwrap();
    let (started, release, latest_started, latest_release) = (Gate::new(), Gate::new(), Gate::new(), Gate::new());
    let (applied, disposed) = (Seq::new(), Seq::new());
    let gates = (started.clone(), release.clone(), latest_started.clone(), latest_release.clone());
    let (a, d) = (applied.clone(), disposed.clone());
    let mut def = PluginDef::new(move |ctx, config| {
        let (started, release, latest_started, latest_release) = gates.clone();
        let (applied, disposed) = (a.clone(), d.clone());
        async move {
            let value = value_of(&config);
            applied.push(value);
            if inject {
                assert_eq!(*ctx.get_as::<i32>("foo")?, 1);
            }
            if value == 1 {
                started.open();
                release.wait().await;
            } else if value == 3 {
                latest_started.open();
                latest_release.wait().await;
            }
            ctx.on_dispose(move || disposed.push(value))?;
            Ok(())
        }
    });
    if inject {
        def = def.inject(["foo"]);
    }
    let fiber = root.plugin_def(&def, json!({ "value": 0 })).unwrap();
    fiber.settled().await.unwrap();

    let settled = Seq::new();
    let first = track(fiber.update(json!({ "value": 1 })).unwrap(), &settled);
    started.wait().await;
    let second = track(fiber.update(json!({ "value": 2 })).unwrap(), &settled);
    let third = track(fiber.update(json!({ "value": 3 })).unwrap(), &settled);
    release.open();
    latest_started.wait().await;

    assert_eq!(applied.get(), vec![0, 1, 3]);
    assert_eq!(disposed.get(), vec![0, 1]);
    assert_eq!(settled.len(), 0);
    assert_eq!(fiber.state(), FiberState::Loading);
    latest_release.open();
    let _ = tokio::join!(first, second, third);

    assert_eq!(settled.get(), vec![true, true, true]);
    assert_eq!(fiber.config(), json!({ "value": 3 }));
    assert_eq!(fiber.state(), FiberState::Active);
    root.fiber().dispose().await.unwrap();
    assert_eq!(disposed.get(), vec![0, 1, 3]);
}

#[tokio::test]
async fn settles_concurrent_updates_on_the_latest_config() {
    concurrent_updates(false).await;
    concurrent_updates(true).await;
}

fn recording_values(applied: &Seq<i64>) -> PluginDef {
    let applied = applied.clone();
    PluginDef::new(move |_, config| {
        applied.push(value_of(&config));
        async { Ok(()) }
    })
}

#[tokio::test]
async fn skips_superseded_loads_before_executing_the_plugin() {
    let root = Context::new();
    let applied = Seq::new();
    let fiber = root.plugin_def(&recording_values(&applied), json!({ "value": 1 })).unwrap();
    let first = fiber.update(json!({ "value": 2 })).unwrap();
    let second = fiber.update(json!({ "value": 3 })).unwrap();
    let (a, b, c) = tokio::join!(first, second, fiber.settled());
    a.unwrap();
    b.unwrap();
    c.unwrap();
    assert_eq!(applied.get(), vec![3]);
    assert_eq!(fiber.state(), FiberState::Active);
}

#[tokio::test]
async fn coalesces_updates_during_unloading() {
    let root = Context::new();
    let (unloading, release) = (Gate::new(), Gate::new());
    let applied = Seq::new();
    let (a, u, r) = (applied.clone(), unloading.clone(), release.clone());
    let def = PluginDef::new(move |ctx, config| {
        let value = value_of(&config);
        a.push(value);
        let (unloading, release) = (u.clone(), r.clone());
        async move {
            ctx.defer(
                "dispose",
                Disposer::future(move || async move {
                    if value == 0 {
                        unloading.open();
                        release.wait().await;
                    }
                }),
            )?;
            Ok(())
        }
    });
    let fiber = root.plugin_def(&def, json!({ "value": 0 })).unwrap();
    fiber.settled().await.unwrap();
    let first = fiber.update(json!({ "value": 1 })).unwrap();
    unloading.wait().await;
    let second = fiber.update(json!({ "value": 2 })).unwrap();
    assert_eq!(applied.get(), vec![0]);
    assert_eq!(fiber.state(), FiberState::Unloading);
    release.open();
    let (a, b) = tokio::join!(first, second);
    a.unwrap();
    b.unwrap();
    assert_eq!(applied.get(), vec![0, 2]);
    assert_eq!(fiber.state(), FiberState::Active);
}

async fn honors_while_loading(restart: bool) {
    let root = Context::new();
    let (started, release) = (Gate::new(), Gate::new());
    let applied = Seq::new();
    let (a, s, r) = (applied.clone(), started.clone(), release.clone());
    let def = PluginDef::new(move |_, config| {
        a.push(value_of(&config));
        let (started, release) = (s.clone(), r.clone());
        async move {
            started.open();
            release.wait().await;
            Ok(())
        }
    });
    let original = json!({ "value": 1 });
    let fiber = root.plugin_def(&def, original.clone()).unwrap();
    started.wait().await;
    let updates = if restart {
        vec![fiber.restart()]
    } else {
        vec![fiber.update(json!({ "value": 2 })).unwrap(), fiber.update(original.clone()).unwrap()]
    };
    release.open();
    for update in futures::future::join_all(updates).await {
        update.unwrap();
    }
    assert_eq!(applied.get(), vec![1, 1]);
    assert_eq!(fiber.config(), original);
    assert_eq!(fiber.state(), FiberState::Active);
}

#[tokio::test]
async fn honors_restart_while_loading_even_when_config_is_unchanged() {
    honors_while_loading(true).await;
}

#[tokio::test]
async fn honors_config_aba_while_loading() {
    honors_while_loading(false).await;
}

#[tokio::test]
async fn cleans_up_a_superseded_failure_without_failing_the_latest_update() {
    let root = Context::new();
    let logged = errors(&root);
    let (started, release) = (Gate::new(), Gate::new());
    let (applied, disposed) = (Seq::new(), Seq::new());
    let (a, d, s, r) = (applied.clone(), disposed.clone(), started.clone(), release.clone());
    let def = PluginDef::new(move |ctx, config| {
        let value = value_of(&config);
        a.push(value);
        let (disposed, started, release) = (d.clone(), s.clone(), r.clone());
        async move {
            ctx.on_dispose(move || disposed.push(value))?;
            if value != 1 {
                return Ok(());
            }
            started.open();
            release.wait().await;
            Err(Error::msg("superseded load"))
        }
    });
    let fiber = root.plugin_def(&def, json!({ "value": 1 })).unwrap();
    started.wait().await;
    let updating = fiber.update(json!({ "value": 2 })).unwrap();
    release.open();
    let (a, b) = tokio::join!(updating, fiber.settled());
    a.unwrap();
    b.unwrap();
    assert_eq!(applied.get(), vec![1, 2]);
    assert_eq!(disposed.get(), vec![1]);
    assert_eq!(logged.get(), vec!["superseded load"]);
    assert_eq!(fiber.state(), FiberState::Active);
    root.fiber().dispose().await.unwrap();
    assert_eq!(disposed.get(), vec![1, 2]);
}

#[tokio::test]
async fn rejects_all_waiting_updates_when_the_latest_load_fails() {
    let root = Context::new();
    let (started, release) = (Gate::new(), Gate::new());
    let (applied, disposed) = (Seq::new(), Seq::new());
    let (a, d, s, r) = (applied.clone(), disposed.clone(), started.clone(), release.clone());
    let def = PluginDef::new(move |ctx, config| {
        let value = value_of(&config);
        a.push(value);
        let (disposed, started, release) = (d.clone(), s.clone(), r.clone());
        async move {
            ctx.on_dispose(move || disposed.push(value))?;
            if value == 1 {
                started.open();
                release.wait().await;
            } else if value == 2 {
                return Err(Error::msg("latest load failed"));
            }
            Ok(())
        }
    });
    let fiber = root.plugin_def(&def, json!({ "value": 0 })).unwrap();
    fiber.settled().await.unwrap();
    let first = fiber.update(json!({ "value": 1 })).unwrap();
    started.wait().await;
    let second = fiber.update(json!({ "value": 2 })).unwrap();
    release.open();
    let (first, second) = tokio::join!(first, second);
    for result in [first, second] {
        assert_eq!(result.unwrap_err().to_string(), "latest load failed");
    }
    assert_eq!(applied.get(), vec![0, 1, 2]);
    assert_eq!(disposed.get(), vec![0, 1, 2]);
    assert_eq!(fiber.state(), FiberState::Failed);
}

#[tokio::test]
async fn stops_an_outdated_load_at_its_checkpoint_and_collects_its_last_inverse() {
    let root = Context::new();
    let (started, release) = (Gate::new(), Gate::new());
    let events = Seq::new();
    let (e, s, r) = (events.clone(), started.clone(), release.clone());
    let def = PluginDef::new(move |ctx, config| {
        let value = value_of(&config);
        let (events, started, release) = (e.clone(), s.clone(), r.clone());
        async move {
            events.push(format!("load {value}"));
            if value != 1 {
                return Ok(());
            }
            started.open();
            release.wait().await;
            ctx.on_dispose(events.pusher("dispose 1".into()))?;
            ctx.checkpoint()?;
            events.push("outdated continuation".into());
            Ok(())
        }
    });
    let fiber = root.plugin_def(&def, json!({ "value": 1 })).unwrap();
    started.wait().await;
    let updating = fiber.update(json!({ "value": 2 })).unwrap();
    release.open();
    updating.await.unwrap();
    assert_eq!(events.get(), vec!["load 1", "dispose 1", "load 2"]);
    assert_eq!(fiber.state(), FiberState::Active);
}

#[tokio::test]
async fn waits_for_dependencies_and_loads_only_the_latest_config() {
    let root = Context::new();
    let applied = Seq::new();
    let a = applied.clone();
    let def = PluginDef::new(move |ctx, config| {
        let applied = a.clone();
        async move {
            assert_eq!(*ctx.get_as::<&str>("foo")?, "available");
            applied.push(value_of(&config));
            Ok(())
        }
    })
    .inject(["foo"]);
    let fiber = root.plugin_def(&def, json!({ "value": 0 })).unwrap();
    let (a, b) =
        tokio::join!(fiber.update(json!({ "value": 1 })).unwrap(), fiber.update(json!({ "value": 2 })).unwrap());
    a.unwrap();
    b.unwrap();
    assert_eq!(fiber.state(), FiberState::Pending);
    assert_eq!(applied.len(), 0);
    root.provide_as("foo", "available").unwrap();
    fiber.settled().await.unwrap();
    assert_eq!(applied.get(), vec![2]);
    assert_eq!(fiber.state(), FiberState::Active);
}

#[tokio::test]
async fn does_not_restart_when_a_hook_accepts_an_update_during_loading() {
    let root = Context::new();
    let (started, release) = (Gate::new(), Gate::new());
    let (applied, received) = (Seq::new(), Seq::new());
    let (a, r, s, rel) = (applied.clone(), received.clone(), started.clone(), release.clone());
    let def = PluginDef::new(move |ctx, config| {
        a.push(value_of(&config));
        let (received, started, release) = (r.clone(), s.clone(), rel.clone());
        async move {
            ctx.on_update(move |config: &Value| {
                received.push(value_of(config));
                true
            })?;
            started.open();
            release.wait().await;
            Ok(())
        }
    });
    let fiber = root.plugin_def(&def, json!({ "value": 1 })).unwrap();
    started.wait().await;
    fiber.update(json!({ "value": 2 })).unwrap().await.unwrap();
    release.open();
    fiber.settled().await.unwrap();
    assert_eq!(applied.get(), vec![1]);
    assert_eq!(received.get(), vec![2]);
    assert_eq!(fiber.config(), json!({ "value": 2 }));
    assert_eq!(fiber.state(), FiberState::Active);
}

#[tokio::test]
async fn does_not_load_a_queued_config_after_disposal() {
    let root = Context::new();
    let (started, release) = (Gate::new(), Gate::new());
    let applied = Seq::new();
    let disposed = Arc::new(AtomicUsize::new(0));
    let (a, d, s, r) = (applied.clone(), disposed.clone(), started.clone(), release.clone());
    let def = PluginDef::new(move |ctx, config| {
        a.push(value_of(&config));
        let (disposed, started, release) = (d.clone(), s.clone(), r.clone());
        async move {
            started.open();
            release.wait().await;
            ctx.on_dispose(move || {
                disposed.fetch_add(1, Ordering::SeqCst);
            })?;
            Ok(())
        }
    });
    let fiber = root.plugin_def(&def, json!({ "value": 1 })).unwrap();
    started.wait().await;
    let updating = fiber.update(json!({ "value": 2 })).unwrap();
    let disposing = fiber.dispose();
    release.open();
    let (_, b) = tokio::join!(updating, disposing);
    b.unwrap();
    assert_eq!(applied.get(), vec![1]);
    assert_eq!(disposed.load(Ordering::SeqCst), 1);
    assert_eq!(fiber.state(), FiberState::Disposed);
}

#[tokio::test]
async fn warns_about_declared_dependency_cycles() {
    let root = Context::new();
    let warnings = common::capture(&root, Level::Warn);
    let a = PluginDef::new(|_, _| async { Ok(()) }).name("a").inject(["y"]).provides(["x"]);
    let b = PluginDef::new(|_, _| async { Ok(()) }).name("b").inject(["x"]).provides(["y"]);
    let a = root.plugin_def(&a, Value::Null).unwrap();
    let b = root.plugin_def(&b, Value::Null).unwrap();
    settle().await;
    assert_eq!(warnings.get(), vec!["dependency cycle: b -> a -> b"]);
    assert_eq!((a.state(), b.state()), (FiberState::Pending, FiberState::Pending));
}
