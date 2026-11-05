//! Guest plugins written with `cordis-guest` (`fixtures/rust-guest`), as
//! dynamic libraries and as WASM modules.

mod common;

use std::collections::HashMap;
use std::env::consts::{DLL_PREFIX, DLL_SUFFIX};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use common::{App, fiber, scratch};
use cordis_hmr::{DylibSource, WasmSource};
use serde_json::json;

const WASM: &str = "wasm32-unknown-unknown";

/// Builds the fixture once per version and target; returns a copy of the
/// artifact, so that later builds do not overwrite it.
fn artifact(version: &str, target: Option<&str>) -> PathBuf {
    static BUILT: Mutex<Option<HashMap<String, PathBuf>>> = Mutex::new(None);
    let mut built = BUILT.lock().unwrap();
    let key = format!("{version}-{}", target.unwrap_or("host"));
    if let Some(path) = built.get_or_insert_default().get(&key) {
        return path.clone();
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust-guest/Cargo.toml");
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("rust-guest");
    let mut build = Command::new(env!("CARGO"));
    build.args(["build", "--quiet", "--manifest-path"]).arg(&manifest).arg("--target-dir").arg(&dir);
    if let Some(target) = target {
        build.args(["--target", target]);
    }
    if version == "v2" {
        build.args(["--features", "v2"]);
    }
    assert!(build.status().unwrap().success(), "cannot build the rust guest");
    let output = match target {
        Some(target) => dir.join(target).join("debug/rust_guest.wasm"),
        None => dir.join(format!("debug/{DLL_PREFIX}rust_guest{DLL_SUFFIX}")),
    };
    let copy = dir.join(format!("{key}-{}", output.file_name().unwrap().to_string_lossy()));
    std::fs::copy(output, &copy).unwrap();
    built.get_or_insert_default().insert(key, copy.clone());
    copy
}

fn install(dir: &Path, version: &str) -> PathBuf {
    let path = dir.join(format!("greeter{DLL_SUFFIX}"));
    std::fs::copy(artifact(version, None), &path).unwrap();
    path
}

#[tokio::test]
async fn runs_a_dylib_plugin() {
    let dir = scratch("dylib-runs");
    let app = App::setup(DylibSource).await;
    app.load(&install(&dir, "v1"), json!({ "name": "alice" })).await;
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
    app.load(&install(&dir, "v1"), json!({})).await;
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
    let path = install(&dir, "v1");
    let entry = app.load(&path, json!({})).await;
    let inner = fiber(&entry);
    app.service("greeter").call("hello", json!(null)).unwrap();

    install(&dir, "v2");
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
    let entry = app.load(&install(&dir, "v1"), json!({})).await;
    let greeter = app.service("greeter");

    app.loader.remove(&entry.id()).unwrap();
    app.loader.settled().await;
    assert!(app.logs().contains(&"greeter v1 disposed".to_string()));
    assert!(greeter.call("hello", json!(null)).unwrap_err().to_string().contains("disposed"));
}

#[tokio::test]
#[ignore = "needs the wasm32-unknown-unknown target (rustup target add wasm32-unknown-unknown)"]
async fn runs_the_same_guest_as_a_wasm_module() {
    let dir = scratch("rust-wasm");
    let app = App::setup(WasmSource::new()).await;
    let path = dir.join("greeter.wasm");
    std::fs::copy(artifact("v1", Some(WASM)), &path).unwrap();
    let entry = app.load(&path, json!({ "name": "alice" })).await;
    let input = json!({ "config": { "name": "alice" }, "imports": {} });
    assert_eq!(app.view("greeter"), json!({ "version": "v1", "input": input }));
    let greeter = app.service("greeter");
    assert_eq!(greeter.call("hello", json!(null)).unwrap()["calls"], 1);
    assert_eq!(greeter.call("fail", json!(null)).unwrap_err().to_string(), "greeter refuses");

    std::fs::copy(artifact("v2", Some(WASM)), &path).unwrap();
    app.reconcile(&[&path]).await.unwrap();
    assert_eq!(app.view("greeter")["version"], "v2");
    app.loader.remove(&entry.id()).unwrap();
    app.loader.settled().await;
    assert!(app.logs().contains(&"greeter v2 disposed".to_string()));
}
