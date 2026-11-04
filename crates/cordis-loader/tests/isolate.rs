//! Port of `packages/loader/tests/isolate.spec.ts` and `isolate-provide.spec.ts`.

mod common;

use std::sync::Arc;

use common::{Counter, Gate, Mock, settle};
use cordis_core::{Disposer, Fiber, FiberState, ImplId, PluginDef};
use parking_lot::Mutex;
use serde_json::{Value, json};

/// Provides `bar` with its config as the value.
fn provider(key: &'static str) -> PluginDef {
    PluginDef::new(move |ctx, config| async move {
        ctx.provide_as(key, config)?;
        Ok(())
    })
}

async fn started(mock: &Mock, id: &str) -> Fiber {
    let fiber = mock.fiber(id);
    let _ = fiber.settled().await;
    fiber
}

#[tokio::test]
async fn basic_isolation() {
    let mock = Mock::boot().await;
    let foo = Counter::default();
    mock.mock("foo", foo.def().inject(["bar"]));
    mock.mock("bar", provider("bar"));
    let provider = mock.loader.create(json!({ "name": "bar" }), None, None).await.unwrap();
    let injector = mock.loader.create(json!({ "name": "foo" }), None, None).await.unwrap();
    settle().await;
    assert_eq!(foo.take(), (1, 0), "initiate");

    let steps = [
        (&injector, json!({ "isolate": { "bar": true } }), (0, 1), "add isolate on injector (relevant)"),
        (&injector, json!({ "isolate": { "bar": true, "qux": true } }), (0, 0), "add isolate on injector (irrelevant)"),
        (&injector, json!({ "isolate": { "qux": true } }), (1, 0), "remove isolate on injector (relevant)"),
        (&injector, json!({ "isolate": null }), (0, 0), "remove isolate on injector (irrelevant)"),
        (&provider, json!({ "isolate": { "bar": true } }), (0, 1), "add isolate on provider (relevant)"),
        (&provider, json!({ "isolate": { "bar": true, "qux": true } }), (0, 0), "add isolate on provider (irrelevant)"),
        (&provider, json!({ "isolate": { "qux": true } }), (1, 0), "remove isolate on provider (relevant)"),
        (&provider, json!({ "isolate": null }), (0, 0), "remove isolate on provider (irrelevant)"),
    ];
    for (id, patch, expected, label) in steps {
        mock.loader.update(id, patch, None, None).await.unwrap();
        settle().await;
        assert_eq!(foo.take(), expected, "{label}");
    }
}

#[tokio::test]
async fn realms_of_groups() {
    let mock = Mock::boot().await;
    let foo = Counter::default();
    mock.mock("foo", foo.def().inject(["bar"]));
    let bar = mock.mock("bar", provider("bar"));
    let group = |isolate: Value, value: &str| json!({ "name": "group", "isolate": { "bar": isolate }, "config": [{ "name": "bar", "config": { "value": value } }] });
    let alpha = mock.loader.create(group(json!(true), "alpha"), None, None).await.unwrap();
    mock.loader.create(group(json!("beta"), "beta"), None, None).await.unwrap();
    settle().await;
    assert_eq!(mock.root.registry().fibers(&bar).len(), 2);
    assert_eq!(foo.take(), (0, 0));

    mock.loader.update(&alpha, json!({ "isolate": { "bar": true } }), None, None).await.unwrap();
    settle().await;
    assert_eq!(mock.root.registry().fibers(&bar).len(), 2);
    assert_eq!(foo.take(), (0, 0), "update isolate group (no change)");

    let create = |isolate: Value| {
        let options = match isolate {
            Value::Null => json!({ "name": "foo" }),
            isolate => json!({ "name": "foo", "isolate": { "bar": isolate } }),
        };
        let loader = mock.loader.clone();
        let alpha = alpha.clone();
        async move { loader.create(options, Some(&alpha), None).await.unwrap() }
    };
    let nested1 = create(Value::Null).await;
    let nested2 = create(json!("beta")).await;
    let nested3 = create(json!(true)).await;
    settle().await;
    assert_eq!(foo.take(), (2, 0));
    let value = |id: &str| mock.fiber(id).context().get_as::<Value>("bar").ok().map(|v| v["value"].clone());
    assert_eq!(value(&nested1), Some(json!("alpha")));
    assert_eq!(value(&nested2), Some(json!("beta")));
    assert_eq!(value(&nested3), None);
    assert_eq!(mock.fiber(&nested3).state(), FiberState::Pending);
}

