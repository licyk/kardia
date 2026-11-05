//! Port of `packages/loader/tests/{index,group,commit,validate}.spec.ts`.

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use common::{Counter, Mock, settle};
use cordis_core::{Error, FiberState, PluginDef};
use cordis_loader::{EntryChange, EntryValidationError, TreeBackend, validate_options};
use parking_lot::Mutex;
use serde_json::{Value, json};

fn noop() -> PluginDef {
    PluginDef::new(|_, _| async { Ok(()) })
}

fn ids(changes: &[EntryChange]) -> Vec<String> {
    changes.iter().map(|change| change.id.clone()).collect()
}

#[tokio::test]
async fn basic_support() {
    let mock = Mock::boot().await;
    let (foo, bar, qux) = (Counter::default(), Counter::default(), Counter::default());
    let foo_def = mock.mock("foo", foo.def());
    let bar_def = mock.mock("bar", bar.def());
    let qux_def = mock.mock("qux", qux.def());

    mock.read(json!([
        { "id": "1", "name": "foo" },
        { "id": "2", "name": "group", "group": true, "config": [
            { "id": "3", "name": "bar", "config": { "a": 1 } },
            { "id": "4", "name": "qux", "disabled": true },
        ] },
    ]))
    .await;
    assert!(mock.enabled(&foo_def) && mock.enabled(&bar_def) && !mock.enabled(&qux_def));
    assert_eq!((foo.take().0, bar.take().0, qux.take().0), (1, 1, 0));

    // loader update
    mock.read(json!([{ "id": "1", "name": "foo" }, { "id": "4", "name": "qux" }])).await;
    assert!(mock.enabled(&foo_def) && !mock.enabled(&bar_def) && mock.enabled(&qux_def));
    assert_eq!((foo.take().0, bar.take().0, qux.take().0), (0, 0, 1));

    // plugin self-update
    mock.fiber("1").update(json!({ "a": 3 })).unwrap().await.unwrap();
    settle().await;
    assert_eq!(
        mock.data(),
        vec![json!({ "id": "1", "name": "foo", "config": { "a": 3 } }), json!({ "id": "4", "name": "qux" })]
    );

    // plugin self-dispose
    mock.fiber("1").dispose().await.unwrap();
    settle().await;
    assert_eq!(
        mock.data(),
        vec![
            json!({ "id": "1", "name": "foo", "disabled": true, "config": { "a": 3 } }),
            json!({ "id": "4", "name": "qux" })
        ]
    );
}

#[tokio::test]
async fn awaits_the_loader_barrier() {
    let mock = Mock::boot().await;
    let gate = Arc::new(tokio::sync::Notify::new());
    let g = gate.clone();
    mock.mock(
        "foo",
        PluginDef::new(move |_, _| {
            let gate = g.clone();
            async move {
                gate.notified().await;
                Ok(())
            }
        }),
    );
    mock.mock("bar", noop().inject(["never"]));
    mock.mock("qux", noop());
    let foo = mock.loader.create(json!({ "name": "foo" }), None, None).await.unwrap();
    let bar = mock.loader.create(json!({ "name": "bar" }), None, None).await.unwrap();
    let qux = mock
        .loader
        .create(json!({ "name": "qux", "inject": { "loader": { "await": true } } }), None, None)
        .await
        .unwrap();
    settle().await;
    assert_eq!(mock.fiber(&foo).state(), FiberState::Loading);
    assert_eq!(mock.fiber(&bar).state(), FiberState::Pending);
    let qux = mock.loader.resolve(&qux).unwrap();
    assert_eq!(qux.outer_fiber().unwrap().state(), FiberState::Pending);
    assert!(qux.inner_fiber().is_none());

    gate.notify_one();
    settle().await;
    assert_eq!(mock.fiber(&foo).state(), FiberState::Active);
    assert_eq!(mock.fiber(&bar).state(), FiberState::Pending);
    assert_eq!(qux.inner_fiber().unwrap().state(), FiberState::Active);
}

