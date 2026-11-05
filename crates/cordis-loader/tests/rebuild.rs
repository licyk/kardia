//! Port of `packages/loader/tests/rebuild.spec.ts` and the cross-tree parts
//! of `commit.spec.ts`.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::{Counter, Gate, Mock, settle};
use cordis_core::{Disposer, FiberState, PluginDef};
use cordis_loader::{EntryChange, Resolver, TreeBackend, group_plugin};
use futures::FutureExt;
use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::{Value, json};

fn noop() -> PluginDef {
    PluginDef::new(|_, _| async { Ok(()) })
}

/// `a`, whose disposal can be held; `b` and `c` count their loads.
struct Fixture {
    mock: Mock,
    a: Counter,
    b: Counter,
    c: Counter,
    hold_a: Arc<Mutex<Option<(Gate, Gate)>>>,
}

async fn fixture() -> Fixture {
    let mock = Mock::boot().await;
    mock.mock(
        "settings",
        PluginDef::new(|ctx, _| async move {
            ctx.provide_as("settings", ())?;
            Ok(())
        }),
    );
    let (a, b, c) = (Counter::default(), Counter::default(), Counter::default());
    let hold_a: Arc<Mutex<Option<(Gate, Gate)>>> = Default::default();
    let (counter, hold) = (a.clone(), hold_a.clone());
    mock.mock(
        "a",
        PluginDef::new(move |ctx, _| {
            counter.calls.fetch_add(1, Ordering::SeqCst);
            let hold = hold.clone();
            async move {
                ctx.defer(
                    "a",
                    Disposer::future(move || async move {
                        let gates = hold.lock().clone();
                        if let Some((entered, release)) = gates {
                            entered.open();
                            release.wait().await;
                        }
                    }),
                )?;
                Ok(())
            }
        }),
    );
    mock.mock("b", b.def());
    mock.mock("c", c.def());
    Fixture { mock, a, b, c, hold_a }
}

fn entries(name: &str, inject: Option<&[&str]>) -> Vec<Value> {
    let mut entry = json!({ "id": "x", "name": name });
    if let Some(inject) = inject {
        entry["inject"] = json!(inject);
    }
    vec![json!({ "id": "settings", "name": "settings" }), entry]
}

#[tokio::test]
async fn rebuilds_an_active_entry_when_its_name_changes() {
    for inject in [None, Some(&["settings"][..])] {
        let fx = fixture().await;
        fx.mock.read(Value::Array(entries("a", inject))).await;
        let previous = fx.mock.fiber("x");
        fx.mock.take_changes();
        fx.mock.loader.root().update(entries("b", inject)).await;
        fx.mock.loader.settled().await;
        assert!(previous.is_removed());
        assert_eq!(fx.mock.fiber("x").state(), FiberState::Active);
        assert_eq!((fx.a.take().0, fx.b.take().0), (1, 1));
        assert!(fx.mock.loader.resolve("x").unwrap().options().get("disabled").is_none());
        assert!(fx.mock.take_changes().iter().all(|c| c.options.as_ref().is_none_or(|o| o.get("disabled").is_none())));
    }
}

#[tokio::test]
async fn uses_the_new_name_when_it_changes_during_the_import() {
    let fx = fixture().await;
    // the old name fails to resolve, as when the rename fixes a wrong name
    let (entered, release) = fx.mock.hold("a", true);
    let root = fx.mock.loader.root();
    let creating = tokio::spawn({
        let root = root.clone();
        async move { root.update(vec![json!({ "id": "x", "name": "a" })]).await }
    });
    entered.wait().await;
    let updating = tokio::spawn({
        let root = root.clone();
        async move { root.update(vec![json!({ "id": "x", "name": "b" })]).await }
    });
    release.open();
    let _ = tokio::join!(creating, updating);
    fx.mock.loader.settled().await;
    assert_eq!((fx.a.take().0, fx.b.take().0), (0, 1));
}

#[tokio::test]
async fn uses_the_new_name_when_it_changes_during_the_patch() {
    let fx = fixture().await;
    let (entered, release, _hook) = fx.mock.hold_patch(false, |options| options["name"] == "a");
    let root = fx.mock.loader.root();
    let creating = tokio::spawn({
        let root = root.clone();
        async move { root.update(vec![json!({ "id": "x", "name": "a" })]).await }
    });
    entered.wait().await;
    let updating = tokio::spawn({
        let root = root.clone();
        async move { root.update(vec![json!({ "id": "x", "name": "b" })]).await }
    });
    release.open();
    let _ = tokio::join!(creating, updating);
    fx.mock.loader.settled().await;
    assert_eq!((fx.a.take().0, fx.b.take().0), (0, 1));
}

