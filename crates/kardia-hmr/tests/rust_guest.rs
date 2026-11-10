//! Guest plugins written with `kardia-guest` (`fixtures/rust-guest`), as
//! dynamic libraries, as processes and as WASM modules.

mod common;

use std::env::consts::{DLL_SUFFIX, EXE_SUFFIX};
use std::path::{Path, PathBuf};

use common::{App, Kind, artifact, fiber, install, scratch};
use kardia_hmr::{DylibSource, ProcessSource, WasmSource};
use serde_json::json;

fn dylib(dir: &Path, version: &str) -> PathBuf {
    install(dir, &format!("greeter{DLL_SUFFIX}"), version, Kind::Dylib)
}

fn process(dir: &Path, version: &str) -> PathBuf {
    install(dir, &format!("greeter{EXE_SUFFIX}"), version, Kind::Process)
}

#[tokio::test]
async fn runs_a_dylib_plugin() {
    let dir = scratch("dylib-runs");
    let app = App::setup(DylibSource).await;
    app.load(&dylib(&dir, "v1"), json!({ "name": "alice" })).await;
    let input = json!({ "config": { "name": "alice" }, "imports": {} });
    assert_eq!(app.view("greeter"), json!({ "version": "v1", "input": input }));
    let greeter = app.service("greeter");
    let request = json!({ "key": "greeter", "method": "hello", "args": null });
    assert_eq!(greeter.call("hello", json!(null)).unwrap(), json!({ "version": "v1", "calls": 1, "request": request }));
    assert_eq!(greeter.call("hello", json!(null)).unwrap()["calls"], 2);
    assert!(app.logs().contains(&"greeter v1 applied".to_string()));
}

#[tokio::test]
async fn reports_guest_errors_and_panics_of_calls() {
    let dir = scratch("dylib-errors");
    let app = App::setup(DylibSource).await;
    app.load(&dylib(&dir, "v1"), json!({})).await;
    let greeter = app.service("greeter");
    assert_eq!(greeter.call("fail", json!(null)).unwrap_err().to_string(), "greeter refuses");
    let error = greeter.call("panic", json!(null)).unwrap_err();
    assert_eq!(error.to_string(), "the guest panicked: greeter panics");
    // the instance survives both
    assert_eq!(greeter.call("hello", json!(null)).unwrap()["calls"], 3);
}

#[tokio::test]
async fn reloads_a_rebuilt_library_in_place() {
    let dir = scratch("dylib-reload");
    let app = App::setup(DylibSource).await;
    let path = dylib(&dir, "v1");
    let entry = app.load(&path, json!({})).await;
    let inner = fiber(&entry);
    app.service("greeter").call("hello", json!(null)).unwrap();

    dylib(&dir, "v2");
    let result = app.reconcile(&[&path]).await.unwrap();
    assert_eq!(result.settled_fibers, vec![inner.clone()]);
    assert_eq!(fiber(&entry), inner);
    assert_eq!(app.view("greeter")["version"], "v2");
    // a new instance with fresh state
    assert_eq!(app.service("greeter").call("hello", json!(null)).unwrap()["calls"], 1);
    assert!(app.logs().contains(&"greeter v1 disposed".to_string()));
}

#[tokio::test]
async fn disposing_the_fiber_frees_the_guest_state() {
    let dir = scratch("dylib-dispose");
    let app = App::setup(DylibSource).await;
    let entry = app.load(&dylib(&dir, "v1"), json!({})).await;
    let greeter = app.service("greeter");

    app.loader.remove(&entry.id()).unwrap();
    app.loader.settled().await;
    assert!(app.logs().contains(&"greeter v1 disposed".to_string()));
    assert!(greeter.call("hello", json!(null)).unwrap_err().to_string().contains("disposed"));
}

#[tokio::test]
async fn runs_the_same_guest_as_a_wasm_module() {
    let dir = scratch("rust-wasm");
    let app = App::setup(WasmSource::new()).await;
    let path = dir.join("greeter.wasm");
    std::fs::copy(artifact("v1", Kind::Wasm), &path).unwrap();
    let entry = app.load(&path, json!({ "name": "alice" })).await;
    let input = json!({ "config": { "name": "alice" }, "imports": {} });
    assert_eq!(app.view("greeter"), json!({ "version": "v1", "input": input }));
    let greeter = app.service("greeter");
    assert_eq!(greeter.call("hello", json!(null)).unwrap()["calls"], 1);
    assert_eq!(greeter.call("fail", json!(null)).unwrap_err().to_string(), "greeter refuses");

    std::fs::copy(artifact("v2", Kind::Wasm), &path).unwrap();
    app.reconcile(&[&path]).await.unwrap();
    assert_eq!(app.view("greeter")["version"], "v2");
    app.loader.remove(&entry.id()).unwrap();
    app.loader.settled().await;
    assert!(app.logs().contains(&"greeter v2 disposed".to_string()));
}