#[tokio::test]
async fn entry_failure() {
    let mock = Mock::boot().await;
    mock.mock("bad", PluginDef::new(|_, _| async { Err(Error::msg("boom")) }));
    mock.mock("good", noop());
    mock.read(json!([{ "id": "1", "name": "bad" }, { "id": "2", "name": "good" }])).await;
    assert_eq!(mock.fiber("1").state(), FiberState::Failed);
    assert_eq!(mock.fiber("2").state(), FiberState::Active);

    // on config update
    mock.read(json!([{ "id": "1", "name": "bad", "config": { "a": 1 } }, { "id": "2", "name": "good" }])).await;
    assert_eq!(mock.fiber("1").state(), FiberState::Failed);
    assert_eq!(mock.fiber("2").state(), FiberState::Active);

    // an id containing the separator is rejected
    mock.read(json!([{ "id": "2", "name": "good" }, { "id": "3:4", "name": "good" }])).await;
    assert!(mock.loader.resolve("3:4").is_err());
    assert_eq!(mock.fiber("2").state(), FiberState::Active);
}

#[tokio::test]
async fn nested_plugins_keep_their_own_config() {
    let mock = Mock::boot().await;
    let child = Counter::default();
    let child_def = child.def();
    mock.mock(
        "parent",
        PluginDef::new(move |ctx, _| {
            let child = child_def.clone();
            async move {
                ctx.plugin_def(&child, json!({ "value": { "__jsExpr": "foo.value" } }))?;
                Ok(())
            }
        }),
    );
    mock.read(json!([{ "id": "1", "name": "parent" }])).await;
    settle().await;
    assert_eq!(child.configs.lock().clone(), vec![json!({ "value": { "__jsExpr": "foo.value" } })]);
}

#[tokio::test]
async fn config_interpolation() {
    let mock = Mock::boot().await;
    mock.mock(
        "provider",
        PluginDef::new(|ctx, config| async move {
            ctx.provide_as("foo", json!({ "value": config["value"] }))?;
            Ok(())
        }),
    );
    let consumer = Counter::default();
    mock.mock("consumer", consumer.def());
    let entry =
        json!({ "id": "1", "name": "consumer", "inject": ["foo"], "config": { "value": { "__jsExpr": "foo.value" } } });

    // wait for dependencies
    mock.read(json!([entry])).await;
    let handle = mock.loader.resolve("1").unwrap();
    assert_eq!(handle.outer_fiber().unwrap().state(), FiberState::Pending);
    assert!(handle.inner_fiber().is_none());
    assert_eq!(consumer.take().0, 0);

    // evaluate after dependencies are ready
    mock.read(json!([entry, { "id": "2", "name": "provider", "config": { "value": 1 } }])).await;
    assert_eq!(mock.fiber("1").state(), FiberState::Active);
    assert_eq!(*consumer.configs.lock(), vec![json!({ "value": 1 })]);

    // re-evaluate when dependencies reload
    mock.loader.update("2", json!({ "config": { "value": 2 } }), None, None).await.unwrap();
    mock.loader.settled().await;
    assert_eq!(*consumer.configs.lock(), vec![json!({ "value": 1 }), json!({ "value": 2 })]);

    // keep the source config unevaluated
    assert_eq!(mock.fiber("1").config(), json!({ "value": { "__jsExpr": "foo.value" } }));
}

#[tokio::test]
async fn persists_an_update_to_a_pending_fiber() {
    let mock = Mock::boot().await;
    mock.mock("foo", noop().inject(["never"]));
    mock.read(json!([{ "id": "1", "name": "foo" }])).await;
    assert_eq!(mock.fiber("1").state(), FiberState::Pending);
    mock.fiber("1").update(json!({ "a": 3 })).unwrap().await.unwrap();
    settle().await;
    assert_eq!(mock.data(), vec![json!({ "id": "1", "name": "foo", "config": { "a": 3 } })]);
}