#[tokio::test]
async fn joins_a_second_rename_while_the_first_disposal_is_pending() {
    for inject in [None, Some(&["settings"][..])] {
        let fx = fixture().await;
        let (entered, held) = (Gate::new(), Gate::new());
        fx.mock.read(Value::Array(entries("a", inject))).await;
        *fx.hold_a.lock() = Some((entered.clone(), held.clone()));
        let root = fx.mock.loader.root();
        let first = tokio::spawn({
            let root = root.clone();
            let config = entries("b", inject);
            async move { root.update(config).await }
        });
        entered.wait().await;
        let second = tokio::spawn({
            let root = root.clone();
            let config = entries("c", inject);
            async move { root.update(config).await }
        });
        held.open();
        let _ = tokio::join!(first, second);
        fx.mock.loader.settled().await;
        assert_eq!((fx.b.take().0, fx.c.take().0), (0, 1));
        assert_eq!(fx.mock.fiber("x").state(), FiberState::Active);
    }
}

#[tokio::test]
async fn applies_nothing_when_the_entry_is_removed_during_the_disposal() {
    let fx = fixture().await;
    let (entered, held) = (Gate::new(), Gate::new());
    fx.mock.read(Value::Array(entries("a", Some(&["settings"])))).await;
    let entry = fx.mock.loader.resolve("x").unwrap();
    *fx.hold_a.lock() = Some((entered.clone(), held.clone()));
    let root = fx.mock.loader.root();
    let renaming = tokio::spawn(async move { root.update(entries("b", Some(&["settings"]))).await });
    entered.wait().await;
    fx.mock.loader.remove("x").unwrap();
    held.open();
    renaming.await.unwrap();
    fx.mock.loader.settled().await;
    settle().await;
    assert_eq!(fx.b.take().0, 0);
    assert!(entry.outer_fiber().is_none());
    assert!(entry.inner_fiber().is_none_or(|fiber| fiber.is_removed()));
}

#[tokio::test]
async fn rebuilds_when_the_plugin_renames_its_own_entry() {
    let fx = fixture().await;
    let held = Gate::new();
    let renames = Arc::new(AtomicUsize::new(0));
    let (loader, h, r) = (fx.mock.loader.clone(), held.clone(), renames.clone());
    fx.mock.mock(
        "renaming",
        PluginDef::new(move |ctx, _| {
            r.fetch_add(1, Ordering::SeqCst);
            let loader = loader.clone();
            tokio::spawn(async move { loader.update("x", json!({ "name": "b" }), None, None).await });
            let held = h.clone();
            async move {
                ctx.defer("held", Disposer::future(move || async move { held.wait().await }))?;
                Ok(())
            }
        }),
    );
    let root = fx.mock.loader.root();
    tokio::spawn(async move { root.update(vec![json!({ "id": "x", "name": "renaming" })]).await });
    settle().await;
    // the rebuild is a task until the old instance is disposed
    assert!(fx.mock.loader.is_busy());
    assert_eq!(fx.b.take().0, 0);
    held.open();
    fx.mock.loader.settled().await;
    assert_eq!(renames.load(Ordering::SeqCst), 1);
    assert_eq!(fx.b.take().0, 1);
    assert_eq!(fx.mock.fiber("x").state(), FiberState::Active);
}

// --- #218 ---

#[tokio::test]
async fn starts_a_pending_entry_when_inject_is_cleared() {
    let mock = Mock::boot().await;
    let plug = Counter::default();
    mock.mock("plug", plug.def());
    let id = mock.loader.create(json!({ "name": "plug", "inject": ["never"] }), None, None).await.unwrap();
    settle().await;
    let entry = mock.loader.resolve(&id).unwrap();
    let old = entry.outer_fiber().unwrap();
    assert_eq!(old.state(), FiberState::Pending);
    assert!(entry.inner_fiber().is_none());
    mock.take_changes();
    mock.loader.update(&id, json!({ "inject": null }), None, None).await.unwrap();
    settle().await;
    let next = mock.fiber(&id);
    assert_eq!(next.state(), FiberState::Active);
    assert_eq!(old.state(), FiberState::Disposed);
    assert_eq!(plug.take().0, 1);
    let options = entry.options();
    assert!(options.get("inject").is_none() && options.get("disabled").is_none());
    assert!(mock.take_changes().iter().all(|c| c.options.as_ref().is_none_or(|o| o.get("disabled").is_none())));
}