#[tokio::test]
async fn transfer_between_realms() {
    let mock = Mock::boot().await;
    let foo = Counter::default();
    mock.mock("foo", foo.def().inject(["bar"]));
    mock.mock("bar", provider("bar"));
    let group = mock
        .loader
        .create(json!({ "name": "group", "isolate": { "bar": true }, "config": [] }), None, None)
        .await
        .unwrap();
    let provider = mock.loader.create(json!({ "name": "bar" }), None, None).await.unwrap();
    let injector = mock.loader.create(json!({ "name": "foo" }), None, None).await.unwrap();
    settle().await;
    assert_eq!(foo.take(), (1, 0));
    let steps = [
        (&injector, Some(group.as_str()), (0, 1), "transfer injector into group"),
        (&provider, Some(group.as_str()), (1, 0), "transfer provider into group"),
        (&injector, None, (0, 1), "transfer injector out of group"),
        (&provider, None, (1, 0), "transfer provider out of group"),
    ];
    for (id, parent, expected, label) in steps {
        mock.loader.update(id, json!({}), Some(parent), None).await.unwrap();
        settle().await;
        assert_eq!(foo.take(), expected, "{label}");
    }
}

#[tokio::test]
async fn keeps_committed_views_of_a_parent_and_a_pinned_child_across_a_realm_move() {
    let mock = Mock::boot().await;
    let (loads, unloads) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(Vec::new())));
    mock.mock("bar", provider("bar"));
    let (l, u) = (loads.clone(), unloads.clone());
    mock.mock(
        "parent",
        PluginDef::new(move |ctx, _| {
            let (loads, unloads) = (l.clone(), u.clone());
            async move {
                let tag = |ctx: &cordis_core::Context| {
                    ctx.get_as::<Value>("bar").unwrap()["tag"].as_str().unwrap().to_string()
                };
                // the child is pinned to the realm the parent resolves `bar` in
                let pinned = ctx.isolate_in("bar", ctx.realm("bar").unwrap());
                let (child_loads, child_unloads) = (loads.clone(), unloads.clone());
                pinned.inject(["bar"], move |ctx| {
                    let (loads, unloads) = (child_loads.clone(), child_unloads.clone());
                    async move {
                        loads.lock().push(format!("child:{}", tag(&ctx)));
                        let reader = ctx.clone();
                        ctx.on_dispose(move || unloads.lock().push(format!("child:{}", tag(&reader))))?;
                        Ok(())
                    }
                })?;
                let reader = ctx.clone();
                ctx.on_dispose(move || unloads.lock().push(format!("parent:{}", tag(&reader))))?;
                Ok(())
            }
        })
        .inject(["bar"]),
    );
    mock.loader
        .create(json!({ "name": "bar", "isolate": { "bar": "a" }, "config": { "tag": "a" } }), None, None)
        .await
        .unwrap();
    mock.loader
        .create(json!({ "name": "bar", "isolate": { "bar": "b" }, "config": { "tag": "b" } }), None, None)
        .await
        .unwrap();
    let id = mock.loader.create(json!({ "name": "parent", "isolate": { "bar": "a" } }), None, None).await.unwrap();
    settle().await;
    assert_eq!(*loads.lock(), vec!["child:a"]);

    mock.loader.update(&id, json!({ "isolate": { "bar": "b" } }), None, None).await.unwrap();
    settle().await;
    assert_eq!(*loads.lock(), vec!["child:a", "child:b"]);
    let mut unloaded = unloads.lock().clone();
    unloaded.sort();
    assert_eq!(unloaded, vec!["child:a", "parent:a"]);
    assert!(mock.root.log_buffer().is_empty());
}

// --- isolate-provide.spec.ts ---

fn slots(mock: &Mock) -> Vec<(cordis_core::RealmId, ImplId)> {
    mock.root.bindings("svc")
}

fn tagged(tag: &'static str, calls: Arc<Mutex<usize>>) -> PluginDef {
    PluginDef::new(move |ctx, config| {
        *calls.lock() += 1;
        let tag = config["tag"].as_str().map(String::from).unwrap_or_else(|| tag.to_string());
        async move {
            ctx.provide_as("svc", tag)?;
            Ok(())
        }
    })
}

