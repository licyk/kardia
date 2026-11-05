//! Guest plugins in dynamic libraries, built from `fixtures/dylib-guest`.

mod common;

use std::env::consts::{DLL_PREFIX, DLL_SUFFIX};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use common::{App, fiber, scratch};
use cordis_hmr::DylibSource;
use serde_json::json;

/// Builds the fixture once per version and returns the library.
fn library(version: &str) -> &'static Path {
    static V1: OnceLock<PathBuf> = OnceLock::new();
    static V2: OnceLock<PathBuf> = OnceLock::new();
    let cell = if version == "v1" { &V1 } else { &V2 };
    cell.get_or_init(|| {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dylib-guest/Cargo.toml");
        let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("dylib-guest-{version}"));
        let mut build = Command::new(env!("CARGO"));
        build.args(["build", "--quiet", "--manifest-path"]).arg(&manifest).arg("--target-dir").arg(&target);
        if version == "v2" {
            build.args(["--features", "v2"]);
        }
        assert!(build.status().unwrap().success(), "cannot build the dylib guest");
        target.join("debug").join(format!("{DLL_PREFIX}dylib_guest{DLL_SUFFIX}"))
    })
}

fn install(dir: &Path, version: &str) -> PathBuf {
    let path = dir.join(format!("greeter{DLL_SUFFIX}"));
    std::fs::copy(library(version), &path).unwrap();
    path
}

#[tokio::test]
async fn runs_a_dylib_plugin() {
    let dir = scratch("dylib-runs");
    let app = App::setup(DylibSource).await;
    let path = install(&dir, "v1");
    app.load(&path, json!({ "name": "alice" })).await;
    let input = json!({ "config": { "name": "alice" }, "imports": {} });
    assert_eq!(app.view("greeter"), json!({ "version": "v1", "input": input }));
    let greeter = app.service("greeter");
    let request = json!({ "key": "greeter", "method": "hello", "args": null });
    assert_eq!(greeter.call("hello", json!(null)).unwrap(), json!({ "version": "v1", "calls": 1, "request": request }));
    assert_eq!(greeter.call("hello", json!(null)).unwrap()["calls"], 2);
    assert!(app.logs().contains(&"greeter v1 applied".to_string()));
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
