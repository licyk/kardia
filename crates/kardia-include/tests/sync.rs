//! Port of `packages/include/tests/sync.spec.ts`.

mod common;

use std::path::PathBuf;

use common::{App, group, plugin, read, write_fixture};
use kardia_core::Level;
use kardia_include::yaml;
use kardia_loader::EntryHandle;
use serde_json::{Value, json};

fn dump(entries: Vec<Value>) -> String {
    yaml::dump(&Value::Array(entries))
}

fn no_errors(app: &App) {
    assert_eq!(app.logs(Level::Error), Vec::<String>::new());
}

#[tokio::test(flavor = "multi_thread")]
async fn coalesces_consecutive_writes_into_the_latest_state() {
    let app = App::setup("sync-coalesce.yml", vec![plugin("a", 1)], json!({})).await;
    for n in [2, 3, 4] {
        app.update("a", json!({ "config": { "tag": "a", "value": n } })).await;
    }
    app.settle().await;
    assert_eq!(app.value("a"), Some(4));
    assert_eq!(app.read()[0]["config"]["value"], json!(4));
    no_errors(&app);
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_file_and_tree_consistent_when_a_refresh_lands_inside_a_write() {
    let app = App::setup("sync-window.yml", vec![plugin("a", 1)], json!({})).await;
    let include = app.include();
    tokio::join!(app.update("a", json!({ "config": { "tag": "a", "value": 2 } })), include.refresh());
    app.settle().await;
    assert_eq!(app.value("a"), Some(2));
    assert_eq!(app.read()[0]["config"]["value"], json!(2));
}

#[tokio::test(flavor = "multi_thread")]
async fn does_not_write_back_when_the_file_removes_or_disables_an_entry() {
    let app =
        App::setup("sync-remove.yml", format!("# keep me\n{}", dump(vec![plugin("a", 1), plugin("b", 1)])), json!({}))
            .await;
    assert!(app.config("b").is_some());
    let after = format!("# keep me\n{}", dump(vec![plugin("a", 1)]));
    app.write(after.clone());
    app.settle().await;
    assert!(app.config("b").is_none());
    assert_eq!(app.text(), after);

    let mut disabled = plugin("a", 1);
    disabled["disabled"] = json!(true);
    let after = format!("# keep me\n{}", dump(vec![disabled]));
    app.write(after.clone());
    app.settle().await;
    assert!(app.config("a").is_none());
    assert_eq!(app.text(), after);
    no_errors(&app);
}

/// Also the races of a watcher-driven file edit with a runtime edit: whichever
/// side gets to the include first, both edits survive.
#[tokio::test(flavor = "multi_thread")]
async fn merges_a_file_edit_and_a_runtime_edit_on_different_entries() {
    for order in ["unwatched", "refresh first", "update first"] {
        let app = App::setup(
            &format!("sync-merge-{}.yml", order.replace(' ', "-")),
            vec![plugin("a", 1), plugin("b", 1)],
            json!({}),
        )
        .await;
        // the file changes under us
        app.write(vec![plugin("a", 1), plugin("b", 2)]);
        let include = app.include();
        let update = app.update("a", json!({ "config": { "tag": "a", "value": 2 } }));
        match order {
            "refresh first" => drop(tokio::join!(include.refresh(), update)),
            "update first" => drop(tokio::join!(update, include.refresh())),
            _ => update.await,
        }
        app.settle().await;
        assert_eq!((app.value("a"), app.value("b")), (Some(2), Some(2)), "{order}");
        let data = app.read();
        assert_eq!(
            (data[0]["config"]["value"].clone(), data[1]["config"]["value"].clone()),
            (json!(2), json!(2)),
            "{order}"
        );
        assert!(app.logs(Level::Error).is_empty(), "{order}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn lets_the_file_win_a_conflict_on_the_same_entry_and_reports_it() {
    let app = App::setup("sync-conflict.yml", vec![plugin("a", 1)], json!({})).await;
    app.write(vec![plugin("a", 3)]);
    app.update("a", json!({ "config": { "tag": "a", "value": 2 } })).await;
    app.settle().await;
    assert_eq!(app.value("a"), Some(3));
    assert_eq!(app.read()[0]["config"]["value"], json!(3));
    let errors = app.logs(Level::Error);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("conflict") && errors[0].contains("config"), "{errors:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn combines_a_move_in_the_file_with_an_edit_at_runtime() {
    let app = App::setup("sync-move.yml", vec![plugin("a", 1), group("g", vec![])], json!({})).await;
    app.write(vec![group("g", vec![plugin("a", 1)])]);
    app.update("a", json!({ "config": { "tag": "a", "value": 2 } })).await;
    app.settle().await;
    let tree = app.tree();
    assert_eq!(Some(tree.resolve("a").unwrap().parent()), tree.resolve("g").unwrap().subgroup());
    assert_eq!(app.value("a"), Some(2));
    assert_eq!(app.read(), vec![group("g", vec![plugin("a", 2)])]);
    no_errors(&app);
}

#[tokio::test(flavor = "multi_thread")]
async fn never_writes_over_a_file_it_cannot_parse() {
    let app = App::setup("sync-broken.yml", vec![plugin("a", 1)], json!({})).await;
    app.write("- {\n");
    app.update("a", json!({ "config": { "tag": "a", "value": 2 } })).await;
    app.include().refresh().await;
    assert!(app.logs(Level::Warn).iter().any(|line| line.contains("failed to parse")));
    assert_eq!(app.text(), "- {\n");
    assert_eq!(app.value("a"), Some(2));

    // once the file is valid again the pending change goes out with the merge
    app.write(vec![plugin("a", 1), plugin("b", 1)]);
    app.settle().await;
    let data = app.read();
    assert_eq!((data[0]["config"]["value"].clone(), data[1]["config"]["value"].clone()), (json!(2), json!(1)));
    assert_eq!(app.value("b"), Some(1));
}

/// An outer include whose entry `inc` includes `<name>-inner.yml` with `patches`.
async fn nested(name: &str, inner: Vec<Value>, patches: Value) -> (App, PathBuf, String) {
    let inner_text = dump(inner);
    let inner = write_fixture(&format!("{name}-inner.yml"), inner_text.clone());
    let entry = json!({ "id": "inc", "name": "include", "config": { "path": format!("./{name}-inner.yml"), "patches": patches } });
    let app = App::setup(&format!("{name}-outer.yml"), vec![entry], json!({})).await;
    (app, inner, inner_text)
}

/// Re-reads the inner include, then the outer one.
async fn settle_nested(app: &App) {
    app.nested("inc").refresh().await;
    app.settle().await;
}

fn patches(app: &App) -> Value {
    app.read()[0]["config"]["patches"].clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn routes_changes_to_patch_owned_keys_into_the_parent_config() {
    let (app, inner, inner_text) =
        nested("sync-patch", vec![plugin("a", 1)], json!([{ "id": "a", "disabled": true }])).await;
    app.nested("inc").refresh().await;
    assert!(app.config("a").is_none());

    app.update("inc:a", json!({ "disabled": false })).await;
    settle_nested(&app).await;
    assert_eq!(app.value("a"), Some(1));
    assert_eq!(std::fs::read_to_string(&inner).unwrap(), inner_text);
    assert_eq!(patches(&app), json!([{ "id": "a", "disabled": false }]));
    no_errors(&app);
}

#[tokio::test(flavor = "multi_thread")]
async fn routes_changes_to_inserted_entries_into_their_patch() {
    let (app, inner, inner_text) =
        nested("sync-insert", vec![plugin("a", 1)], json!([{ "insert": [plugin("x", 1)] }])).await;
    app.nested("inc").refresh().await;
    app.loader.settled().await;
    assert_eq!(app.value("x"), Some(1));

    app.update("inc:x", json!({ "config": { "tag": "x", "value": 2 } })).await;
    settle_nested(&app).await;
    assert_eq!(app.value("x"), Some(2));
    assert_eq!(std::fs::read_to_string(&inner).unwrap(), inner_text);
    assert_eq!(patches(&app), json!([{ "insert": [plugin("x", 2)] }]));
}

// --- entries inserted and overridden by patches ---

fn x(value: i64) -> Value {
    json!({ "tag": "x", "value": value })
}

fn enabled(mut options: Value) -> Value {
    options["disabled"] = json!(false);
    options
}

async fn persists_each_key_to_its_effective_owner(name: &str, patches_in: Value, expected: Value, initial: i64) {
    let (app, inner, inner_text) = nested(&format!("sync-owner-{name}"), vec![plugin("a", 1)], patches_in).await;
    assert_eq!(app.value("x"), Some(initial));

    app.update("inc:x", json!({ "config": x(3), "disabled": false })).await;
    settle_nested(&app).await;

    assert_eq!(app.value("x"), Some(3));
    assert_eq!(patches(&app), expected);
    assert_eq!(std::fs::read_to_string(&inner).unwrap(), inner_text);
    no_errors(&app);
}

#[tokio::test(flavor = "multi_thread")]
async fn persists_each_key_to_its_effective_owner_last_override() {
    persists_each_key_to_its_effective_owner(
        "last",
        json!([{ "insert": [plugin("x", 1)] }, { "id": "x", "config": x(2) }]),
        json!([{ "insert": [enabled(plugin("x", 1))] }, { "id": "x", "config": x(3) }]),
        2,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn persists_each_key_to_its_effective_owner_multiple_overrides() {
    persists_each_key_to_its_effective_owner(
        "multiple",
        json!([{ "insert": [plugin("x", 1)] }, { "id": "x", "config": x(0) }, { "id": "x", "config": x(2) }]),
        json!([{ "insert": [enabled(plugin("x", 1))] }, { "id": "x", "config": x(0) }, { "id": "x", "config": x(3) }]),
        2,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn persists_each_key_to_its_effective_owner_insertion_without_overrides() {
    persists_each_key_to_its_effective_owner(
        "insertion",
        json!([{ "insert": [plugin("x", 1)] }]),
        json!([{ "insert": [enabled(plugin("x", 3))] }]),
        1,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn persists_each_key_to_its_effective_owner_skipped_override_before_insertion() {
    persists_each_key_to_its_effective_owner(
        "skipped",
        json!([{ "id": "x", "config": x(2) }, { "insert": [plugin("x", 1)] }]),
        json!([{ "id": "x", "config": x(2) }, { "insert": [enabled(plugin("x", 3))] }]),
        1,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn persists_each_key_to_its_effective_owner_skipped_name_mismatch() {
    persists_each_key_to_its_effective_owner(
        "mismatch",
        json!([
            { "insert": [plugin("x", 1)] },
            { "id": "x", "config": x(2) },
            { "id": "x", "name": "wrong-plugin", "config": x(9) },
        ]),
        json!([
            { "insert": [enabled(plugin("x", 1))] },
            { "id": "x", "config": x(3) },
            { "id": "x", "name": "wrong-plugin", "config": x(9) },
        ]),
        2,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn routes_past_a_name_mismatched_override() {
    // `apply_patches` skips this patch, so it owns nothing and the change is the file's
    let skipped = json!([{ "id": "x", "name": "wrong-plugin", "config": x(2) }]);
    let (app, inner, _) = nested("sync-file-owner", vec![plugin("x", 1)], skipped.clone()).await;
    assert_eq!(app.value("x"), Some(1));

    app.update("inc:x", json!({ "config": x(3) })).await;
    settle_nested(&app).await;

    assert_eq!(app.value("x"), Some(3));
    assert_eq!(patches(&app), skipped);
    assert_eq!(read(&inner), vec![plugin("x", 3)]);
    no_errors(&app);
}

// --- entries of a group config replaced by a patch ---

async fn replaced_group(name: &str, config: Vec<Value>) -> (App, PathBuf, String) {
    let (app, inner, inner_text) = nested(
        &format!("sync-{name}"),
        vec![group("g", vec![plugin("hidden", 1)])],
        json!([{ "id": "g", "config": config }]),
    )
    .await;
    app.nested("inc").refresh().await;
    app.loader.settled().await;
    assert_eq!(app.value("a"), Some(1));
    assert!(app.config("hidden").is_none());
    (app, inner, inner_text)
}

#[tokio::test(flavor = "multi_thread")]
async fn routes_changes_into_the_patch() {
    let (app, inner, inner_text) = replaced_group("group-update", vec![plugin("a", 1)]).await;
    app.update("inc:a", json!({ "config": { "tag": "a", "value": 2 } })).await;
    settle_nested(&app).await;

    assert_eq!(app.value("a"), Some(2));
    assert_eq!(std::fs::read_to_string(&inner).unwrap(), inner_text);
    assert_eq!(patches(&app), json!([{ "id": "g", "config": [plugin("a", 2)] }]));
}

#[tokio::test(flavor = "multi_thread")]
async fn places_new_entries_into_the_patch() {
    let (app, inner, inner_text) = replaced_group("group-create", vec![plugin("a", 1)]).await;
    let parent = format!("{}:inc:g", app.id);
    let created = json!({ "name": "config-plugin", "config": { "tag": "b", "value": 1 } });
    app.loader.create(created, Some(&parent), None).await.unwrap();
    settle_nested(&app).await;

    assert_eq!(app.value("b"), Some(1));
    assert_eq!(std::fs::read_to_string(&inner).unwrap(), inner_text);
    let patches = patches(&app);
    let config = patches[0]["config"].as_array().unwrap();
    assert_eq!(patches[0]["id"], json!("g"));
    assert_eq!(config.len(), 2);
    assert_eq!(config[0], plugin("a", 1));
    assert_eq!(config[1]["config"], json!({ "tag": "b", "value": 1 }));
}

#[tokio::test(flavor = "multi_thread")]
async fn removes_entries_from_the_patch() {
    let (app, inner, inner_text) = replaced_group("group-remove", vec![plugin("a", 1)]).await;
    app.loader.remove(&format!("{}:inc:a", app.id)).unwrap();
    settle_nested(&app).await;

    assert!(app.config("a").is_none());
    assert_eq!(std::fs::read_to_string(&inner).unwrap(), inner_text);
    assert_eq!(patches(&app), json!([{ "id": "g", "config": [] }]));
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_anonymous_entries_across_file_edits() {
    let anonymous = json!({ "name": "config-plugin", "config": { "tag": "a", "value": 1 } });
    let (app, inner, inner_text) = replaced_group("group-anon", vec![anonymous]).await;
    let find = |app: &App| {
        let tree = app.nested("inc").tree().clone();
        let entry = tree.entries().into_iter().find(|entry| entry.options()["config"]["tag"] == json!("a")).unwrap();
        (entry.clone(), entry.inner_fiber())
    };
    let before = find(&app);

    std::fs::write(&inner, format!("# touched\n{inner_text}")).unwrap();
    settle_nested(&app).await;
    assert_eq!(find(&app), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn routes_an_edit_to_an_entry_whose_group_moved_in_the_same_batch() {
    let (app, inner, _) =
        nested("sync-batch-move", vec![group("h", vec![])], json!([{ "insert": [group("g", vec![plugin("x", 1)])] }]))
            .await;
    app.nested("inc").refresh().await;
    app.loader.settled().await;

    // both changes are reported before either is written
    let (g, h) = (format!("{}:inc:g", app.id), format!("{}:inc:h", app.id));
    let (moved, _) = tokio::join!(
        app.loader.update(&g, json!({}), Some(Some(&h)), None),
        app.update("inc:x", json!({ "config": x(2) })),
    );
    moved.unwrap();
    app.nested("inc").flush().await;
    app.settle().await;

    assert_eq!(app.value("x"), Some(2));
    no_errors(&app);
    assert_eq!(read(&inner), vec![group("h", vec![group("g", vec![plugin("x", 2)])])]);
    assert_eq!(patches(&app), json!([{ "insert": [] }]));
}

// --- anonymous entries ---

fn tagged(app: &App, tag: &str) -> Vec<EntryHandle> {
    app.tree().entries().into_iter().filter(|entry| entry.options()["config"]["tag"] == json!(tag)).collect()
}

fn local_id(entry: &EntryHandle) -> String {
    entry.options()["id"].as_str().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_anonymous_entries_stable_across_reloads_without_writing() {
    let anonymous = json!({ "name": "config-plugin", "config": { "tag": "anon", "value": 1 } });
    let text = dump(vec![anonymous.clone(), plugin("b", 1)]);
    let app = App::setup("sync-anonymous.yml", text.clone(), json!({})).await;
    assert_eq!(app.value("anon"), Some(1));
    assert_eq!(app.text(), text);
    let applies = app.applies("anon");

    let edited = dump(vec![anonymous, plugin("b", 2)]);
    app.write(edited.clone());
    app.settle().await;

    assert_eq!(app.value("b"), Some(2));
    assert_eq!(app.applies("anon"), applies);
    assert_eq!(app.text(), edited);

    // the assigned id travels with the next write
    app.update("b", json!({ "config": { "tag": "b", "value": 3 } })).await;
    app.settle().await;
    let first = &app.read()[0];
    let id = first["id"].as_str().unwrap();
    assert!(id.len() == 8 && id.chars().all(|c| c.is_ascii_hexdigit()), "{id}");
    assert_eq!(first["config"]["tag"], json!("anon"));
}

#[tokio::test(flavor = "multi_thread")]
async fn does_not_give_a_root_anonymous_plugin_the_nested_plugins_id() {
    let shape = json!({ "name": "config-plugin", "config": { "tag": "x", "value": 1 } });
    let anonymous_group = json!({ "name": "group", "group": true, "config": [shape.clone()] });
    let app = App::setup("sync-anon-root.yml", dump(vec![anonymous_group, shape.clone()]), json!({})).await;

    let root_group = app.tree().root();
    let entries = tagged(&app, "x");
    let root = entries.iter().find(|entry| entry.parent() == root_group).unwrap().clone();
    let nested = entries.iter().find(|entry| entry.parent() != root_group).unwrap().clone();
    assert_ne!(root, nested);
    let (root_id, nested_id) = (local_id(&root), local_id(&nested));
    let group_entry = app.tree().entries().into_iter().find(|entry| entry.options()["group"] == json!(true)).unwrap();
    assert_eq!(Some(nested.parent()), group_entry.subgroup());
    let applies = app.applies("x");

    app.write(dump(vec![shape]));
    app.settle().await;

    let surviving = tagged(&app, "x");
    assert_eq!(surviving, vec![root]);
    assert_eq!(local_id(&surviving[0]), root_id);
    assert_ne!(local_id(&surviving[0]), nested_id);
    assert_eq!(app.applies("x"), applies);
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_an_anonymous_group_and_its_child_stable_when_a_neighbor_changes() {
    let child = json!({ "name": "config-plugin", "config": { "tag": "child", "value": 1 } });
    let anonymous_group = json!({ "name": "group", "group": true, "config": [child] });
    let app =
        App::setup("sync-anon-neighbor.yml", dump(vec![anonymous_group.clone(), plugin("b", 1)]), json!({})).await;

    let tree = app.tree();
    let group_entry = tree.entries().into_iter().find(|entry| entry.options()["group"] == json!(true)).unwrap();
    let child_entry = tagged(&app, "child").remove(0);
    assert_eq!(group_entry.parent(), tree.root());
    assert_eq!(Some(child_entry.parent()), group_entry.subgroup());
    let (group_id, child_id) = (local_id(&group_entry), local_id(&child_entry));
    let applies = app.applies("child");

    app.write(dump(vec![anonymous_group, plugin("b", 2)]));
    app.settle().await;

    assert_eq!(app.value("b"), Some(2));
    assert_eq!(tree.resolve(&group_id).unwrap(), group_entry);
    assert_eq!(tree.resolve(&child_id).unwrap(), child_entry);
    assert_eq!(local_id(&group_entry), group_id);
    assert_eq!(local_id(&child_entry), child_id);
    assert_eq!(Some(child_entry.parent()), group_entry.subgroup());
    assert_eq!(app.applies("child"), applies);
}

#[tokio::test(flavor = "multi_thread")]
async fn does_not_reuse_an_id_that_the_file_now_writes_explicitly() {
    let anonymous = json!({ "name": "config-plugin", "config": { "tag": "anon", "value": 1 } });
    let app = App::setup("sync-explicit-id.yml", dump(vec![anonymous.clone()]), json!({})).await;
    let id = local_id(&app.tree().entries()[0]);
    assert!(!app.text().contains(&id));

    app.write(dump(vec![
        anonymous,
        json!({ "id": id, "name": "config-plugin", "config": { "tag": "other", "value": 1 } }),
    ]));
    app.settle().await;

    let tree = app.tree();
    assert_eq!(tree.resolve(&id).unwrap().options()["config"], json!({ "tag": "other", "value": 1 }));
    let anon = tagged(&app, "anon").remove(0);
    assert_ne!(local_id(&anon), id);
    assert_eq!(tree.resolve(&local_id(&anon)).unwrap(), anon);
}

#[tokio::test(flavor = "multi_thread")]
async fn does_not_reuse_an_id_owned_by_an_explicit_patch_insert() {
    let anonymous = json!({ "name": "config-plugin", "config": { "tag": "anon", "value": 1 } });
    let app = App::setup("sync-patch-insert.yml", dump(vec![anonymous.clone()]), json!({})).await;
    let id = local_id(&app.tree().entries()[0]);
    assert!(!app.text().contains(&id));

    let insert = json!({ "id": id, "name": "config-plugin", "config": { "tag": "other", "value": 9 } });
    let config = json!({ "path": app.filename.to_string_lossy(), "patches": [{ "insert": [insert] }] });
    app.loader.update(&app.id, json!({ "config": config }), None, None).await.unwrap();
    app.loader.settled().await;

    // a byte change forces a reread; the assigned id stays out of the file
    app.write(format!("# reread\n{}", dump(vec![anonymous])));
    app.settle().await;

    let tree = app.tree();
    assert_eq!(tree.resolve(&id).unwrap().options()["config"], json!({ "tag": "other", "value": 9 }));
    let anon = tagged(&app, "anon").remove(0);
    assert_ne!(local_id(&anon), id);
    assert_eq!(tree.resolve(&local_id(&anon)).unwrap(), anon);
}

// --- read-only files and disposal ---

fn set_readonly(app: &App, readonly: bool) {
    let mut permissions = std::fs::metadata(&app.filename).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(readonly);
    std::fs::set_permissions(&app.filename, permissions).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_runtime_changes_in_memory_when_the_file_is_read_only() {
    let app = App::setup("sync-readonly.yml", vec![plugin("a", 1), plugin("b", 1)], json!({})).await;
    let text = app.text();
    set_readonly(&app, true);

    app.update("a", json!({ "config": { "tag": "a", "value": 2 } })).await;
    app.settle().await;

    assert_eq!(app.value("a"), Some(2));
    assert_eq!(app.text(), text);
    assert!(app.logs(Level::Warn).iter().any(|line| line.contains("read-only")));

    // an unrelated file edit keeps the in-memory overlay
    set_readonly(&app, false);
    app.write(vec![plugin("a", 1), plugin("b", 2)]);
    set_readonly(&app, true);
    app.settle().await;

    assert_eq!(app.value("a"), Some(2));
    assert_eq!(app.value("b"), Some(2));
    assert_eq!(app.read()[0]["config"]["value"], json!(1));
    set_readonly(&app, false);
}

#[tokio::test(flavor = "multi_thread")]
async fn removes_a_nested_entry_through_its_composite_id() {
    let app = App::setup("sync-nested-remove.yml", vec![plugin("a", 1), plugin("b", 1)], json!({})).await;

    // the public id is `<include>:<local>`; the tree's store is keyed by the local id
    app.loader.remove(&format!("{}:a", app.id)).unwrap();
    app.settle().await;

    assert!(app.config("a").is_none());
    assert_eq!(app.value("b"), Some(1));
    assert!(app.tree().resolve("a").is_err());
    assert_eq!(app.read().iter().map(|entry| entry["id"].clone()).collect::<Vec<_>>(), vec![json!("b")]);
}

#[tokio::test(flavor = "multi_thread")]
async fn removes_an_entry_of_a_group_through_its_composite_id() {
    let app =
        App::setup("sync-group-remove.yml", vec![group("g", vec![plugin("a", 1)]), plugin("b", 1)], json!({})).await;
    app.loader.remove(&format!("{}:a", app.id)).unwrap();
    app.settle().await;
    assert!(app.config("a").is_none());
    assert_eq!(app.read(), vec![group("g", vec![]), plugin("b", 1)]);
}

#[tokio::test(flavor = "multi_thread")]
async fn retries_a_write_that_failed_earlier_when_disposing() {
    let app = App::setup("sync-dispose-retry.yml", vec![plugin("a", 1)], json!({})).await;
    let text = app.text();
    set_readonly(&app, true);

    app.update("a", json!({ "config": { "tag": "a", "value": 2 } })).await;
    app.settle().await;
    assert_eq!(app.text(), text);

    // the file becomes writable again, but nothing else touches the tree
    set_readonly(&app, false);
    app.dispose().await;

    assert_eq!(app.read()[0]["config"]["value"], json!(2));
}

#[tokio::test(flavor = "multi_thread")]
async fn flushes_pending_changes_on_dispose() {
    let app = App::setup("sync-dispose.yml", vec![plugin("a", 1)], json!({})).await;

    let id = format!("{}:a", app.id);
    let update = app.loader.update(&id, json!({ "config": { "tag": "a", "value": 2 } }), None, None);
    let _ = tokio::join!(update, app.dispose());

    assert_eq!(app.read()[0]["config"]["value"], json!(2));
}

#[tokio::test(flavor = "multi_thread")]
async fn plugin_self_updates_and_self_disposals_are_written_back() {
    let app = App::setup("sync-self.yml", vec![plugin("a", 1), plugin("b", 1)], json!({})).await;
    let tree = app.tree();
    let a = tree.resolve("a").unwrap().inner_fiber().unwrap();
    a.update(json!({ "tag": "a", "value": 7 })).unwrap().await.unwrap();
    tree.resolve("b").unwrap().inner_fiber().unwrap().dispose().await.unwrap();
    app.settle().await;
    let data = app.read();
    assert_eq!(data[0]["config"]["value"], json!(7));
    assert_eq!(data[1]["disabled"], json!(true));
}

#[tokio::test(flavor = "multi_thread")]
async fn moves_an_entry_into_another_include_as_a_single_instance() {
    let app = App::setup("sync-move-source.yml", vec![plugin("m", 1)], json!({})).await;
    let target = write_fixture("sync-move-target.yml", vec![group("g", vec![])]);
    let config = json!({ "name": "include", "config": { "path": target.to_string_lossy() } });
    let target_id = app.loader.create(config, None, None).await.unwrap();
    app.loader.settled().await;
    let target_include =
        || kardia_include::Include::of(&app.loader.resolve(&target_id).unwrap().subtree().unwrap()).unwrap();
    assert_eq!(app.running(), vec!["m"]);

    let (from, to) = (format!("{}:m", app.id), format!("{target_id}:g"));
    app.loader.update(&from, json!({}), Some(Some(&to)), None).await.unwrap();
    app.settle().await;
    target_include().refresh().await;
    app.loader.settled().await;

    assert_eq!(app.read(), Vec::<Value>::new());
    assert_eq!(read(&target), vec![group("g", vec![plugin("m", 1)])]);
    assert_eq!(app.running(), vec!["m"]);
    let moved = app.loader.resolve(&format!("{target_id}:m")).unwrap();
    assert_eq!(moved.options()["config"], json!({ "tag": "m", "value": 1 }));
    assert!(app.loader.resolve(&from).is_err());

    app.loader.remove(&format!("{target_id}:m")).unwrap();
    target_include().flush().await;
    app.loader.settled().await;

    assert_eq!(app.running(), Vec::<String>::new());
    assert_eq!(read(&target), vec![group("g", vec![])]);
}