#[tokio::test]
async fn reapplies_when_inject_gains_a_service_that_is_already_provided() {
    let mock = Mock::boot().await;
    mock.root.provide_as("foo", 1i32).unwrap();
    let reads = Arc::new(Mutex::new(Vec::new()));
    let r = reads.clone();
    mock.mock(
        "plug",
        PluginDef::new(move |ctx, _| {
            r.lock().push(ctx.get_as::<i32>("foo").ok().map(|v| *v));
            async { Ok(()) }
        }),
    );
    let id = mock.loader.create(json!({ "name": "plug" }), None, None).await.unwrap();
    settle().await;
    let old = mock.fiber(&id);
    mock.loader.update(&id, json!({ "inject": ["foo"] }), None, None).await.unwrap();
    settle().await;
    assert_eq!(reads.lock().len(), 2);
    assert_eq!(reads.lock()[1], Some(1));
    assert_eq!(old.state(), FiberState::Disposed);
    assert_ne!(mock.fiber(&id), old);
}

#[tokio::test]
async fn keeps_the_new_outer_fiber_pending_and_drops_the_old_listener_when_inject_is_missing() {
    struct Ping;
    impl cordis_core::Event for Ping {
        type Output = ();
    }
    let mock = Mock::boot().await;
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    mock.mock(
        "plug",
        PluginDef::new(move |ctx, _| {
            let hits = h.clone();
            async move {
                ctx.on(move |_: &Ping| {
                    hits.fetch_add(1, Ordering::SeqCst);
                })?;
                Ok(())
            }
        }),
    );
    let id = mock.loader.create(json!({ "name": "plug" }), None, None).await.unwrap();
    settle().await;
    let old = mock.fiber(&id);
    mock.root.emit(&Ping);
    mock.loader.update(&id, json!({ "inject": ["foo"] }), None, None).await.unwrap();
    settle().await;
    assert_eq!(old.state(), FiberState::Disposed);
    let entry = mock.loader.resolve(&id).unwrap();
    assert_eq!(entry.outer_fiber().unwrap().state(), FiberState::Pending);
    mock.root.emit(&Ping);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

fn gated_plug(mock: &Mock, gate: &Gate, applies: &Arc<AtomicUsize>) -> PluginDef {
    let (gate, applies) = (gate.clone(), applies.clone());
    mock.mock(
        "plug",
        PluginDef::new(move |ctx, _| {
            applies.fetch_add(1, Ordering::SeqCst);
            let gate = gate.clone();
            async move {
                ctx.defer("gated", Disposer::future(move || async move { gate.wait().await }))?;
                Ok(())
            }
        }),
    )
}

#[tokio::test]
async fn joins_a_second_inject_update_while_the_first_rebuild_is_disposing() {
    let mock = Mock::boot().await;
    mock.root.provide_as("foo", 1i32).unwrap();
    mock.root.provide_as("bar", 2i32).unwrap();
    let (gate, applies) = (Gate::new(), Arc::new(AtomicUsize::new(0)));
    let plug = gated_plug(&mock, &gate, &applies);
    let id = mock.loader.create(json!({ "name": "plug" }), None, None).await.unwrap();
    settle().await;
    let (l1, l2) = (mock.loader.clone(), mock.loader.clone());
    let (i1, i2) = (id.clone(), id.clone());
    let first = tokio::spawn(async move { l1.update(&i1, json!({ "inject": ["foo"] }), None, None).await });
    let second = tokio::spawn(async move { l2.update(&i2, json!({ "inject": ["bar"] }), None, None).await });
    settle().await;
    assert!(mock.loader.is_busy());
    assert_eq!(applies.load(Ordering::SeqCst), 1);
    gate.open();
    let _ = tokio::join!(first, second);
    mock.loader.settled().await;
    assert_eq!(applies.load(Ordering::SeqCst), 2);
    assert_eq!(mock.loader.resolve(&id).unwrap().options()["inject"], json!(["bar"]));
    let active = mock.root.registry().fibers(&plug).iter().filter(|f| f.state() == FiberState::Active).count();
    assert_eq!(active, 1);
}

#[tokio::test]
async fn uses_the_latest_name_during_a_pending_import() {
    for fail in [false, true] {
        let mock = Mock::boot().await;
        mock.mock("a", noop());
        let (b, c) = (Counter::default(), Counter::default());
        mock.mock("b", b.def());
        let c_def = mock.mock("c", c.def());
        mock.read(json!([{ "id": "x", "name": "a" }])).await;
        let (entered, release) = mock.hold("b", fail);
        let root = mock.loader.root();
        let first = tokio::spawn({
            let root = root.clone();
            async move { root.update(vec![json!({ "id": "x", "name": "b" })]).await }
        });
        entered.wait().await;
        let second = tokio::spawn({
            let root = root.clone();
            async move { root.update(vec![json!({ "id": "x", "name": "c" })]).await }
        });
        settle().await;
        assert!(mock.loader.is_busy());
        release.open();
        let _ = tokio::join!(first, second);
        mock.loader.settled().await;
        let entry = mock.loader.resolve("x").unwrap();
        assert_eq!(entry.options()["name"], json!("c"));
        assert_eq!((b.take().0, c.take().0), (0, 1), "fail: {fail}");
        assert_eq!(entry.inner_fiber().unwrap().runtime(), Some(c_def.key()));
    }
}

#[tokio::test]
async fn uses_the_latest_name_during_a_context_patch() {
    let mock = Mock::boot().await;
    mock.mock("a", noop());
    let (b, c) = (Counter::default(), Counter::default());
    mock.mock("b", b.def());
    let c_def = mock.mock("c", c.def());
    mock.read(json!([{ "id": "x", "name": "a" }])).await;
    let (entered, release, _hook) = mock.hold_patch(true, |options| options["name"] == "b");
    let root = mock.loader.root();
    let first = tokio::spawn({
        let root = root.clone();
        async move { root.update(vec![json!({ "id": "x", "name": "b" })]).await }
    });
    entered.wait().await;
    let second = tokio::spawn({
        let root = root.clone();
        async move { root.update(vec![json!({ "id": "x", "name": "c" })]).await }
    });
    settle().await;
    assert!(mock.loader.is_busy());
    release.open();
    let _ = tokio::join!(first, second);
    mock.loader.settled().await;
    let entry = mock.loader.resolve("x").unwrap();
    assert_eq!(entry.options()["name"], json!("c"));
    assert_eq!((b.take().0, c.take().0), (0, 1));
    assert_eq!(entry.inner_fiber().unwrap().runtime(), Some(c_def.key()));
    assert!(entry.options().get("disabled").is_none());
}

#[tokio::test]
async fn does_not_apply_a_fiber_removed_while_rebuild_is_disposing() {
    let mock = Mock::boot().await;
    mock.root.provide_as("foo", 1i32).unwrap();
    let (gate, applies) = (Gate::new(), Arc::new(AtomicUsize::new(0)));
    let plug = gated_plug(&mock, &gate, &applies);
    let id = mock.loader.create(json!({ "name": "plug" }), None, None).await.unwrap();
    settle().await;
    let (loader, i) = (mock.loader.clone(), id.clone());
    let pending = tokio::spawn(async move { loader.update(&i, json!({ "inject": ["foo"] }), None, None).await });
    settle().await;
    mock.loader.remove(&id).unwrap();
    gate.open();
    let _ = pending.await;
    settle().await;
    assert_eq!(applies.load(Ordering::SeqCst), 1);
    assert!(mock.loader.resolve(&id).is_err());
    assert!(!mock.root.registry().has(&plug));
}

#[tokio::test]
async fn keeps_a_rebuild_started_during_apply_until_its_disposer_settles() {
    let mock = Mock::boot().await;
    for (key, n) in [("foo", 1i32), ("bar", 2), ("qux", 3)] {
        mock.root.provide_as(key, n).unwrap();
    }
    let (gate, applies, id) = (Gate::new(), Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(String::new())));
    let (loader, g, a, i) = (mock.loader.clone(), gate.clone(), applies.clone(), id.clone());
    let plug = mock.mock(
        "plug",
        PluginDef::new(move |ctx, _| {
            let n = a.fetch_add(1, Ordering::SeqCst) + 1;
            if n == 2 {
                let (loader, id) = (loader.clone(), i.lock().clone());
                tokio::spawn(async move { loader.update(&id, json!({ "inject": ["bar"] }), None, None).await });
            }
            let gate = g.clone();
            async move {
                ctx.defer(
                    "gated",
                    Disposer::future(move || async move {
                        if n == 2 {
                            gate.wait().await;
                        }
                    }),
                )?;
                Ok(())
            }
        }),
    );
    *id.lock() = mock.loader.create(json!({ "name": "plug" }), None, None).await.unwrap();
    let id = id.lock().clone();
    settle().await;
    mock.loader.update(&id, json!({ "inject": ["foo"] }), None, None).await.unwrap();
    settle().await;
    assert!(mock.loader.is_busy());
    let loader = mock.loader.clone();
    let waiting = tokio::spawn(async move { loader.settled().await });
    settle().await;
    assert!(!waiting.is_finished());
    assert_eq!(applies.load(Ordering::SeqCst), 2);

    let (loader, i) = (mock.loader.clone(), id.clone());
    let third = tokio::spawn(async move { loader.update(&i, json!({ "inject": ["qux"] }), None, None).await });
    settle().await;
    assert_eq!(applies.load(Ordering::SeqCst), 2);

    gate.open();
    third.await.unwrap().unwrap();
    waiting.await.unwrap();
    settle().await;
    assert_eq!(applies.load(Ordering::SeqCst), 3);
    let options = mock.loader.resolve(&id).unwrap().options();
    assert_eq!(options["inject"], json!(["qux"]));
    let active = mock.root.registry().fibers(&plug).iter().filter(|f| f.state() == FiberState::Active).count();
    assert_eq!(active, 1);
    assert!(options.get("disabled").is_none());
}