#[tokio::test]
async fn keeps_self_access_while_the_provision_context_changes_realms() {
    let mock = Mock::boot().await;
    let reads = Arc::new(Mutex::new(Vec::new()));
    let r = reads.clone();
    mock.mock(
        "prov",
        PluginDef::new(move |ctx, _| {
            let reads = r.clone();
            async move {
                ctx.provide_as("svc", "prov")?;
                let reader = ctx.clone();
                ctx.on_update(move |_: &Value| {
                    reads.lock().push(reader.get_as::<&str>("svc").map(|v| *v).unwrap_or("missing"));
                    true
                })?;
                Ok(())
            }
        }),
    );
    let id = mock.loader.create(json!({ "name": "prov", "config": { "revision": 1 } }), None, None).await.unwrap();
    let fiber = started(&mock, &id).await;
    mock.loader
        .update(&id, json!({ "isolate": { "svc": "A" }, "config": { "revision": 2 } }), None, None)
        .await
        .unwrap();
    assert_eq!(*reads.lock(), vec!["prov"]);
    assert_eq!(*fiber.context().get_as::<&str>("svc").unwrap(), "prov");
}

#[tokio::test]
async fn migrates_a_provider_without_restart_and_reloads_it_in_place() {
    let mock = Mock::boot().await;
    let calls = Arc::new(Mutex::new(0));
    mock.mock("prov", tagged("init", calls.clone()));
    let id = mock.loader.create(json!({ "name": "prov", "config": { "tag": "init" } }), None, None).await.unwrap();
    started(&mock, &id).await;
    let before = slots(&mock);
    assert_eq!(before.len(), 1);

    mock.loader.update(&id, json!({ "isolate": { "svc": true } }), None, None).await.unwrap();
    assert_eq!(*calls.lock(), 1);
    let after = slots(&mock);
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].1, before[0].1, "same binding");
    assert_ne!(after[0].0, before[0].0, "new realm");

    mock.loader.update(&id, json!({ "config": { "tag": "reloaded" } }), None, None).await.unwrap();
    started(&mock, &id).await;
    assert_eq!(*calls.lock(), 2);
    let reloaded = slots(&mock);
    assert_eq!(reloaded.len(), 1);
    assert_ne!(reloaded[0].1, before[0].1);
    assert_eq!(reloaded[0].0, after[0].0, "the private realm stays");
}

#[tokio::test]
async fn keeps_an_impl_that_reused_the_abandoned_realm() {
    let mock = Mock::boot().await;
    let calls = Arc::new(Mutex::new(0));
    mock.mock("prov", tagged("prov", calls.clone()));
    mock.mock("other", tagged("other", Arc::default()));
    let id = mock.loader.create(json!({ "name": "prov" }), None, None).await.unwrap();
    started(&mock, &id).await;
    let before = slots(&mock)[0];
    mock.loader.update(&id, json!({ "isolate": { "svc": true } }), None, None).await.unwrap();
    assert_eq!(*calls.lock(), 1);
    let moved = slots(&mock)[0];
    assert_eq!(moved.1, before.1);
    assert_ne!(moved.0, before.0);

    let other = mock.loader.create(json!({ "name": "other" }), None, None).await.unwrap();
    started(&mock, &other).await;
    let other_slot = *slots(&mock).iter().find(|(_, id)| *id != before.1).unwrap();
    assert_eq!(other_slot.0, before.0, "the other provider takes the abandoned root realm");

    mock.fiber(&id).dispose().await.unwrap();
    let left = slots(&mock);
    assert!(!left.iter().any(|(_, id)| *id == before.1));
    assert!(left.contains(&other_slot));
}

