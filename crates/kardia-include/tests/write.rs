//! Port of `packages/include/tests/write.spec.ts`: failures injected into
//! the rename that replaces the file.

mod common;

use std::io::ErrorKind;

use common::{App, Faults, plugin, write_fixture};
use kardia_core::Level;
use kardia_include::yaml;
use serde_json::{Value, json};

async fn setup(name: &str, data: Vec<Value>) -> (App, Faults) {
    let faults = Faults::default();
    let app = App::boot_faulty(write_fixture(name, data), json!({}), Some(faults.clone())).await;
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
        renames.kind = Some(ErrorKind::StorageFull);
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
