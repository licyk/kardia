//! Guest plugins compiled from WAT text by the wasmtime backend.

mod common;

use std::path::{Path, PathBuf};

use common::{App, fiber, scratch};
use kardia_core::{FiberState, PluginDef};
use kardia_hmr::WasmSource;
use serde_json::{Value, json};

/// A guest module whose metadata is `meta`. `@N` in `body` stands for the
/// pointer and length of `strings[N]`; the guest's buffer starts at 1024.
fn wat(meta: Value, strings: &[&str], body: &str) -> String {
    let mut offset = 0;
    let mut data = String::new();
    let mut place = |text: &str| {
        data += &format!("(data (i32.const {offset}) \"{}\")\n", text.replace('\\', "\\\\").replace('"', "\\\""));
        offset += text.len();
        format!("(i32.const {}) (i32.const {})", offset - text.len(), text.len())
    };
    let meta = place(&meta.to_string());
    let mut body = body.to_string();
    for (index, text) in strings.iter().enumerate() {
        body = body.replace(&format!("@{index}"), &place(text));
    }
    format!(
        r#"(module
  (import "cordis" "read" (func $read (param i32)))
  (import "cordis" "write" (func $write (param i32 i32)))
  (import "cordis" "log" (func $log (param i32 i32 i32)))
  (import "cordis" "provide" (func $provide (param i32 i32 i32 i32) (result i32)))
  (import "cordis" "view" (func $view (param i32 i32) (result i32)))
  (import "cordis" "invoke" (func $invoke (param i32 i32 i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  {data}
  (func (export "cordis_meta") (result i32) (call $write {meta}) (i32.const 0))
  {body})"#
    )
}

/// Provides `greeter` (viewed as its apply input) and `version`; calls
/// answer with their request.
fn greeter(version: &str, meta: Value) -> String {
    let (applied, disposed) = (format!("greeter {version} applied"), format!("greeter {version} disposed"));
    wat(
        meta,
        &["greeter", "version", &format!("\"{version}\""), &applied, &disposed],
        r#"
  (func (export "cordis_apply") (param $len i32) (result i32)
    (call $read (i32.const 1024))
    (drop (call $provide @0 (i32.const 1024) (local.get $len)))
    (drop (call $provide @1 @2))
    (call $log (i32.const 2) @3)
    (i32.const 0))
  (func (export "cordis_call") (param $len i32) (result i32)
    (call $read (i32.const 1024))
    (call $write (i32.const 1024) (local.get $len))
    (i32.const 0))
  (func (export "cordis_dispose") (call $log (i32.const 2) @4))"#,
    )
}

fn greeter_meta() -> Value {
    json!({ "plugin": { "name": "greeter", "provide": ["greeter", "version"] } })
}

/// Fails its apply with the error of a host call: views `source`, or
/// invokes `greeter`, and provides the result as `copy`.
fn forwarder(meta: Value, call: &str) -> String {
    let body = r#"
  (func (export "cordis_apply") (param $len i32) (result i32)
    (local $result i32)
    (local.set $result CALL)
    (if (i32.lt_s (local.get $result) (i32.const 0))
      (then
        (call $read (i32.const 1024))
        (call $write (i32.const 1024) (i32.sub (i32.const -1) (local.get $result)))
        (return (i32.const 1))))
    (call $read (i32.const 1024))
    (drop (call $provide @2 (i32.const 1024) (local.get $result)))
    (i32.const 0))"#;
    wat(meta, &["source", "greeter", "copy", "hello", "[2]", "self"], &body.replace("CALL", call))
}

fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path
}

#[tokio::test]
async fn runs_a_wasm_plugin() {
    let dir = scratch("wasm-runs");
    let app = App::setup(WasmSource::new()).await;
    let path = write(&dir, "greeter.wat", &greeter("v1", greeter_meta()));
    let entry = app.load(&path, json!({ "name": "alice" })).await;
    assert_eq!(fiber(&entry).state(), FiberState::Active);
    assert_eq!(app.view("greeter"), json!({ "config": { "name": "alice" }, "imports": {} }));
    assert_eq!(app.view("version"), json!("v1"));
    let answer = app.service("greeter").call("hello", json!([1])).unwrap();
    assert_eq!(answer, json!({ "key": "greeter", "method": "hello", "args": [1] }));
    assert!(app.logs().contains(&"greeter v1 applied".to_string()));
}

#[tokio::test]
async fn reloads_a_changed_module_in_place() {
    let dir = scratch("wasm-reload");
    let app = App::setup(WasmSource::new()).await;
    let path = write(&dir, "greeter.wat", &greeter("v1", greeter_meta()));
    let entry = app.load(&path, json!({})).await;
    let inner = fiber(&entry);

    write(&dir, "greeter.wat", &greeter("v2", greeter_meta()));
    let result = app.reconcile(&[&path]).await.unwrap();
    assert_eq!(result.settled_fibers, vec![inner.clone()]);
    assert_eq!(fiber(&entry), inner);
    assert_eq!(app.view("version"), json!("v2"));
    let logs = app.logs();
    let position = |text: &str| logs.iter().position(|log| log == text).unwrap();
    assert!(position("greeter v1 disposed") < position("greeter v2 applied"));
}

#[tokio::test]
async fn disposing_the_fiber_releases_the_instance() {
    let dir = scratch("wasm-dispose");
    let app = App::setup(WasmSource::new()).await;
    let path = write(&dir, "greeter.wat", &greeter("v1", greeter_meta()));
    let entry = app.load(&path, json!({})).await;
    let greeter = app.service("greeter");

    app.loader.remove(&entry.id()).unwrap();
    app.loader.settled().await;
    assert!(app.logs().contains(&"greeter v1 disposed".to_string()));
    assert!(app.root.view("greeter").is_err());
    let error = greeter.call("hello", json!(null)).unwrap_err();
    assert!(error.to_string().contains("disposed"), "{error}");
}

#[tokio::test]
async fn guest_failures_and_traps_fail_the_plugin() {
    let dir = scratch("wasm-failures");
    let app = App::setup(WasmSource::new()).await;
    let plugin = json!({ "plugin": {} });
    let failing = wat(
        plugin.clone(),
        &["no greeting today"],
        r#"(func (export "cordis_apply") (param i32) (result i32) (call $write @0) (i32.const 1))"#,
    );
    let entry = app.load(&write(&dir, "failing.wat", &failing), json!({})).await;
    assert_eq!(fiber(&entry).state(), FiberState::Failed);
    assert_eq!(fiber(&entry).error().unwrap().to_string(), "no greeting today");

    let trapping = wat(plugin, &[], r#"(func (export "cordis_apply") (param i32) (result i32) unreachable)"#);
    let entry = app.load(&write(&dir, "trapping.wat", &trapping), json!({})).await;
    assert_eq!(fiber(&entry).state(), FiberState::Failed);
    assert!(fiber(&entry).error().unwrap().to_string().contains("unreachable"));
}

#[tokio::test]
async fn views_declared_services_only() {
    let dir = scratch("wasm-views");
    let app = App::setup(WasmSource::new()).await;
    let source = PluginDef::new(|ctx, _| async move {
        ctx.provide_serialized_as("source", json!({ "n": 1 }))?;
        Ok(())
    });
    app.root.plugin_def(&source, Value::Null).unwrap().settled().await.unwrap();
    let view = "(call $view @0)";
    let declared = forwarder(json!({ "plugin": { "inject": ["source"] } }), view);
    let entry = app.load(&write(&dir, "declared.wat", &declared), json!({})).await;
    assert_eq!(fiber(&entry).state(), FiberState::Active);
    assert_eq!(app.view("copy"), json!({ "n": 1 }));

    let undeclared = forwarder(json!({ "plugin": {} }), view);
    let entry = app.load(&write(&dir, "undeclared.wat", &undeclared), json!({})).await;
    assert!(fiber(&entry).error().unwrap().to_string().contains("source"));
}

#[tokio::test]
async fn calls_services_of_other_guests() {
    let dir = scratch("wasm-calls");
    let app = App::setup(WasmSource::new()).await;
    app.load(&write(&dir, "greeter.wat", &greeter("v1", greeter_meta())), json!({})).await;
    let caller = forwarder(json!({ "plugin": { "inject": ["greeter"] } }), "(call $invoke @1 @3 @4)");
    let entry = app.load(&write(&dir, "caller.wat", &caller), json!({})).await;
    assert_eq!(fiber(&entry).state(), FiberState::Active);
    assert_eq!(app.view("copy"), json!({ "key": "greeter", "method": "hello", "args": [2] }));
}

#[tokio::test]
async fn rejects_calls_back_into_a_running_guest() {
    let dir = scratch("wasm-reentrant");
    let app = App::setup(WasmSource::new()).await;
    let reentrant = forwarder(json!({ "plugin": {} }), "(drop (call $provide @5 @4)) (call $invoke @5 @3 @4)");
    let entry = app.load(&write(&dir, "reentrant.wat", &reentrant), json!({})).await;
    assert!(fiber(&entry).error().unwrap().to_string().contains("re-entrant"));
}

#[tokio::test]
async fn passes_dependency_values_and_reloads_dependents() {
    let dir = scratch("wasm-deps");
    let app = App::setup(WasmSource::new()).await;
    let data = |n: u32| wat(json!({ "value": { "n": n } }), &[], "");
    let data_path = write(&dir, "data.wat", &data(1));
    let meta = json!({ "deps": ["./data.wat"], "plugin": { "provide": ["greeter", "version"] } });
    app.load(&write(&dir, "greeter.wat", &greeter("v1", meta)), json!({})).await;
    let id = data_path.to_str().unwrap();
    assert_eq!(app.view("greeter")["imports"], json!({ id: { "n": 1 } }));

    write(&dir, "data.wat", &data(2));
    app.reconcile(&[&data_path]).await.unwrap();
    assert_eq!(app.view("greeter")["imports"], json!({ id: { "n": 2 } }));
}