/// A plugin owning a tree of its own, like `include`.
fn tree_plugin(mock: &Mock, changes: Arc<Mutex<Vec<String>>>) -> PluginDef {
    struct Changes(Arc<Mutex<Vec<String>>>);
    impl TreeBackend for Changes {
        fn commit(&self, change: &EntryChange) {
            self.0.lock().push(change.id.clone());
        }
    }
    let loader = mock.loader.clone();
    PluginDef::new(move |ctx, _| {
        let (loader, changes) = (loader.clone(), changes.clone());
        async move {
            let tree = loader.create_tree(&ctx, Arc::new(Changes(changes)));
            let root = tree.root();
            let stopping = root.clone();
            ctx.defer(
                "tree",
                cordis_core::Disposer::future(move || async move {
                    // the children are disposed before they are unregistered
                    settle().await;
                    stopping.stop();
                }),
            )?;
            root.update(vec![json!({ "id": "a", "name": "bar" })]).await;
            Ok(())
        }
    })
    .inject(["foo"])
}

#[tokio::test]
async fn keeps_children_enabled_when_the_parent_loses_a_dependency() {
    let mock = Mock::boot().await;
    let changes = Arc::new(Mutex::new(Vec::new()));
    mock.mock("bar", noop());
    mock.mock("tree", tree_plugin(&mock, changes.clone()));
    let provision = mock.root.provide_as("foo", ()).unwrap();
    mock.read(json!([{ "id": "1", "name": "tree" }])).await;
    settle().await;
    assert_eq!(mock.fiber("1").state(), FiberState::Active);
    assert_eq!(mock.loader.resolve("1:a").unwrap().inner_fiber().unwrap().state(), FiberState::Active);

    provision.dispose().await.unwrap();
    mock.loader.settled().await;
    assert_eq!(mock.fiber("1").state(), FiberState::Pending);
    assert!(changes.lock().is_empty());
}

#[tokio::test]
async fn keeps_the_entry_of_a_plugin_replaced_in_place() {
    let mock = Mock::boot().await;
    let foo = mock.mock("foo", noop());
    mock.read(json!([{ "id": "1", "name": "foo", "config": { "a": 1 } }])).await;
    let fiber = mock.fiber("1");
    let next = Counter::default();
    mock.root.registry().replace(&foo, &next.def());
    fiber.settled().await.unwrap();
    assert_eq!(mock.fiber("1"), fiber);
    assert_eq!(next.configs.lock().clone(), vec![json!({ "a": 1 })]);
    assert!(!mock.loader.resolve("1").unwrap().is_disabled());
    assert_eq!(mock.data(), vec![json!({ "id": "1", "name": "foo", "config": { "a": 1 } })]);
}

// --- group.spec.ts ---

#[tokio::test]
async fn group_basic_support() {
    let mock = Mock::boot().await;
    let foo = Counter::default();
    mock.mock("foo", foo.def());
    let group = json!({ "name": "group", "group": true, "config": [{ "name": "foo" }] });
    let outer = mock.loader.create(group.clone(), None, None).await.unwrap();
    let inner = mock.loader.create(group, Some(&outer), None).await.unwrap();
    settle().await;
    mock.fiber(&outer);
    mock.fiber(&inner);
    assert_eq!(foo.take(), (2, 0));
    assert_eq!(mock.loader.entries().len(), 4);

    mock.loader.update(&inner, json!({ "disabled": true }), None, None).await.unwrap();
    settle().await;
    assert_eq!(foo.take(), (0, 1));
    mock.loader.update(&outer, json!({ "disabled": true }), None, None).await.unwrap();
    settle().await;
    assert_eq!(foo.take(), (0, 1));
    // the outer group is still disabled
    mock.loader.update(&inner, json!({ "disabled": null }), None, None).await.unwrap();
    settle().await;
    assert_eq!(foo.take(), (0, 0));
    mock.loader.update(&outer, json!({ "disabled": null }), None, None).await.unwrap();
    settle().await;
    assert_eq!(foo.take(), (2, 0));
    assert_eq!(mock.loader.entries().len(), 4);
}

