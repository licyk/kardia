//! Port of `packages/include/tests/watch.spec.ts`.
//!
//! The TS spec swaps `fs` for a fake that records the watched paths. The
//! `fs` service here is a concrete type, so a watch is observed through
//! its effect instead: an external edit reaching the tree without a refresh.

mod common;

use std::time::{Duration, Instant};

use common::{App, plugin};
use serde_json::json;

/// Rewrites the file with growing values of `a` until the tree sees one or
/// `budget` runs out; some platforms deliver the first events late or drop
/// single events.
async fn sees_external_edits(app: &App, budget: Duration) -> bool {
    let start = Instant::now();
    let mut value = 100;
    while start.elapsed() < budget {
        value += 1;
        app.write(vec![plugin("a", value)]);
        tokio::time::sleep(Duration::from_millis(100)).await;
        if app.value("a").is_some_and(|current| current > 99) {
            return true;
        }
    }
    false
}

#[tokio::test(flavor = "multi_thread")]
async fn does_not_watch_the_config_file_by_default() {
    let app = App::setup("watch-default.yml", vec![plugin("a", 0)], json!({})).await;
    assert!(!sees_external_edits(&app, Duration::from_secs(1)).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn watches_the_config_file_with_watch_true() {
    let app = App::setup("watch-enabled.yml", vec![plugin("a", 0)], json!({ "watch": true })).await;
    assert!(sees_external_edits(&app, Duration::from_secs(20)).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn restarts_when_watch_is_toggled() {
    let app = App::setup("watch-toggle.yml", vec![plugin("a", 0)], json!({})).await;
    let config = json!({ "path": app.filename.to_string_lossy(), "watch": true });
    app.loader.update(&app.id, json!({ "config": config }), None, None).await.unwrap();
    app.loader.settled().await;
    assert!(sees_external_edits(&app, Duration::from_secs(20)).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_watching_the_file_across_its_own_rename_based_writes() {
    let app = App::setup("watch-fs.yml", vec![plugin("a", 0)], json!({ "watch": true })).await;

    // two runtime writes, each replacing the file through a rename
    for value in [1, 2] {
        app.update("a", json!({ "config": { "tag": "a", "value": value } })).await;
        app.include().refresh().await;
    }
    assert!(app.text().contains("value: 2"));
    tokio::time::sleep(Duration::from_millis(500)).await;

    // an external edit still reaches the tree through the watch
    assert!(sees_external_edits(&app, Duration::from_secs(20)).await);

    // and the writes of the include itself do not start a feedback loop (the
    // TS spec counts refresh calls; here the file must stay as written)
    app.include().flush().await;
    let external = app.text();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(app.text(), external);
}
