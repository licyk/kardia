//! WASM guest modules on wasmtime. A module compiles once per version and
//! instantiates in its own store per fiber, so old and new versions run side
//! by side and dropping an instance releases all of its memory.

mod instance;
mod source;

pub use source::{WasmGuest, WasmLimits, WasmSource, set_macos_mach_ports};
