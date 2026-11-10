//! Guest plugins compiled from WAT text by the wasmtime backend.

mod common;

use std::path::{Path, PathBuf};

use common::{App, fiber, scratch};
use kardia_core::{Context, Error, FiberState, PluginDef};
use std::time::{Duration, Instant};

use kardia_hmr::{GuestFailure, GuestFailureKind, GuestMeta, GuestService, WasmGuest, WasmLimits, WasmSource};
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
    assert_eq!(kind(&fiber(&entry).error().unwrap()), None);

    let trapping = wat(plugin, &[], r#"(func (export "cordis_apply") (param i32) (result i32) unreachable)"#);
    let entry = app.load(&write(&dir, "trapping.wat", &trapping), json!({})).await;
    assert_eq!(fiber(&entry).state(), FiberState::Failed);
    assert!(fiber(&entry).error().unwrap().to_string().contains("unreachable"));
    assert_eq!(kind(&fiber(&entry).error().unwrap()), Some(GuestFailureKind::Trap));

    let missing = wat(json!({ "plugin": {} }), &[], "");
    let entry = app.load(&write(&dir, "missing.wat", &missing), json!({})).await;
    let error = fiber(&entry).error().unwrap();
    assert_eq!(error.to_string(), "failed to find function export `cordis_apply`");
    assert_eq!(kind(&error), None);
}

/// How the instance failed, if the error is its failure.
fn kind(error: &Error) -> Option<GuestFailureKind> {
    GuestFailure::of(error).map(|failure| failure.kind)
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
    let declared = forwarder(json!({ "plugin": { "inject": ["source"], "provide": ["copy"] } }), view);
    let entry = app.load(&write(&dir, "declared.wat", &declared), json!({})).await;
    assert_eq!(fiber(&entry).state(), FiberState::Active);
    assert_eq!(app.view("copy"), json!({ "n": 1 }));

    let undeclared = forwarder(json!({ "plugin": { "provide": ["copy"] } }), view);
    let entry = app.load(&write(&dir, "undeclared.wat", &undeclared), json!({})).await;
    assert!(fiber(&entry).error().unwrap().to_string().contains("source"));
}

#[tokio::test]
async fn calls_services_of_other_guests() {
    let dir = scratch("wasm-calls");
    let app = App::setup(WasmSource::new()).await;
    app.load(&write(&dir, "greeter.wat", &greeter("v1", greeter_meta())), json!({})).await;
    let caller =
        forwarder(json!({ "plugin": { "inject": ["greeter"], "provide": ["copy"] } }), "(call $invoke @1 @3 @4)");
    let entry = app.load(&write(&dir, "caller.wat", &caller), json!({})).await;
    assert_eq!(fiber(&entry).state(), FiberState::Active);
    assert_eq!(app.view("copy"), json!({ "key": "greeter", "method": "hello", "args": [2] }));
}