// --- commit.spec.ts: cross-tree moves ---

#[derive(Default)]
struct Recorded(Mutex<Vec<EntryChange>>);

struct TreeBackendImpl(Arc<Recorded>);

impl TreeBackend for TreeBackendImpl {
    fn commit(&self, change: &EntryChange) {
        self.0.0.lock().push(change.clone());
    }

    fn resolver(&self) -> Option<Arc<dyn Resolver>> {
        struct Groups;
        impl Resolver for Groups {
            fn resolve(&self, _: &str) -> BoxFuture<'static, cordis_core::Result<PluginDef>> {
                futures::future::ready(Ok(group_plugin())).boxed()
            }
        }
        Some(Arc::new(Groups))
    }
}

async fn tree_fixture() -> (Mock, Arc<Recorded>, String, PluginDef) {
    let mock = Mock::boot().await;
    let plugin = mock.mock("foo", noop());
    let recorded = Arc::new(Recorded::default());
    let (loader, r) = (mock.loader.clone(), recorded.clone());
    mock.mock(
        "tree",
        PluginDef::new(move |ctx, _| {
            let (loader, recorded) = (loader.clone(), r.clone());
            async move {
                let tree = loader.create_tree(&ctx, Arc::new(TreeBackendImpl(recorded)));
                tree.root().update(vec![json!({ "id": "g", "name": "group", "group": true, "config": [] })]).await;
                Ok(())
            }
        })
        .inject(["loader"]),
    );
    let id = mock.loader.create(json!({ "name": "tree" }), None, None).await.unwrap();
    mock.loader.settled().await;
    mock.take_changes();
    (mock, recorded, id, plugin)
}