#[tokio::test]
async fn group_transfer() {
    let mock = Mock::boot().await;
    let foo = Counter::default();
    mock.mock("foo", foo.def());
    let create = |options: Value, parent: Option<String>| {
        let loader = mock.loader.clone();
        async move { loader.create(options, parent.as_deref(), None).await.unwrap() }
    };
    let id = create(json!({ "name": "foo" }), None).await;
    let alpha = create(json!({ "name": "group", "group": true, "config": [] }), None).await;
    let beta =
        create(json!({ "name": "group", "group": true, "disabled": true, "config": [] }), Some(alpha.clone())).await;
    let gamma = create(json!({ "name": "group", "group": true, "config": [] }), Some(beta.clone())).await;
    assert_eq!(foo.take(), (1, 0));
    assert_eq!(mock.loader.entries().len(), 4);

    mock.loader.update(&id, json!({}), Some(Some(&alpha)), None).await.unwrap();
    settle().await;
    assert_eq!(foo.take(), (0, 0), "enabled -> enabled");
    mock.loader.update(&id, json!({}), Some(Some(&beta)), None).await.unwrap();
    settle().await;
    assert_eq!(foo.take(), (0, 1), "enabled -> disabled");
    mock.loader.update(&id, json!({}), Some(Some(&gamma)), None).await.unwrap();
    settle().await;
    assert_eq!(foo.take(), (0, 0), "disabled -> disabled");
    mock.loader.update(&id, json!({}), Some(None), None).await.unwrap();
    settle().await;
    assert_eq!(foo.take(), (1, 0), "disabled -> enabled");
    assert_eq!(mock.loader.entries().len(), 4);
}

#[tokio::test(start_paused = true)]
async fn group_update_settles_its_children() {
    let mock = Mock::boot().await;
    let foo = mock.mock("foo", noop());
    let bar = mock.mock("bar", noop());
    let group = mock
        .loader
        .create(json!({ "name": "group", "group": true, "config": [{ "id": "1", "name": "foo" }] }), None, None)
        .await
        .unwrap();
    settle().await;
    assert!(mock.enabled(&foo) && !mock.enabled(&bar));
    // a slow resolution makes the awaited reconciliation observable
    mock.delay.store(20, Ordering::SeqCst);
    let config = json!({ "config": [{ "id": "1", "name": "foo" }, { "id": "2", "name": "bar" }] });
    mock.loader.update(&group, config, None, None).await.unwrap();
    assert!(mock.enabled(&bar));
    mock.fiber("2");
}

#[tokio::test]
async fn group_intercepts_nest() {
    let mock = Mock::boot().await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    mock.mock(
        "foo",
        PluginDef::new(move |ctx, _| {
            s.lock().push(ctx.intercepts("foo"));
            async { Ok(()) }
        }),
    );
    let group = |meta: Value| json!({ "name": "group", "group": true, "intercept": { "foo": meta }, "config": [] });
    let outer = mock.loader.create(group(json!({ "a": 1 })), None, None).await.unwrap();
    let inner = mock.loader.create(group(json!({ "b": 2 })), Some(&outer), None).await.unwrap();
    mock.loader.create(json!({ "name": "foo", "intercept": { "foo": { "c": 3 } } }), Some(&inner), None).await.unwrap();
    settle().await;
    assert_eq!(seen.lock().clone(), vec![vec![json!({ "a": 1 }), json!({ "b": 2 }), json!({ "c": 3 })]]);
}

#[tokio::test]
async fn intercept_changes_apply_without_restart() {
    let mock = Mock::boot().await;
    let foo = Counter::default();
    mock.mock("foo", foo.def());
    let id = mock.loader.create(json!({ "name": "foo", "intercept": { "x": { "a": 1 } } }), None, None).await.unwrap();
    settle().await;
    let fiber = mock.fiber(&id);
    assert_eq!(fiber.context().intercepts("x"), vec![json!({ "a": 1 })]);
    mock.loader.update(&id, json!({ "intercept": { "x": { "a": 2 } } }), None, None).await.unwrap();
    assert_eq!(fiber.context().intercepts("x"), vec![json!({ "a": 2 })]);
    assert_eq!(foo.take(), (1, 0));
}

// --- commit.spec.ts ---