#[tokio::test]
async fn rejects_calls_back_into_a_running_guest() {
    let dir = scratch("wasm-reentrant");
    let app = App::setup(WasmSource::new()).await;
    let reentrant = forwarder(
        json!({ "plugin": { "provide": ["self", "copy"] } }),
        "(drop (call $provide @5 @4)) (call $invoke @5 @3 @4)",
    );
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

/// A plugin whose apply never returns, and whose calls loop forever after the
/// first one.
fn looping() -> String {
    wat(
        json!({ "plugin": { "provide": ["looping"] } }),
        &["looping", "{}"],
        r#"
  (global $calls (mut i32) (i32.const 0))
  (func (export "cordis_apply") (param i32) (result i32)
    (drop (call $provide @0 @1))
    (i32.const 0))
  (func (export "cordis_call") (param i32) (result i32)
    (global.set $calls (i32.add (global.get $calls) (i32.const 1)))
    (if (i32.gt_s (global.get $calls) (i32.const 1)) (then (loop $forever (br $forever))))
    (call $write @1)
    (i32.const 0))"#,
    )
}

#[tokio::test]
async fn fuel_stops_an_infinite_loop_and_the_guest() {
    let dir = scratch("wasm-fuel");
    let app = App::setup(WasmSource::with_limits(WasmLimits::new().fuel(1_000_000))).await;
    app.load(&write(&dir, "looping.wat", &looping()), json!({})).await;
    let looping = app.service("looping");
    assert_eq!(looping.call("go", json!(null)).unwrap(), json!({}));
    let error = looping.call("go", json!(null)).unwrap_err();
    assert_eq!(kind(&error), Some(GuestFailureKind::OutOfFuel));
    let error = error.to_string();
    assert!(error.contains("fuel"), "{error}");
    let error = looping.call("go", json!(null)).unwrap_err();
    assert_eq!(kind(&error), Some(GuestFailureKind::OutOfFuel));
    let error = error.to_string();
    assert!(error.starts_with("the guest was stopped"), "{error}");
}

#[tokio::test]
async fn a_timeout_stops_an_infinite_loop() {
    let dir = scratch("wasm-epoch");
    let app = App::setup(WasmSource::with_limits(WasmLimits::new().timeout(Duration::from_secs(10)))).await;
    app.load(&write(&dir, "looping.wat", &looping()), json!({})).await;
    let looping = app.service("looping");
    looping.call("go", json!(null)).unwrap();
    // the caller's own deadline is shorter than the limit's
    let start = Instant::now();
    let error = looping.call_timeout("go", json!(null), Duration::from_millis(100)).unwrap_err();
    assert!(start.elapsed() < Duration::from_secs(5));
    assert_eq!(kind(&error), Some(GuestFailureKind::Timeout));
    let error = error.to_string();
    assert!(error.contains("interrupt"), "{error}");
    let error = looping.call("go", json!(null)).unwrap_err();
    assert_eq!(kind(&error), Some(GuestFailureKind::Timeout));
    assert!(error.to_string().starts_with("the guest was stopped"), "{error}");
}

#[tokio::test]
async fn caps_the_memory_of_an_instance() {
    let dir = scratch("wasm-memory");
    let growing = wat(
        json!({ "plugin": {} }),
        &[],
        r#"(func (export "cordis_apply") (param i32) (result i32)
    (drop (memory.grow (i32.const 32)))
    (i32.const 0))"#,
    );
    let path = write(&dir, "growing.wat", &growing);
    let app = App::setup(WasmSource::with_limits(WasmLimits::new().memory(1 << 20))).await;
    let entry = app.load(&path, json!({})).await;
    assert_eq!(fiber(&entry).state(), FiberState::Failed);
    let error = fiber(&entry).error().unwrap();
    assert_eq!(kind(&error), Some(GuestFailureKind::OutOfMemory));
    let error = error.to_string();
    assert!(error.contains("grow"), "{error}");

    let app = App::setup(WasmSource::new()).await;
    let entry = app.load(&path, json!({})).await;
    assert_eq!(fiber(&entry).state(), FiberState::Active);
}

/// The greeter with the `kardia_*` export names, importing the host
/// functions named in `kardia` from the module `kardia`.
fn renamed(kardia: &[&str]) -> String {
    let mut text = greeter("v1", greeter_meta()).replace("(export \"cordis_", "(export \"kardia_");
    for name in kardia {
        text = text.replace(&format!("(import \"cordis\" \"{name}\""), &format!("(import \"kardia\" \"{name}\""));
    }
    text
}

#[tokio::test]
async fn finds_the_kardia_names_of_exports_and_host_functions() {
    let dir = scratch("wasm-kardia-names");
    let app = App::setup(WasmSource::new()).await;
    let all = ["read", "write", "log", "provide", "view", "invoke"];
    let entry = app.load(&write(&dir, "greeter.wat", &renamed(&all)), json!({ "name": "alice" })).await;
    assert_eq!(fiber(&entry).state(), FiberState::Active);
    assert_eq!(app.view("version"), json!("v1"));
    let answer = app.service("greeter").call("hello", json!([1])).unwrap();
    assert_eq!(answer, json!({ "key": "greeter", "method": "hello", "args": [1] }));
    app.loader.remove(&entry.id()).unwrap();
    app.loader.settled().await;
    assert!(app.logs().contains(&"greeter v1 disposed".to_string()));
}

#[tokio::test]
async fn a_module_may_import_from_both_modules() {
    let dir = scratch("wasm-mixed-names");
    let app = App::setup(WasmSource::new()).await;
    let entry = app.load(&write(&dir, "greeter.wat", &renamed(&["read", "log"])), json!({})).await;
    assert_eq!(fiber(&entry).state(), FiberState::Active);
    assert!(app.logs().contains(&"greeter v1 applied".to_string()));
    assert_eq!(app.service("greeter").call("hello", json!(null)).unwrap()["method"], "hello");
}

