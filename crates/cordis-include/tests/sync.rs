//! Port of `packages/include/tests/{sync,reload,parse}.spec.ts`.

mod common;

use common::{App, group, plugin};
use cordis_core::Level;
use serde_json::{Value, json};

fn value(app: &App, tag: &str) -> Option<i64> {
    app.config(tag)?["value"].as_i64()
}

#[tokio::test(flavor = "multi_thread")]
async fn coalesces_consecutive_writes_into_the_latest_state() {
    let app = App::setup("sync-coalesce.yml", vec![plugin("a", 1)], json!({})).await;
    for n in [2, 3, 4] {
        app.update("a", json!({ "config": { "tag": "a", "value": n } })).await;
    }
    app.settle().await;
    assert_eq!(value(&app, "a"), Some(4));
    assert_eq!(app.read()[0]["config"]["value"], json!(4));
    assert!(app.logs(Level::Error).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_file_and_tree_consistent_when_a_refresh_lands_inside_a_write() {
    let app = App::setup("sync-window.yml", vec![plugin("a", 1)], json!({})).await;
    let include = app.include();
    tokio::join!(app.update("a", json!({ "config": { "tag": "a", "value": 2 } })), include.refresh());
    app.settle().await;
    assert_eq!(value(&app, "a"), Some(2));
    assert_eq!(app.read()[0]["config"]["value"], json!(2));
}

#[tokio::test(flavor = "multi_thread")]
async fn does_not_write_back_when_the_file_removes_or_disables_an_entry() {
    let dump = |entries: Vec<Value>| cordis_include::yaml::dump(&Value::Array(entries));
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
    assert!(app.logs(Level::Error).is_empty());
}

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
        assert_eq!((value(&app, "a"), value(&app, "b")), (Some(2), Some(2)), "{order}");
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
    assert_eq!(value(&app, "a"), Some(3));
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
    let tree = app.loader.resolve(&app.id).unwrap().subtree().unwrap();
    assert_eq!(Some(tree.resolve("a").unwrap().parent()), tree.resolve("g").unwrap().subgroup());
    assert_eq!(value(&app, "a"), Some(2));
    assert_eq!(app.read(), vec![group("g", vec![plugin("a", 2)])]);
    assert!(app.logs(Level::Error).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn never_writes_over_a_file_it_cannot_parse() {
    let app = App::setup("sync-broken.yml", vec![plugin("a", 1)], json!({})).await;
    app.write("- {\n");
    app.update("a", json!({ "config": { "tag": "a", "value": 2 } })).await;
    app.include().refresh().await;
    assert!(app.logs(Level::Warn).iter().any(|line| line.contains("failed to parse")));
    assert_eq!(app.text(), "- {\n");
    assert_eq!(value(&app, "a"), Some(2));

    // once the file is valid again the pending change goes out with the merge
    app.write(vec![plugin("a", 1), plugin("b", 1)]);
    app.settle().await;
    let data = app.read();
    assert_eq!((data[0]["config"]["value"].clone(), data[1]["config"]["value"].clone()), (json!(2), json!(1)));
    assert_eq!(value(&app, "b"), Some(1));
}

async fn nested(name: &str, patches: Value) -> (App, std::path::PathBuf, String) {
    let inner = common::fixture(&format!("{name}-inner.yml"));
    let inner_text = cordis_include::yaml::dump(&Value::Array(vec![plugin("a", 1)]));
    std::fs::write(&inner, &inner_text).unwrap();
    let entry = json!({ "id": "inc", "name": "include", "config": { "path": format!("./{name}-inner.yml"), "patches": patches } });
    let app = App::setup(&format!("{name}-outer.yml"), vec![entry], json!({})).await;
    app.loader.settled().await;
    (app, inner, inner_text)
}

fn inner_include(app: &App) -> cordis_include::Include {
    let tree = app.loader.resolve(&format!("{}:inc", app.id)).unwrap().subtree().unwrap();
    cordis_include::Include::of(&tree).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn routes_changes_to_patch_owned_keys_into_the_parent_config() {
    let (app, inner, inner_text) = nested("sync-patch", json!([{ "id": "a", "disabled": true }])).await;
    inner_include(&app).refresh().await;
    assert!(app.config("a").is_none());

    app.update("inc:a", json!({ "disabled": false })).await;
    inner_include(&app).refresh().await;
    app.settle().await;
    app.loader.settled().await;
    assert_eq!(value(&app, "a"), Some(1));
    assert_eq!(std::fs::read_to_string(&inner).unwrap(), inner_text);
    assert_eq!(app.read()[0]["config"]["patches"], json!([{ "id": "a", "disabled": false }]));
    assert!(app.logs(Level::Error).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn routes_changes_to_inserted_entries_into_their_patch() {
    let (app, inner, inner_text) = nested("sync-insert", json!([{ "insert": [plugin("x", 1)] }])).await;
    inner_include(&app).refresh().await;
    app.loader.settled().await;
    assert_eq!(value(&app, "x"), Some(1));

    app.update("inc:x", json!({ "config": { "tag": "x", "value": 2 } })).await;
    inner_include(&app).refresh().await;
    app.settle().await;
    app.loader.settled().await;
    assert_eq!(value(&app, "x"), Some(2));
    assert_eq!(std::fs::read_to_string(&inner).unwrap(), inner_text);
    assert_eq!(app.read()[0]["config"]["patches"], json!([{ "insert": [plugin("x", 2)] }]));
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_anonymous_entries_stable_across_reloads_without_writing() {
    let text = "- name: config-plugin\n  config:\n    tag: a\n    value: 1\n- name: config-plugin\n  config:\n    tag: b\n    value: 1\n";
    let app = App::setup("sync-anonymous.yml", text, json!({})).await;
    let fiber = |app: &App| {
        let tree = app.loader.resolve(&app.id).unwrap().subtree().unwrap();
        tree.entries().into_iter().find(|e| e.options()["config"]["tag"] == json!("a")).unwrap().inner_fiber().unwrap()
    };
    let before = fiber(&app);
    app.write(text.replace("tag: b\n    value: 1", "tag: b\n    value: 2"));
    app.settle().await;
    assert_eq!(fiber(&app), before, "the unchanged neighbor keeps its id");
    assert_eq!(value(&app, "b"), Some(2));
    assert!(!app.text().contains("id:"), "assigned ids are not written");
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_runtime_changes_in_memory_when_the_file_is_read_only() {
    let app = App::setup("sync-readonly.yml", vec![plugin("a", 1)], json!({})).await;
    let before = app.text();
    let mut permissions = std::fs::metadata(&app.filename).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&app.filename, permissions.clone()).unwrap();
    app.update("a", json!({ "config": { "tag": "a", "value": 2 } })).await;
    app.settle().await;
    assert_eq!(value(&app, "a"), Some(2));
    assert_eq!(app.text(), before);
    assert!(app.logs(Level::Warn).iter().any(|line| line.contains("read-only")));
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    std::fs::set_permissions(&app.filename, permissions).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn removes_a_nested_entry_through_its_composite_id_and_flushes_on_dispose() {
    let app =
        App::setup("sync-remove-nested.yml", vec![group("g", vec![plugin("a", 1)]), plugin("b", 1)], json!({})).await;
    app.loader.remove(&format!("{}:a", app.id)).unwrap();
    app.settle().await;
    assert!(app.config("a").is_none());
    assert_eq!(app.read(), vec![group("g", vec![]), plugin("b", 1)]);

    app.update("b", json!({ "config": { "tag": "b", "value": 5 } })).await;
    app.root.fiber().dispose().await.unwrap();
    assert_eq!(app.read()[1]["config"]["value"], json!(5));
}

#[tokio::test(flavor = "multi_thread")]
async fn plugin_self_updates_and_self_disposals_are_written_back() {
    let app = App::setup("sync-self.yml", vec![plugin("a", 1), plugin("b", 1)], json!({})).await;
    let tree = app.loader.resolve(&app.id).unwrap().subtree().unwrap();
    let a = tree.resolve("a").unwrap().inner_fiber().unwrap();
    a.update(json!({ "tag": "a", "value": 7 })).unwrap().await.unwrap();
    tree.resolve("b").unwrap().inner_fiber().unwrap().dispose().await.unwrap();
    app.settle().await;
    let data = app.read();
    assert_eq!(data[0]["config"]["value"], json!(7));
    assert_eq!(data[1]["disabled"], json!(true));
}

// --- reload.spec.ts ---

#[tokio::test(flavor = "multi_thread")]
async fn re_applies_patches_on_every_reload_and_reverts_a_removed_patch() {
    let patches = json!({ "patches": [{ "id": "a", "disabled": true }] });
    let app = App::setup("reload-patches.yml", vec![plugin("a", 1)], patches).await;
    assert!(app.config("a").is_none());
    app.write(vec![plugin("a", 1), plugin("x", 1)]);
    app.include().refresh().await;
    app.wait_for(|app| app.config("x").is_some()).await;
    assert!(app.config("a").is_none());

    // patches apply to a copy of the parsed file, so dropping one reverts the entry
    let path = app.filename.to_string_lossy().to_string();
    app.loader.update(&app.id, json!({ "config": { "path": path } }), None, None).await.unwrap();
    app.wait_for(|app| app.config("a").is_some()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuilds_an_entry_whose_name_changes_in_the_file() {
    let app = App::setup("reload-rename.yml", vec![plugin("a", 1)], json!({})).await;
    assert!(app.config("a").is_some());
    let renamed = "- id: a\n  name: group\n  group: true\n  config: []\n";
    app.write(renamed);
    app.settle().await;
    assert!(app.config("a").is_none());
    // the old plugin is not taken for a self-dispose
    assert_eq!(app.text(), renamed);
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_the_tree_reloadable_after_an_invalid_file() {
    let app = App::setup("reload-invalid.yml", vec![plugin("a", 1)], json!({})).await;
    app.write("a: 1\n");
    app.include().refresh().await;
    assert!(app.logs(Level::Warn).iter().any(|line| line.contains("failed to validate config file")));
    assert!(app.config("a").is_some());
    app.write(vec![plugin("a", 1), plugin("x", 1)]);
    app.settle().await;
    assert!(app.config("x").is_some() && app.config("a").is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_initial_only_for_a_missing_file() {
    let missing = common::fixture("reload-initial-missing.yml");
    let _ = std::fs::remove_file(&missing);
    let app = App::boot(
        missing.clone(),
        json!({ "initial": [{ "name": "config-plugin", "config": { "tag": "i", "value": 1 } }] }),
    )
    .await;
    assert!(app.config("i").is_some());
    assert!(app.read()[0]["id"].is_string());

    let broken = "- {\n";
    let app = App::setup("reload-initial-broken.yml", broken, json!({ "initial": [plugin("x", 1)] })).await;
    assert_eq!(app.text(), broken);
    assert!(app.config("x").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn reconfigures_a_running_app_when_the_watched_file_changes() {
    let app = App::setup("watch.yml", vec![plugin("a", 1)], json!({ "watch": true })).await;
    assert_eq!(value(&app, "a"), Some(1));
    // the OS may deliver the first events late, so the file is written until it is seen
    for attempt in 2..200 {
        app.write(vec![plugin("a", attempt)]);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if value(&app, "a").is_some_and(|value| value > 1) {
            return;
        }
    }
    panic!("the watched edit was not applied");
}

// --- parse.spec.ts ---

#[tokio::test(flavor = "multi_thread")]
async fn resolves_scalars_by_the_core_schema() {
    let text = "%YAML 1.1\n---\n- id: a\n  name: config-plugin\n  config:\n    tag: a\n    value: [yes, 0777, 0b101]\n    <<: { merged: true }\n";
    let app = App::setup("parse-scalars.yml", text, json!({})).await;
    let config = app.config("a").unwrap();
    assert_eq!(config["value"], json!(["yes", 777, "0b101"]));
    assert_eq!(config["<<"], json!({ "merged": true }));
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_expressions_back_with_their_tag() {
    let text = "- id: a\n  name: config-plugin\n  config:\n    tag: a\n    value: !!js \"'#' + 1\"\n";
    let app = App::setup("parse-expr.yml", text, json!({})).await;
    assert_eq!(app.config("a").unwrap()["value"], json!("#1"));
    app.update("a", json!({ "config": { "tag": "a", "value": { "__jsExpr": "'#' + 2" } } })).await;
    app.include().flush().await;
    assert!(app.text().contains("    value: !!js '''#'' + 2'\n"), "{}", app.text());
    assert_eq!(app.read()[0]["config"]["value"], json!({ "__jsExpr": "'#' + 2" }));
    // force a re-read of what was written
    app.write(app.text() + "# touched\n");
    app.settle().await;
    assert_eq!(app.config("a").unwrap()["value"], json!("#2"));
}
