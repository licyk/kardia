//! Port of `packages/include/tests/reload.spec.ts`.

mod common;

use common::{App, plugin};
use kardia_core::Level;
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn re_applies_patches_on_every_reload() {
    let patches = json!({ "patches": [{ "id": "a", "disabled": true }] });
    let app = App::setup("reload-patches.yml", vec![plugin("a", 1)], patches).await;
    assert!(app.config("a").is_none());

    app.write(vec![plugin("a", 1), plugin("x", 1)]);
    app.include().refresh().await;

    // the new entry loads, and the patch is applied to the new parse too
    app.wait_for(|app| app.config("x").is_some()).await;
    assert!(app.config("a").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn rebuilds_an_entry_whose_name_changes_in_the_file() {
    let app = App::setup("reload-rename.yml", vec![plugin("a", 1)], json!({})).await;
    assert!(app.config("a").is_some());

    let renamed = "- id: a\n  name: group\n  group: true\n  config: []\n";
    app.write(renamed);
    app.settle().await;

    assert!(app.config("a").is_none());
    assert!(app.tree().resolve("a").unwrap().subgroup().is_some());
    // the old plugin is not taken for a self-dispose
    assert_eq!(app.text(), renamed);
}

#[tokio::test(flavor = "multi_thread")]
async fn reverts_an_entry_when_its_patch_is_removed() {
    let patches = json!({ "patches": [{ "id": "a", "disabled": true }] });
    let app = App::setup("reload-revert.yml", vec![plugin("a", 1)], patches).await;
    assert!(app.config("a").is_none());

    // patches apply to a copy of the parsed file, so dropping one reverts the entry
    let path = app.filename.to_string_lossy().to_string();
    app.loader.update(&app.id, json!({ "config": { "path": path } }), None, None).await.unwrap();

    app.wait_for(|app| app.config("a").is_some()).await;
    assert_eq!(app.value("a"), Some(1));
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_the_tree_reloadable_after_an_invalid_file() {
    let app = App::setup("reload-invalid.yml", vec![plugin("a", 1)], json!({})).await;

    app.write("a: 1\n");
    // refresh resolves on a bad file; the include reports it itself
    app.include().refresh().await;
    assert!(app.logs(Level::Warn).iter().any(|line| line.contains("failed to validate config file")));
    assert!(app.config("a").is_some());

    // the cache still holds the last good file, so the next good file reads
    // as a change and reconciles against an intact tree
    app.write(vec![plugin("a", 1), plugin("x", 1)]);
    app.include().refresh().await;
    app.wait_for(|app| app.config("x").is_some()).await;
    assert!(app.config("a").is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn never_overwrites_an_existing_but_unparsable_file_with_initial() {
    let broken = "- {\n";
    let app = App::setup("reload-initial.yml", broken, json!({ "initial": [plugin("x", 1)] })).await;

    // only a missing file falls back to `initial`; a file that failed to
    // parse belongs to the user and must survive
    assert_eq!(app.text(), broken);
    assert!(app.config("x").is_none());
}

/// Not in the TS spec: the counterpart of the case above.
#[tokio::test(flavor = "multi_thread")]
async fn writes_initial_for_a_missing_file() {
    let missing = common::fixture("reload-initial-missing.yml");
    let _ = std::fs::remove_file(&missing);
    let initial = json!({ "initial": [{ "name": "config-plugin", "config": { "tag": "i", "value": 1 } }] });
    let app = App::boot(missing, initial).await;
    assert!(app.config("i").is_some());
    assert!(app.read()[0]["id"].is_string());
}
