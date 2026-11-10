//! How wasmtime handles traps on macOS: a process-wide setting, so this
//! suite runs in a process of its own, setting it before any engine exists.

use kardia_core::Context;
use kardia_hmr::{GuestFailure, GuestFailureKind, GuestMeta, WasmGuest, WasmLimits, WasmSource, set_macos_mach_ports};
use serde_json::json;

/// A plugin whose apply traps.
const TRAPPING: &str = r#"(module
  (import "cordis" "write" (func $write (param i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "{\"plugin\":{}}")
  (func (export "cordis_meta") (result i32) (call $write (i32.const 0) (i32.const 13)) (i32.const 0))
  (func (export "cordis_apply") (param i32) (result i32) unreachable))"#;

#[tokio::test]
async fn signals_instead_of_mach_ports_are_fixed_at_the_first_engine() {
    set_macos_mach_ports(true).unwrap();
    set_macos_mach_ports(false).unwrap();
    let guest = WasmGuest::new(TRAPPING, WasmLimits::new()).unwrap();
    // the trap is handled with the setting the engine was made with
    let root = Context::new();
    let error = root.plugin_def(&guest.plugin(GuestMeta::default()), json!({})).unwrap().settled().await.unwrap_err();
    assert_eq!(GuestFailure::of(&error).unwrap().kind, GuestFailureKind::Trap, "{error}");

    set_macos_mach_ports(false).unwrap();
    let error = set_macos_mach_ports(true).unwrap_err().to_string();
    assert!(error.contains("macos_use_mach_ports"), "{error}");
    // later engines keep the setting
    let guest = WasmSource::new().guest(TRAPPING).unwrap();
    let error = root.plugin_def(&guest.plugin(GuestMeta::default()), json!({})).unwrap().settled().await.unwrap_err();
    assert_eq!(GuestFailure::of(&error).unwrap().kind, GuestFailureKind::Trap, "{error}");
}