#[tokio::test]
async fn commit_reports_changes() {
    let mock = Mock::boot().await;
    mock.mock("foo", noop());
    let id = mock.loader.create(json!({ "name": "foo", "config": { "a": 1 } }), None, None).await.unwrap();
    let group = mock.loader.create(json!({ "name": "group", "group": true, "config": [] }), None, None).await.unwrap();
    let changes = mock.take_changes();
    assert_eq!(ids(&changes), vec![id.clone(), group.clone()]);
    assert_eq!(changes[0].group, mock.loader.root());
    assert!(changes[0].legacy.is_none());
    assert_eq!(changes[0].options, Some(json!({ "id": id, "name": "foo", "config": { "a": 1 } })));
    assert!(mock.data().contains(changes[0].options.as_ref().unwrap()));

    // update reports the new options together with the previous ones
    mock.loader.update(&id, json!({ "config": { "a": 2 } }), None, None).await.unwrap();
    let changes = mock.take_changes();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].legacy, Some(json!({ "id": id, "name": "foo", "config": { "a": 1 } })));
    assert_eq!(changes[0].options, Some(json!({ "id": id, "name": "foo", "config": { "a": 2 } })));
    assert_eq!(changes[0].options.as_ref(), Some(&mock.loader.resolve(&id).unwrap().options()));

    // move reports the target group
    mock.loader.update(&id, json!({}), Some(Some(&group)), None).await.unwrap();
    let changes = mock.take_changes();
    assert_eq!(changes.len(), 1);
    assert_eq!(Some(changes[0].group.clone()), mock.loader.resolve(&group).unwrap().subgroup());
    assert_eq!(changes[0].from, Some(mock.loader.root()));
    assert_eq!(changes[0].options, changes[0].legacy);
    // the group's own options mirror its children
    assert_eq!(mock.data()[0]["config"][0]["id"], json!(id));

    // update in place does not report a move
    mock.loader.update(&id, json!({ "config": { "a": 4 } }), None, None).await.unwrap();
    assert!(mock.take_changes()[0].from.is_none());

    // fiber.update from inside the plugin reports a config change
    mock.fiber(&id).update(json!({ "a": 5 })).unwrap().await.unwrap();
    let changes = mock.take_changes();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].legacy.as_ref().unwrap()["config"], json!({ "a": 4 }));
    assert_eq!(changes[0].options.as_ref().unwrap()["config"], json!({ "a": 5 }));

    // remove reports the removed entry
    mock.loader.remove(&id).unwrap();
    let changes = mock.take_changes();
    assert_eq!(ids(&changes), vec![id.clone()]);
    assert!(changes[0].options.is_none());
    assert_eq!(changes[0].legacy.as_ref().unwrap()["name"], json!("foo"));
    assert!(mock.loader.resolve(&id).is_err());
}

#[tokio::test]
async fn does_not_mistake_a_child_fiber_disposing_itself_for_the_entry() {
    let mock = Mock::boot().await;
    mock.mock(
        "parent",
        PluginDef::new(|ctx, _| async move {
            let child = ctx.plugin_def(&PluginDef::new(|_, _| async { Ok(()) }), Value::Null)?;
            drop(child.dispose());
            Ok(())
        }),
    );
    let parent = mock.loader.create(json!({ "name": "parent" }), None, None).await.unwrap();
    settle().await;
    mock.fiber(&parent);
    assert!(mock.loader.resolve(&parent).unwrap().options().get("disabled").is_none());
    assert!(
        mock.take_changes().iter().all(|change| change.options.as_ref().is_none_or(|o| o.get("disabled").is_none()))
    );
}

#[tokio::test]
async fn does_not_report_reads() {
    let mock = Mock::boot().await;
    mock.mock("foo", noop());
    let group = json!({ "id": "g", "name": "group", "group": true, "config": [] });
    mock.read(json!([{ "id": "x", "name": "foo" }, group])).await;
    mock.take_changes();
    mock.read(json!([group])).await;
    settle().await;
    assert!(mock.take_changes().is_empty());
    assert!(mock.loader.resolve("x").is_err());
}