fn running(mock: &Mock, plugin: &PluginDef) -> usize {
    mock.root.registry().fibers(plugin).len()
}

#[tokio::test]
async fn reports_a_removal_to_the_source_tree_and_a_creation_to_the_target_tree() {
    let (mock, recorded, tree, _) = tree_fixture().await;
    let id = mock.loader.create(json!({ "name": "foo", "config": { "a": 1 } }), None, None).await.unwrap();
    mock.loader.settled().await;
    mock.take_changes();
    let target = format!("{tree}:g");
    mock.loader.update(&id, json!({ "config": { "a": 2 } }), Some(Some(&target)), None).await.unwrap();
    let changes = mock.take_changes();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].group, mock.loader.root());
    assert!(changes[0].options.is_none());
    assert_eq!(changes[0].legacy, Some(json!({ "id": id, "name": "foo", "config": { "a": 1 } })));
    let changes = recorded.0.lock().clone();
    assert_eq!(changes.len(), 1);
    assert_eq!(Some(changes[0].group.clone()), mock.loader.resolve(&target).unwrap().subgroup());
    assert!(changes[0].legacy.is_none() && changes[0].from.is_none());
    assert_eq!(changes[0].options, Some(json!({ "id": id, "name": "foo", "config": { "a": 2 } })));
}

