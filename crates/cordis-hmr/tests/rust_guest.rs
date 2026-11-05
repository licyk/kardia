//! Guest plugins written with `cordis-guest` (`fixtures/rust-guest`), as
//! dynamic libraries, as processes and as WASM modules.

mod common;

use std::collections::HashMap;
use std::env::consts::{DLL_PREFIX, DLL_SUFFIX, EXE_SUFFIX};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use common::{App, fiber, scratch};
use cordis_hmr::{DylibSource, ProcessSource, WasmSource};
use serde_json::json;

#[derive(Clone, Copy, Debug)]
enum Kind {
    Dylib,
    Process,
    /// Needs `rustup target add wasm32-unknown-unknown`.
    Wasm,
}

/// Builds the fixture once per version and kind; returns a link to the
/// artifact, so that later builds (which replace it) do not change it.
fn artifact(version: &str, kind: Kind) -> PathBuf {
    static BUILT: Mutex<Option<HashMap<String, PathBuf>>> = Mutex::new(None);
    let mut built = BUILT.lock().unwrap();
    let key = format!("{version}-{kind:?}");
    if let Some(path) = built.get_or_insert_default().get(&key) {
        return path.clone();
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust-guest/Cargo.toml");
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("rust-guest");
    let mut build = Command::new(env!("CARGO"));
    build.args(["build", "--quiet", "--manifest-path"]).arg(&manifest).arg("--target-dir").arg(&dir);
    match kind {
        Kind::Dylib => build.arg("--lib"),
        Kind::Process => build.args(["--bin", "rust-guest-process"]),
        Kind::Wasm => build.args(["--lib", "--target", "wasm32-unknown-unknown"]),
    };
    if version == "v2" {
        build.args(["--features", "v2"]);
    }
    assert!(build.status().unwrap().success(), "cannot build the rust guest for {key}");
    let output = match kind {
        Kind::Dylib => dir.join(format!("debug/{DLL_PREFIX}rust_guest{DLL_SUFFIX}")),
        Kind::Process => dir.join(format!("debug/rust-guest-process{EXE_SUFFIX}")),
        Kind::Wasm => dir.join("wasm32-unknown-unknown/debug/rust_guest.wasm"),
    };
    let copy = dir.join(format!("{key}-{}", output.file_name().unwrap().to_string_lossy()));
    link(&output, &copy);
    built.get_or_insert_default().insert(key, copy.clone());
    copy
}

/// Executables are linked, never written: a file this process holds open
/// for writing cannot be started while another thread forks ("Text file
/// busy"), as the fork inherits the handle until it execs.
fn link(from: &Path, to: &Path) {
    let _ = std::fs::remove_file(to);
    std::fs::hard_link(from, to).unwrap();
}

/// Puts the artifact at `dir/name`, replacing it through a rename, as an
/// executable that is running cannot be written over.
fn install(dir: &Path, name: &str, version: &str, kind: Kind) -> PathBuf {
    let (path, staged) = (dir.join(name), dir.join(format!(".{name}.new")));
    link(&artifact(version, kind), &staged);
    std::fs::rename(&staged, &path).unwrap();
    path
}

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