#[tokio::test]
async fn runs_a_process_plugin() {
    let dir = scratch("process-runs");
    let app = App::setup(ProcessSource).await;
    app.load(&process(&dir, "v1"), json!({ "name": "alice" })).await;
    let input = json!({ "config": { "name": "alice" }, "imports": {} });
    assert_eq!(app.view("greeter"), json!({ "version": "v1", "input": input }));
    let greeter = app.service("greeter");
    let request = json!({ "key": "greeter", "method": "hello", "args": [1] });
    assert_eq!(greeter.call("hello", json!([1])).unwrap(), json!({ "version": "v1", "calls": 1, "request": request }));
    assert_eq!(greeter.call("hello", json!(null)).unwrap()["calls"], 2);
    assert_eq!(greeter.call("fail", json!(null)).unwrap_err().to_string(), "greeter refuses");
    let error = greeter.call("panic", json!(null)).unwrap_err();
    assert_eq!(error.to_string(), "the guest panicked: greeter panics");
    assert_eq!(greeter.call("hello", json!(null)).unwrap()["calls"], 5);
    assert!(app.logs().contains(&"greeter v1 applied".to_string()));
}

/// The host calls a guest makes during a call, through either transport.
fn check_host_calls(app: &App) {
    let greeter = app.service("greeter");
    assert_eq!(greeter.call("view", json!("greeter")).unwrap()["version"], "v1");
    let error = greeter.call("view", json!("missing")).unwrap_err().to_string();
    assert!(error.contains("missing"), "{error}");
    let request = json!({ "key": "greeter", "method": "hello", "args": null });
    let error = greeter.call("invoke", request).unwrap_err().to_string();
    assert_eq!(error, "re-entrant call into a guest instance");
}

#[tokio::test]
async fn a_process_guest_calls_the_host() {
    let dir = scratch("process-host-calls");
    let app = App::setup(ProcessSource).await;
    app.load(&process(&dir, "v1"), json!({})).await;
    check_host_calls(&app);
}

#[tokio::test]
async fn a_dylib_guest_calls_the_host() {
    let dir = scratch("dylib-host-calls");
    let app = App::setup(DylibSource).await;
    app.load(&dylib(&dir, "v1"), json!({})).await;
    check_host_calls(&app);
}

#[tokio::test]
async fn a_process_that_breaks_the_protocol_fails_its_later_calls() {
    let dir = scratch("process-protocol");
    let app = App::setup(ProcessSource).await;
    app.load(&process(&dir, "v1"), json!({})).await;
    let greeter = app.service("greeter");
    let error = greeter.call("print", json!(null)).unwrap_err().to_string();
    assert!(error.starts_with("invalid message from guest"), "{error}");
    assert_eq!(greeter.call("hello", json!(null)).unwrap_err().to_string(), "the guest process exited");
}

#[tokio::test]
async fn reloads_a_replaced_executable_by_respawning_it() {
    let dir = scratch("process-reload");
    let app = App::setup(ProcessSource).await;
    let path = process(&dir, "v1");
    let entry = app.load(&path, json!({})).await;
    let inner = fiber(&entry);
    let old = app.service("greeter").call("pid", json!(null)).unwrap();

    process(&dir, "v2");
    let result = app.reconcile(&[&path]).await.unwrap();
    assert_eq!(result.settled_fibers, vec![inner.clone()]);
    assert_eq!(fiber(&entry), inner);
    assert_eq!(app.view("greeter")["version"], "v2");
    // a new process with fresh state
    let greeter = app.service("greeter");
    assert_ne!(greeter.call("pid", json!(null)).unwrap(), old);
    assert_eq!(greeter.call("hello", json!(null)).unwrap()["calls"], 2);
    assert!(app.logs().contains(&"greeter v1 disposed".to_string()));
}

#[tokio::test]
async fn disposing_the_fiber_ends_the_process() {
    let dir = scratch("process-dispose");
    let app = App::setup(ProcessSource).await;
    let entry = app.load(&process(&dir, "v1"), json!({})).await;
    let greeter = app.service("greeter");
    let pid = greeter.call("pid", json!(null)).unwrap();

    app.loader.remove(&entry.id()).unwrap();
    app.loader.settled().await;
    assert!(app.logs().contains(&"greeter v1 disposed".to_string()));
    assert!(greeter.call("hello", json!(null)).unwrap_err().to_string().contains("disposed"));
    if cfg!(target_os = "linux") {
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }
}

#[tokio::test]
async fn a_crashing_process_fails_only_its_own_calls() {
    let dir = scratch("process-crash");
    let app = App::setup(ProcessSource).await;
    let entry = app.load(&process(&dir, "v1"), json!({})).await;
    let greeter = app.service("greeter");
    assert_eq!(greeter.call("exit", json!(null)).unwrap_err().to_string(), "the guest process exited");
    assert_eq!(greeter.call("hello", json!(null)).unwrap_err().to_string(), "the guest process exited");

    // the host carries on: the entry can be removed and loaded again
    app.loader.remove(&entry.id()).unwrap();
    app.loader.settled().await;
    app.load(&process(&dir, "v1"), json!({})).await;
    assert_eq!(app.service("greeter").call("hello", json!(null)).unwrap()["calls"], 1);
}