#[tokio::test]
async fn transfers_entries_and_group_members_to_the_target_tree() {
    let (mock, _, tree, foo) = tree_fixture().await;
    let id = mock.loader.create(json!({ "name": "foo" }), None, None).await.unwrap();
    mock.loader.settled().await;
    let target = format!("{tree}:g");
    mock.loader.update(&id, json!({}), Some(Some(&target)), None).await.unwrap();
    mock.loader.settled().await;
    assert!(mock.loader.resolve(&id).is_err());
    let entry = mock.loader.resolve(&format!("{tree}:{id}")).unwrap();
    assert_eq!(entry.id(), format!("{tree}:{id}"));
    assert_eq!(running(&mock, &foo), 1);
    // the target tree adopts the entry when it reconciles its source
    let options = entry.options();
    mock.loader.resolve(&target).unwrap().subgroup().unwrap().update(vec![options]).await;
    mock.loader.settled().await;
    assert_eq!(mock.loader.resolve(&format!("{tree}:{id}")).unwrap(), entry);
    assert_eq!(running(&mock, &foo), 1);
    mock.loader.remove(&format!("{tree}:{id}")).unwrap();
    mock.loader.settled().await;
    assert_eq!(running(&mock, &foo), 0);

    let group = mock
        .loader
        .create(json!({ "name": "group", "group": true, "config": [{ "id": "member", "name": "foo" }] }), None, None)
        .await
        .unwrap();
    mock.loader.settled().await;
    let member = mock.loader.resolve("member").unwrap();
    mock.loader.update(&group, json!({}), Some(Some(&target)), None).await.unwrap();
    mock.loader.settled().await;
    assert!(mock.loader.resolve(&group).is_err() && mock.loader.resolve("member").is_err());
    assert_eq!(mock.loader.resolve(&format!("{tree}:member")).unwrap(), member);
    assert_eq!(member.id(), format!("{tree}:member"));
    assert_eq!(running(&mock, &foo), 1);
    mock.loader.remove(&format!("{tree}:{group}")).unwrap();
    mock.loader.settled().await;
    assert_eq!(running(&mock, &foo), 0);
}

#[tokio::test]
async fn rejects_a_move_onto_an_id_that_the_target_tree_already_uses() {
    let (mock, recorded, tree, _) = tree_fixture().await;
    let id = mock.loader.create(json!({ "id": "g", "name": "foo" }), None, None).await.unwrap();
    mock.loader.settled().await;
    let entry = mock.loader.resolve(&id).unwrap();
    mock.take_changes();
    let error = mock.loader.update(&id, json!({}), Some(Some(&format!("{tree}:g"))), None).await.unwrap_err();
    assert!(error.to_string().contains("already exists"));
    assert_eq!(mock.loader.resolve(&id).unwrap(), entry);
    assert_eq!(entry.parent(), mock.loader.root());
    assert!(mock.data().contains(&entry.options()));
    assert!(mock.take_changes().is_empty() && recorded.0.lock().is_empty());
}

#[tokio::test]
async fn does_not_persist_child_disposal_while_the_tree_loses_its_loader() {
    let mock = Mock::boot().await;
    mock.mock("child", noop());
    let recorded = Arc::new(Recorded::default());
    let trees = Arc::new(Mutex::new(None));
    let (loader, r, t) = (mock.loader.clone(), recorded.clone(), trees.clone());
    let tree_def = PluginDef::new(move |ctx, _| {
        let (loader, recorded, trees) = (loader.clone(), r.clone(), t.clone());
        async move {
            let tree = loader.create_tree(&ctx, Arc::new(Committed(recorded)));
            tree.root().update(vec![json!({ "id": "child", "name": "child" })]).await;
            let root = tree.root();
            *trees.lock() = Some(tree);
            ctx.defer(
                "tree",
                Disposer::future(move || async move {
                    // include flushes pending writes before stopping its entry group
                    tokio::task::yield_now().await;
                    root.stop();
                }),
            )?;
            Ok(())
        }
    })
    .inject(["loader"]);
    let fiber = mock.loader.context().plugin_def(&tree_def, Value::Null).unwrap();
    fiber.settled().await.unwrap();
    mock.loader.settled().await;
    let tree = trees.lock().clone().unwrap();
    assert_eq!(tree.root().data().len(), 1);
    mock.loader.context().fiber().dispose().await.unwrap();
    settle().await;
    assert!(tree.root().data()[0].get("disabled").is_none());
    assert!(recorded.0.lock().is_empty());
}

struct Committed(Arc<Recorded>);

impl TreeBackend for Committed {
    fn commit(&self, change: &EntryChange) {
        self.0.0.lock().push(change.clone());
    }
}