#[tokio::test]
async fn retains_a_moved_provider_until_its_consumers_finish_cleanup() {
    let mock = Mock::boot().await;
    let gate = Gate::new();
    let timeline = Arc::new(Mutex::new(Vec::new()));
    let t = timeline.clone();
    mock.mock(
        "provider",
        PluginDef::new(move |ctx, config| {
            let timeline = t.clone();
            async move {
                let tag = config["tag"].as_str().unwrap().to_string();
                ctx.provide_as("svc", tag.clone())?;
                ctx.on_dispose(move || timeline.lock().push(format!("closed: {tag}")))?;
                Ok(())
            }
        }),
    );
    let (t, g) = (timeline.clone(), gate.clone());
    mock.mock(
        "consumer",
        PluginDef::new(move |ctx, _| {
            let (timeline, gate) = (t.clone(), g.clone());
            async move {
                let reader = ctx.clone();
                ctx.defer(
                    "cleanup",
                    Disposer::future(move || async move {
                        gate.wait().await;
                        timeline.lock().push(format!("read: {}", *reader.get_as::<String>("svc").unwrap()));
                    }),
                )?;
                Ok(())
            }
        })
        .inject(["svc"]),
    );
    let id = mock.loader.create(json!({ "name": "provider", "config": { "tag": "moved" } }), None, None).await.unwrap();
    let provider = started(&mock, &id).await;
    let original = slots(&mock)[0];
    mock.loader.update(&id, json!({ "isolate": { "svc": "B" } }), None, None).await.unwrap();
    let moved = slots(&mock)[0];
    assert_eq!(moved.1, original.1);

    let other =
        mock.loader.create(json!({ "name": "provider", "config": { "tag": "other" } }), None, None).await.unwrap();
    started(&mock, &other).await;
    let other_slot = *slots(&mock).iter().find(|(_, id)| *id != original.1).unwrap();
    assert_eq!(other_slot.0, original.0);
    let consumer =
        mock.loader.create(json!({ "name": "consumer", "isolate": { "svc": "B" } }), None, None).await.unwrap();
    started(&mock, &consumer).await;

    let task = provider.dispose();
    settle().await;
    let during_cleanup = timeline.lock().clone();
    gate.open();
    task.await.unwrap();
    mock.loader.settled().await;
    assert!(during_cleanup.is_empty());
    assert_eq!(*timeline.lock(), vec!["read: moved", "closed: moved"]);
    assert_eq!(slots(&mock), vec![other_slot]);
    assert_eq!(mock.fiber(&consumer).state(), FiberState::Pending);
}

#[tokio::test]
async fn does_not_delete_another_realm_when_one_impl_moves() {
    let mock = Mock::boot().await;
    let calls = Arc::new(Mutex::new(0));
    mock.mock("stay", tagged("stay", Arc::default()));
    mock.mock("move", tagged("move", calls.clone()));
    let stay = mock.loader.create(json!({ "name": "stay", "isolate": { "svc": "stay" } }), None, None).await.unwrap();
    let mover = mock.loader.create(json!({ "name": "move", "isolate": { "svc": "from" } }), None, None).await.unwrap();
    started(&mock, &stay).await;
    started(&mock, &mover).await;
    let [first, second]: [_; 2] = slots(&mock).try_into().unwrap();
    mock.loader.update(&mover, json!({ "isolate": { "svc": "to" } }), None, None).await.unwrap();
    assert_eq!(*calls.lock(), 1);
    let after = slots(&mock);
    assert!(after.contains(&first), "the other realm stays");
    let moved = after.iter().find(|(_, id)| *id == second.1).unwrap();
    assert_ne!(moved.0, second.0);
    mock.fiber(&mover).dispose().await.unwrap();
    assert_eq!(slots(&mock), vec![first]);
}

#[tokio::test]
async fn blocked_moves_keep_the_binding_where_it_lives() {
    let mock = Mock::boot().await;
    mock.mock("provider", tagged("", Arc::default()));
    let starts = Arc::new(Mutex::new(Vec::new()));
    let s = starts.clone();
    mock.mock(
        "consumer",
        PluginDef::new(move |ctx, _| {
            s.lock().push(ctx.get_as::<String>("svc").map(|v| (*v).clone()).unwrap_or_default());
            async { Ok(()) }
        })
        .inject(["svc"]),
    );
    let first = mock
        .loader
        .create(json!({ "name": "provider", "config": { "tag": "first" }, "isolate": { "svc": "A" } }), None, None)
        .await
        .unwrap();
    let second = mock
        .loader
        .create(json!({ "name": "provider", "config": { "tag": "second" }, "isolate": { "svc": "B" } }), None, None)
        .await
        .unwrap();
    for (tag, label) in [("A", "A"), ("B", "B")] {
        mock.loader
            .create(json!({ "name": "consumer", "config": { "tag": tag }, "isolate": { "svc": label } }), None, None)
            .await
            .unwrap();
    }
    mock.loader.settled().await;
    let before = slots(&mock);
    // realm B is taken, so the binding of `first` stays in realm A
    mock.loader.update(&first, json!({ "isolate": { "svc": "B" } }), None, None).await.unwrap();
    mock.loader.settled().await;
    assert_eq!(slots(&mock), before);
    assert_eq!(*mock.fiber(&first).context().get_as::<String>("svc").unwrap(), "first");
    let mut started = starts.lock().clone();
    started.sort();
    assert_eq!(started, vec!["first", "second"]);
    // withdrawing it still notifies the realm it really lives in
    mock.fiber(&first).dispose().await.unwrap();
    mock.loader.settled().await;
    assert_eq!(slots(&mock).len(), 1);
    let _ = second;
}