#[tokio::test]
async fn keeps_an_entry_that_another_group_adopted() {
    let mock = Mock::boot().await;
    let foo = Counter::default();
    let def = mock.mock("foo", foo.def());
    mock.read(json!([{ "id": "x", "name": "foo" }, { "id": "g", "name": "group", "group": true, "config": [] }])).await;
    assert_eq!(foo.take().0, 1);
    mock.read(json!([{ "id": "g", "name": "group", "group": true, "config": [{ "id": "x", "name": "foo" }] }])).await;
    settle().await;
    let entry = mock.loader.resolve("x").unwrap();
    assert_eq!(Some(entry.parent()), mock.loader.resolve("g").unwrap().subgroup());
    mock.fiber("x");
    assert!(mock.enabled(&def));
}

// --- validate.spec.ts (runtime part) ---

#[tokio::test]
async fn reject_invalid_options_at_runtime_before_they_are_committed() {
    let mock = Mock::boot().await;
    mock.mock("foo", noop());
    let error = mock.loader.create(json!({ "name": 1 }), None, None).await.unwrap_err();
    assert!(error.is::<EntryValidationError>());
    assert_eq!(error.to_string(), "invalid entry options:\n  - name must be a string (at name)");
    assert!(mock.take_changes().is_empty() && mock.data().is_empty());

    let id = mock.loader.create(json!({ "name": "foo" }), None, None).await.unwrap();
    mock.take_changes();
    let error = mock.loader.update(&id, json!({ "disabled": "yes" }), None, None).await.unwrap_err();
    assert!(error.is::<EntryValidationError>());
    assert!(mock.take_changes().is_empty());
    assert_eq!(mock.loader.resolve(&id).unwrap().options(), json!({ "id": id, "name": "foo" }));
    mock.loader.settled().await;
    assert_eq!(mock.fiber(&id).state(), FiberState::Active);
}

#[tokio::test]
async fn check_the_entries_of_a_group_passed_at_runtime_as_a_whole() {
    let mock = Mock::boot().await;
    mock.mock("foo", noop());
    let config = json!([null, { "id": "child", "name": "foo" }, { "name": "foo", "inject": [1] }]);
    let error = mock.loader.create(json!({ "name": "group", "group": true, "config": config }), None, None).await;
    assert_eq!(
        error.unwrap_err().to_string(),
        [
            "invalid entry options:",
            "  - entry must be an object (at config.0)",
            "  - inject.0 must be a string (at config.2.inject.0)",
        ]
        .join("\n")
    );
    assert!(mock.take_changes().is_empty());
    assert!(mock.loader.resolve("child").is_err());

    let id = mock.loader.create(json!({ "name": "group", "group": true, "config": [] }), None, None).await.unwrap();
    mock.take_changes();
    let error = mock.loader.update(&id, json!({ "config": [{ "id": "child" }] }), None, None).await.unwrap_err();
    assert!(error.to_string().contains("name is required (at config.0.name)"));
    assert!(mock.take_changes().is_empty());
    assert_eq!(mock.loader.resolve(&id).unwrap().options()["config"], json!([]));

    // a source hands its entries over one at a time
    assert!(validate_options(&json!({ "name": "group", "group": true, "config": [null] }), false).is_empty());
}

#[tokio::test]
async fn unload_an_entry_whose_options_turn_invalid_and_reload_it_once_fixed() {
    let mock = Mock::boot().await;
    let foo = Counter::default();
    mock.mock("foo", foo.def());
    mock.read(json!([{ "id": "a", "name": "foo" }, { "id": "b", "name": "foo" }])).await;
    let fiber = mock.fiber("a");
    mock.take_changes();

    mock.read(json!([{ "id": "a", "name": "foo", "group": "no" }, { "id": "b", "name": "foo" }, "oops"])).await;
    assert!(mock.loader.resolve("a").is_err());
    assert_eq!(fiber.state(), FiberState::Disposed);
    assert_eq!(mock.fiber("b").state(), FiberState::Active);
    // the disposal is not mistaken for a self-dispose, which writes `disabled`
    assert!(mock.take_changes().is_empty());

    mock.read(json!([{ "id": "a", "name": "foo" }, { "id": "b", "name": "foo" }])).await;
    assert_eq!(mock.fiber("a").state(), FiberState::Active);
    assert_eq!(foo.take().0, 3);
}
