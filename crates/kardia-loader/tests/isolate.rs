//! Port of `packages/loader/tests/isolate.spec.ts` and `isolate-provide.spec.ts`.

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use common::{Counter, Gate, Mock, settle};
use kardia_core::{Context, Disposer, EffectHandle, Fiber, FiberState, ImplId, PluginDef, ServiceChanged};
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
                let tag = |ctx: &kardia_core::Context| {
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

/// `foo` (injecting `bar`) and `bar` (providing its config).
fn realm_mock(mock: &Mock) -> Counter {
    let injector = Counter::default();
    mock.mock("foo", injector.def().inject(["bar"]));
    mock.mock("bar", provider("bar"));
    injector
}

fn bar_value(fiber: &Fiber) -> Option<Value> {
    fiber.context().get_as::<Value>("bar").ok().map(|bar| bar["value"].clone())
}

#[tokio::test]
async fn special_case_nested_realms() {
    let mock = Mock::boot().await;
    let foo = realm_mock(&mock);
    let create = |options: Value, parent: Option<String>| {
        let loader = mock.loader.clone();
        async move { loader.create(options, parent.as_deref(), None).await.unwrap() }
    };
    let outer = create(json!({ "name": "group", "config": [] }), None).await;
    let inner =
        create(json!({ "name": "group", "isolate": { "bar": "custom" }, "config": [] }), Some(outer.clone())).await;
    create(json!({ "name": "bar", "config": { "value": "custom" } }), Some(inner.clone())).await;
    let alpha = create(json!({ "name": "foo", "isolate": { "bar": "custom" } }), None).await;
    let beta = create(json!({ "name": "foo" }), Some(inner)).await;
    settle().await;
    assert_eq!(bar_value(&mock.fiber(&alpha)), Some(json!("custom")));
    assert_eq!(bar_value(&mock.fiber(&beta)), Some(json!("custom")));
    foo.take();

    mock.loader.update(&outer, json!({ "isolate": { "bar": "custom" } }), None, None).await.unwrap();
    settle().await;
    assert_eq!(foo.take(), (0, 0));
    mock.loader.update(&outer, json!({ "isolate": {} }), None, None).await.unwrap();
    settle().await;
    assert_eq!(foo.take(), (0, 0));
}

#[tokio::test]
async fn special_case_change_provider() {
    let mock = Mock::boot().await;
    let foo = realm_mock(&mock);
    for label in ["alpha", "beta"] {
        let options = json!({ "name": "bar", "isolate": { "bar": label }, "config": { "value": label } });
        mock.loader.create(options, None, None).await.unwrap();
    }
    let group = mock
        .loader
        .create(json!({ "name": "group", "isolate": { "bar": "alpha" }, "config": [] }), None, None)
        .await
        .unwrap();
    let id = mock.loader.create(json!({ "name": "foo" }), Some(&group), None).await.unwrap();
    settle().await;
    assert_eq!(foo.take(), (1, 0));
    let fiber = mock.fiber(&id);
    assert_eq!(bar_value(&fiber), Some(json!("alpha")));

    mock.loader.update(&group, json!({ "isolate": { "bar": "beta" } }), None, None).await.unwrap();
    settle().await;
    assert_eq!(foo.take(), (1, 1));
    assert_eq!(bar_value(&fiber), Some(json!("beta")));
}

#[tokio::test]
async fn special_case_change_injector() {
    let mock = Mock::boot().await;
    let foo = realm_mock(&mock);
    let alpha = mock.loader.create(json!({ "name": "foo", "isolate": { "bar": "alpha" } }), None, None).await.unwrap();
    let beta = mock.loader.create(json!({ "name": "foo", "isolate": { "bar": "beta" } }), None, None).await.unwrap();
    let group = mock
        .loader
        .create(json!({ "name": "group", "isolate": { "bar": "alpha" }, "config": [] }), None, None)
        .await
        .unwrap();
    let inner = mock.loader.create(json!({ "name": "bar" }), Some(&group), None).await.unwrap();
    started(&mock, &inner).await;
    settle().await;
    assert_eq!(foo.take(), (1, 0));
    let (fiber1, fiber2) = (mock.fiber(&alpha), mock.fiber(&beta));
    assert!(fiber1.context().get_as::<Value>("bar").is_ok());
    assert!(fiber2.context().get_as::<Value>("bar").is_err());

    mock.loader.update(&group, json!({ "isolate": { "bar": "beta" } }), None, None).await.unwrap();
    settle().await;
    assert_eq!(foo.take(), (1, 1));
    assert!(fiber1.context().get_as::<Value>("bar").is_err());
    assert!(fiber2.context().get_as::<Value>("bar").is_ok());
}

// --- isolate-provide.spec.ts ---

fn slots(mock: &Mock) -> Vec<(kardia_core::RealmId, ImplId)> {
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
async fn keeps_impls_on_intermediate_symbols_across_repeated_moves() {
    let mock = Mock::boot().await;
    let calls = Arc::new(Mutex::new(0));
    mock.mock("prov", tagged("prov", calls.clone()));
    mock.mock("rootUser", tagged("rootUser", Arc::default()));
    mock.mock("keep", PluginDef::new(|_, _| async { Ok(()) }));
    mock.mock("onA", tagged("onA", Arc::default()));
    let create = |options: Value| {
        let mock = &mock;
        async move {
            let id = mock.loader.create(options, None, None).await.unwrap();
            started(mock, &id).await;
            id
        }
    };
    let id = create(json!({ "name": "prov" })).await;
    let (root_realm, original) = slots(&mock)[0];
    let realm_of = |impl_id: ImplId| slots(&mock).into_iter().find(|(_, id)| *id == impl_id).map(|(realm, _)| realm);
    let new_slot = |known: &[ImplId]| slots(&mock).into_iter().find(|(_, id)| !known.contains(id)).unwrap();

    mock.loader.update(&id, json!({ "isolate": { "svc": "A" } }), None, None).await.unwrap();
    assert_eq!(*calls.lock(), 1);
    assert_eq!(slots(&mock).len(), 1);
    let at_a = realm_of(original).unwrap();
    assert_ne!(at_a, root_realm);

    create(json!({ "name": "rootUser" })).await;
    let root_user = new_slot(&[original]);
    assert_eq!(root_user.0, root_realm);

    create(json!({ "name": "keep", "isolate": { "svc": "A" } })).await;
    mock.loader.update(&id, json!({ "isolate": { "svc": "B" } }), None, None).await.unwrap();
    assert_eq!(*calls.lock(), 1);
    let at_b = realm_of(original).unwrap();
    assert!(at_b != at_a && at_b != root_realm);

    // the label A is still in use, so a new provider reuses its realm
    create(json!({ "name": "onA", "isolate": { "svc": "A" } })).await;
    let on_a = new_slot(&[original, root_user.1]);
    assert_eq!(on_a.0, at_a);

    mock.loader.update(&id, json!({ "isolate": { "svc": "C" } }), None, None).await.unwrap();
    assert_eq!(*calls.lock(), 1);
    let at_c = realm_of(original).unwrap();
    assert!(![root_realm, at_a, at_b].contains(&at_c));

    mock.fiber(&id).dispose().await.unwrap();
    let left = slots(&mock);
    assert!(!left.iter().any(|(_, id)| *id == original));
    assert!(left.contains(&root_user) && left.contains(&on_a));
}

#[tokio::test]
async fn notifies_the_retained_realm_on_disposal_after_a_blocked_move() {
    for mode in ["fiber", "provide"] {
        let mock = Mock::boot().await;
        let gate = Gate::new();
        let provisions: Arc<Mutex<HashMap<String, EffectHandle>>> = Default::default();
        let consumers: Arc<Mutex<HashMap<String, Context>>> = Default::default();
        let (starts, stops) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(Vec::new())));
        let p = provisions.clone();
        mock.mock(
            "provider",
            PluginDef::new(move |ctx, config| {
                let provisions = p.clone();
                async move {
                    let tag = config["tag"].as_str().unwrap().to_string();
                    let provision = ctx.provide_as("svc", tag.clone())?;
                    provisions.lock().insert(tag, provision);
                    Ok(())
                }
            }),
        );
        let (c, s1, s2, g) = (consumers.clone(), starts.clone(), stops.clone(), gate.clone());
        mock.mock(
            "consumer",
            PluginDef::new(move |ctx, config| {
                let (consumers, starts, stops, gate) = (c.clone(), s1.clone(), s2.clone(), g.clone());
                async move {
                    consumers.lock().insert(config["tag"].as_str().unwrap().to_string(), ctx.clone());
                    starts.lock().push(svc(&ctx).unwrap());
                    let reader = ctx.clone();
                    ctx.defer(
                        "cleanup",
                        Disposer::future(move || async move {
                            gate.wait().await;
                            stops.lock().push(svc(&reader).unwrap());
                        }),
                    )?;
                    Ok(())
                }
            })
            .inject(["svc"]),
        );
        let mut ids = Vec::new();
        for (name, tag, label) in
            [("provider", "first", "A"), ("provider", "second", "B"), ("consumer", "A", "A"), ("consumer", "B", "B")]
        {
            let options = json!({ "name": name, "config": { "tag": tag }, "isolate": { "svc": label } });
            ids.push(mock.loader.create(options, None, None).await.unwrap());
        }
        for id in &ids {
            started(&mock, id).await;
        }
        let [first, _, ca, cb] = ids.try_into().unwrap();
        let realm_a = mock.fiber(&first).context().realm("svc").unwrap();
        let before = slots(&mock);
        let a = *before.iter().find(|(realm, _)| *realm == realm_a).unwrap();
        let b = *before.iter().find(|slot| **slot != a).unwrap();

        // realm B is taken, so the binding of `first` stays in realm A
        mock.loader.update(&first, json!({ "isolate": { "svc": "B" } }), None, None).await.unwrap();
        assert!(slots(&mock).contains(&a) && slots(&mock).contains(&b));
        assert_eq!(svc(mock.fiber(&first).context()).unwrap(), "first");
        assert_eq!(*starts.lock(), vec!["first", "second"]);
        assert!(stops.lock().is_empty());

        let (events_a, events_b) = (Arc::new(Mutex::new(0)), Arc::new(Mutex::new(0)));
        for (realm, events) in [(a.0, events_a.clone()), (b.0, events_b.clone())] {
            mock.root
                .isolate_in("svc", realm)
                .on(move |event: &ServiceChanged| {
                    if event.key.as_str() == "svc" {
                        *events.lock() += 1;
                    }
                })
                .unwrap();
        }
        let disposal = match mode {
            "fiber" => mock.fiber(&first).dispose(),
            _ => provisions.lock()["first"].dispose(),
        };
        let task = tokio::spawn(disposal);
        settle().await;
        let completed_during_cleanup = task.is_finished();
        gate.open();
        task.await.unwrap().unwrap();
        mock.loader.settled().await;
        assert!(!completed_during_cleanup, "{mode}");
        assert_eq!(*stops.lock(), vec!["first"]);
        assert_eq!(mock.fiber(&ca).state(), FiberState::Pending);
        assert!(svc(&consumers.lock()["A"]).is_none());
        assert_eq!(mock.fiber(&cb).state(), FiberState::Active);
        assert_eq!(svc(&consumers.lock()["B"]).unwrap(), "second");
        assert_eq!(slots(&mock), vec![b]);
        assert!(*events_a.lock() > 0, "{mode}");
        assert_eq!(*events_b.lock(), 0);

        let replacement = json!({ "name": "provider", "config": { "tag": "replacement" }, "isolate": { "svc": "A" } });
        let replacement = mock.loader.create(replacement, None, None).await.unwrap();
        started(&mock, &replacement).await;
        mock.loader.settled().await;
        assert_eq!(mock.fiber(&ca).state(), FiberState::Active);
        assert_eq!(svc(&consumers.lock()["A"]).unwrap(), "replacement");
        assert_eq!(*starts.lock(), vec!["first", "second", "replacement"]);
        assert_eq!(*stops.lock(), vec!["first"]);
        assert_eq!(mock.fiber(&cb).state(), FiberState::Active);
        assert_eq!(svc(&consumers.lock()["B"]).unwrap(), "second");
        assert_eq!(*events_b.lock(), 0);
    }
}

fn svc(ctx: &Context) -> Option<String> {
    ctx.get_as::<String>("svc").ok().map(|svc| (*svc).clone())
}
