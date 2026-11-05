//! Port of `packages/include/tests/write.spec.ts`: failures injected into
//! the rename that replaces the file.

mod common;

use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;

use common::{App, plugin, write_fixture};
use kardia_core::Level;
use kardia_include::{IncludePlugin, yaml};
use parking_lot::Mutex;
use serde_json::{Value, json};

/// Every scheduled failure fails with `kind`; `before_fail` runs right
/// before it, so a test can simulate an external writer racing the include.
struct Renames {
    calls: usize,
    failures: usize,
    kind: ErrorKind,
    before_fail: Option<Box<dyn Fn() + Send>>,
}

type Faults = Arc<Mutex<Renames>>;

async fn setup(name: &str, data: Vec<Value>) -> (App, Faults) {
    let faults: Faults =
        Arc::new(Mutex::new(Renames { calls: 0, failures: 0, kind: ErrorKind::ResourceBusy, before_fail: None }));
    let hook = faults.clone();
    let include = IncludePlugin::with_rename(move |from: &Path, to: &Path| {
        let mut renames = hook.lock();
        renames.calls += 1;
        if renames.failures > 0 {
            renames.failures -= 1;
            if let Some(before_fail) = &renames.before_fail {
                before_fail();
            }
            return Err(renames.kind.into());
        }
        std::fs::rename(from, to)
    });
    let app = App::boot_with(write_fixture(name, data), json!({}), include).await;
    (app, faults)
}

#[tokio::test(flavor = "multi_thread")]
async fn retries_a_rename_that_fails_transiently() {
    let (app, faults) = setup("write-retry.yml", vec![plugin("a", 1)]).await;
    faults.lock().failures = 2;

    app.update("a", json!({ "config": { "tag": "a", "value": 2 } })).await;
    app.settle().await;

    assert_eq!(faults.lock().calls, 3);
    assert_eq!(app.read()[0]["config"]["value"], json!(2));
    assert_eq!(app.logs(Level::Warn), Vec::<String>::new());
    assert_eq!(app.logs(Level::Error), Vec::<String>::new());
}

#[tokio::test(flavor = "multi_thread")]
async fn never_overwrites_a_change_that_lands_while_it_waits_to_retry() {
    let (app, faults) = setup("write-race.yml", vec![plugin("a", 1), plugin("b", 1)]).await;
    let filename = app.filename.clone();
    let external = yaml::dump(&Value::Array(vec![plugin("a", 1), plugin("b", 2)]));
    {
        let mut renames = faults.lock();
        renames.failures = 1;
        renames.before_fail = Some(Box::new(move || std::fs::write(&filename, &external).unwrap()));
    }

    app.update("a", json!({ "config": { "tag": "a", "value": 2 } })).await;
    app.settle().await;

    // the stale check before the retry sees the external edit, and the
    // runtime change is merged into it
    let data = app.read();
    assert_eq!((data[0]["config"]["value"].clone(), data[1]["config"]["value"].clone()), (json!(2), json!(2)));
    assert_eq!(app.value("b"), Some(2));
    assert_eq!(faults.lock().calls, 2);
    assert_eq!(app.logs(Level::Error), Vec::<String>::new());
}

#[tokio::test(flavor = "multi_thread")]
async fn does_not_retry_other_errors_but_still_flushes_on_dispose() {
    let (app, faults) = setup("write-fatal.yml", vec![plugin("a", 1)]).await;
    let text = app.text();
    {
        let mut renames = faults.lock();
        renames.failures = 1;
        renames.kind = ErrorKind::StorageFull;
    }

    app.update("a", json!({ "config": { "tag": "a", "value": 2 } })).await;
    app.settle().await;

    assert_eq!(faults.lock().calls, 1);
    assert_eq!(app.text(), text);
    assert!(app.logs(Level::Warn).iter().any(|line| line.contains("failed to write")));
    assert_eq!(app.value("a"), Some(2));

    app.dispose().await;
    assert_eq!(app.read()[0]["config"]["value"], json!(2));
}

#[tokio::test(flavor = "multi_thread")]
async fn gives_up_after_a_bounded_number_of_attempts_and_keeps_the_change() {
    let (app, faults) = setup("write-bounded.yml", vec![plugin("a", 1)]).await;
    let text = app.text();
    faults.lock().failures = usize::MAX;

    app.update("a", json!({ "config": { "tag": "a", "value": 2 } })).await;
    app.settle().await;

    assert_eq!(faults.lock().calls, 11);
    assert_eq!(app.text(), text);
    assert!(app.logs(Level::Warn).iter().any(|line| line.contains("failed to write")));
    assert_eq!(app.value("a"), Some(2));

    // the lock clears; the journal goes out with the dispose flush
    faults.lock().failures = 0;
    app.dispose().await;
    assert_eq!(app.read()[0]["config"]["value"], json!(2));
}