#[tokio::test]
async fn prefers_the_kardia_name_when_a_module_exports_both() {
    let dir = scratch("wasm-both-names");
    let app = App::setup(WasmSource::new()).await;
    let both = wat(
        json!({ "plugin": { "provide": ["both"] } }),
        &["both", "{}", "kardia", "cordis"],
        r#"
  (func (export "cordis_apply") (param i32) (result i32) (drop (call $provide @0 @1)) (i32.const 0))
  (func (export "kardia_call") (param i32) (result i32) (call $write @2) (i32.const 1))
  (func (export "cordis_call") (param i32) (result i32) (call $write @3) (i32.const 1))"#,
    );
    let entry = app.load(&write(&dir, "both.wat", &both), json!({})).await;
    assert_eq!(fiber(&entry).state(), FiberState::Active);
    let error = app.service("both").call("hello", json!(null)).unwrap_err().to_string();
    assert_eq!(error, "kardia");
}

#[tokio::test]
async fn refuses_to_provide_an_undeclared_service() {
    let dir = scratch("wasm-undeclared-provide");
    let app = App::setup(WasmSource::new()).await;
    let body = r#"
  (func (export "cordis_apply") (param i32) (result i32)
    (local $result i32)
    (local.set $result (call $provide @0 @1))
    (call $read (i32.const 1024))
    (call $write (i32.const 1024) (i32.sub (i32.const -1) (local.get $result)))
    (i32.const 1))"#;
    let failing = wat(json!({ "plugin": { "provide": ["declared"] } }), &["undeclared", "{}"], body);
    let entry = app.load(&write(&dir, "failing.wat", &failing), json!({})).await;
    let error = fiber(&entry).error().unwrap().to_string();
    assert_eq!(error, r#"cannot provide "undeclared": the plugin does not declare it in provide"#);
    assert!(app.root.view("undeclared").is_err());

    // a guest that ignores the refusal runs, without the service
    let ignoring = wat(
        json!({ "plugin": { "provide": ["declared"] } }),
        &["undeclared", "{}", "declared"],
        r#"
  (func (export "cordis_apply") (param i32) (result i32)
    (drop (call $provide @0 @1))
    (drop (call $provide @2 @1))
    (i32.const 0))"#,
    );
    let entry = app.load(&write(&dir, "ignoring.wat", &ignoring), json!({})).await;
    assert_eq!(fiber(&entry).state(), FiberState::Active);
    assert!(app.root.view("undeclared").is_err());
    assert_eq!(app.view("declared"), json!({}));
}

#[tokio::test]
async fn runs_a_wasm_guest_with_metadata_from_outside() {
    let meta = GuestMeta::new("greeter").provide("greeter").provide("version");
    let guest = WasmGuest::new(greeter("v1", greeter_meta()), WasmLimits::new().fuel(1_000_000)).unwrap();
    let root = Context::new();
    let fiber = root.plugin_def(&guest.plugin(meta), json!({ "name": "alice" })).unwrap();
    fiber.settled().await.unwrap();
    assert_eq!(root.view("version").unwrap(), json!("v1"));
    let service = root.get_as::<GuestService>("greeter").unwrap();
    assert_eq!(service.call("hello", json!([1])).unwrap()["method"], "hello");

    // a source's guests share its engine
    let source = WasmSource::new();
    let impostor = source.guest(greeter("v1", json!({ "plugin": { "name": "impostor" } }))).unwrap();
    let fiber = root.plugin_def(&impostor.plugin(GuestMeta::new("greeter").provide("greeter")), json!({})).unwrap();
    let error = fiber.settled().await.unwrap_err().to_string();
    assert!(error.contains("impostor"), "{error}");
}

#[tokio::test]
async fn making_a_wasm_guest_and_its_plugin_instantiates_nothing() {
    // instantiating this module fails: it imports a function no host has
    let unlinkable = r#"(module (import "cordis" "missing" (func)) (memory (export "memory") 1))"#;
    let guest = WasmGuest::new(unlinkable, WasmLimits::new()).unwrap();
    let def = guest.plugin(GuestMeta::new("unlinkable"));
    let root = Context::new();
    let error = root.plugin_def(&def, json!({})).unwrap().settled().await.unwrap_err();
    assert!(error.to_string().contains("missing"), "{error}");
    assert_eq!(kind(&error), None);
    assert!(guest.module("unlinkable.wat").is_err());

    let error = WasmGuest::new("(module", WasmLimits::new()).err().unwrap();
    assert!(error.to_string().contains("expected"), "{error}");
}
