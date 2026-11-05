//! Port of `packages/include/tests/patch.spec.ts`. The fixture `base.yml`
//! holds a plain entry `timer` and a group `group` with the entry `inner`.

mod common;

use common::{App, group, plugin};
use cordis_core::Level;
use serde_json::{Value, json};

async fn setup(name: &str, patches: Value) -> App {
    let base = vec![plugin("timer", 1), group("group", vec![plugin("inner", 1)])];
    App::setup(name, base, json!({ "patches": patches })).await
}

fn extra() -> Value {
    json!({ "name": "config-plugin", "config": { "tag": "extra", "value": 1 } })
}

fn warned(app: &App, text: &str) -> bool {
    app.logs(Level::Warn).iter().any(|line| line.contains(text))
}

#[tokio::test(flavor = "multi_thread")]
async fn should_load_without_patches() {
    let app = setup("patch-none.yml", json!([])).await;
    assert_eq!(app.value("inner"), Some(1));
}

#[tokio::test(flavor = "multi_thread")]
async fn should_disable_an_entry_via_patch() {
    let app = setup("patch-disable.yml", json!([{ "id": "inner", "disabled": true }])).await;
    assert!(app.config("inner").is_none());
    assert!(app.config("timer").is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn should_override_config_via_patch() {
    let patches = json!([{ "id": "inner", "config": { "tag": "inner", "custom": true } }]);
    let app = setup("patch-config.yml", patches).await;
    assert_eq!(app.config("inner"), Some(json!({ "tag": "inner", "custom": true })));
}

#[tokio::test(flavor = "multi_thread")]
async fn should_warn_on_name_mismatch_and_skip_patch() {
    let patches = json!([{ "id": "inner", "name": "wrong-name", "disabled": true }]);
    let app = setup("patch-mismatch.yml", patches).await;
    // the patch is skipped, so the entry stays active
    assert_eq!(app.value("inner"), Some(1));
    assert!(warned(&app, "name mismatch for inner"));
}

#[tokio::test(flavor = "multi_thread")]
async fn should_warn_on_non_existent_id() {
    let app = setup("patch-missing.yml", json!([{ "id": "nonexistent", "disabled": true }])).await;
    assert_eq!(app.value("inner"), Some(1));
    assert!(warned(&app, "entry nonexistent not found"));
}

#[tokio::test(flavor = "multi_thread")]
async fn should_insert_entries_into_root_group() {
    let app = setup("patch-insert-root.yml", json!([{ "insert": [extra()] }])).await;
    assert_eq!(app.value("extra"), Some(1));
    assert_eq!(app.value("inner"), Some(1));
    assert!(
        app.tree()
            .entries()
            .iter()
            .any(|entry| entry.parent() == app.tree().root() && entry.options()["config"]["tag"] == json!("extra"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn should_insert_entries_into_a_specific_group() {
    let app = setup("patch-insert-group.yml", json!([{ "id": "group", "insert": [extra()] }])).await;
    assert_eq!(app.value("extra"), Some(1));
    let tree = app.tree();
    let inserted = tree.entries().into_iter().find(|entry| entry.options()["config"]["tag"] == json!("extra"));
    assert_eq!(inserted.map(|entry| entry.parent()), tree.resolve("group").unwrap().subgroup());
}

#[tokio::test(flavor = "multi_thread")]
async fn should_warn_when_inserting_into_a_non_group_entry() {
    let app = setup("patch-insert-plain.yml", json!([{ "id": "timer", "insert": [extra()] }])).await;
    assert_eq!(app.value("inner"), Some(1));
    // timer is not a group
    assert!(app.config("extra").is_none());
    assert!(warned(&app, "entry timer is not a group"));
}

#[tokio::test(flavor = "multi_thread")]
async fn should_apply_multiple_patches() {
    let patches = json!([{ "id": "inner", "disabled": true }, { "insert": [extra()] }]);
    let app = setup("patch-multiple.yml", patches).await;
    assert!(app.config("inner").is_none());
    assert_eq!(app.value("extra"), Some(1));
}

#[tokio::test(flavor = "multi_thread")]
async fn should_let_a_later_patch_address_a_row_inserted_by_an_earlier_patch() {
    let mut added = extra();
    added["id"] = json!("added");
    let patches = json!([{ "insert": [added] }, { "id": "added", "disabled": true }]);
    let app = setup("patch-later.yml", patches).await;
    assert_eq!(app.value("inner"), Some(1));
    // inserted rows are indexed as they land, so the second patch finds `added`
    assert!(app.config("extra").is_none());
    assert!(app.tree().resolve("added").unwrap().is_disabled());
}

#[tokio::test(flavor = "multi_thread")]
async fn should_validate_name_consistency_matching_name_is_ok() {
    let patches = json!([{ "id": "inner", "name": "config-plugin", "disabled": true }]);
    let app = setup("patch-matching.yml", patches).await;
    // the name matches, so the patch applies
    assert!(app.config("inner").is_none());
    assert!(!warned(&app, "name mismatch"));
}
