//! The guest ABI of `kardia-hmr` (see its crate docs), and the glue that
//! [`export!`](crate::export) calls into.
#![allow(unsafe_code)]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::{Error, Guest, Request, Result};

/// What a host offers a guest: the ABI's host calls, behind the transport
/// the guest runs on. A [`Host`] holds one, and may hand out a detached one.
pub trait HostCalls: Send + Sync {
    fn raw_read(&self, len: usize) -> Vec<u8>;
    fn raw_write(&self, bytes: &[u8]);
    fn raw_log(&self, level: i32, text: &[u8]);
    fn raw_provide(&self, key: &[u8], value: &[u8]) -> i32;
    fn raw_view(&self, key: &[u8]) -> i32;
    fn raw_invoke(&self, key: &[u8], method: &[u8], args: &[u8]) -> i32;

    /// A host that reaches the host from any thread, between exports.
    fn detached(&self) -> Arc<dyn HostCalls>;

    /// Whether [`detached`](HostCalls::detached) reaches the host outside an export.
    fn is_detached(&self) -> bool {
        false
    }

    /// Whether the host cancelled the export this host belongs to.
    fn cancelled(&self) -> bool {
        false
    }
}

/// A host that reaches nothing: the WASM and dylib backends have no channel
/// to their host outside an export, so a detached host there fails.
#[allow(dead_code)] // only the process transport hands one out
pub(crate) struct Unreachable;

#[allow(dead_code)]
pub(crate) fn unreachable() -> Unreachable {
    Unreachable
}

impl HostCalls for Unreachable {
    fn raw_read(&self, _: usize) -> Vec<u8> {
        Self::MESSAGE.to_vec()
    }

    fn raw_write(&self, _: &[u8]) {}

    fn raw_log(&self, _: i32, _: &[u8]) {}

    fn raw_provide(&self, _: &[u8], _: &[u8]) -> i32 {
        Self::REFUSED
    }

    fn raw_view(&self, _: &[u8]) -> i32 {
        Self::REFUSED
    }

    fn raw_invoke(&self, _: &[u8], _: &[u8], _: &[u8]) -> i32 {
        Self::REFUSED
    }

    fn detached(&self) -> Arc<dyn HostCalls> {
        Arc::new(Unreachable)
    }

    fn is_detached(&self) -> bool {
        true
    }
}

impl Unreachable {
    #[allow(dead_code)]
    const MESSAGE: &'static [u8] = b"this guest has no channel to its host outside an export";

    /// The status of a host call that cannot reach its host: an error.
    #[allow(dead_code)]
    const REFUSED: i32 = -1 - Self::MESSAGE.len() as i32;
}

/// The host side of the running export, for host calls.
#[derive(Clone)]
pub struct Host {
    pub(crate) api: Arc<dyn HostCalls>,
}

impl Host {
    pub(crate) fn new(api: Arc<dyn HostCalls>) -> Self {
        Self { api }
    }

    pub(crate) fn raw_read(&self, len: usize) -> Vec<u8> {
        self.api.raw_read(len)
    }

    pub(crate) fn raw_write(&self, bytes: &[u8]) {
        self.api.raw_write(bytes)
    }

    pub(crate) fn raw_log(&self, level: i32, text: &[u8]) {
        self.api.raw_log(level, text)
    }

    pub(crate) fn raw_provide(&self, key: &[u8], value: &[u8]) -> i32 {
        self.api.raw_provide(key, value)
    }

    pub(crate) fn raw_view(&self, key: &[u8]) -> i32 {
        self.api.raw_view(key)
    }

    pub(crate) fn raw_invoke(&self, key: &[u8], method: &[u8], args: &[u8]) -> i32 {
        self.api.raw_invoke(key, method, args)
    }

    pub(crate) fn detach(&self) -> Host {
        Host { api: self.api.detached() }
    }

    pub(crate) fn reaches_outside(&self) -> bool {
        self.api.is_detached()
    }

    pub(crate) fn was_cancelled(&self) -> bool {
        self.api.cancelled()
    }
}

#[cfg(target_family = "wasm")]
mod imports {
    #[link(wasm_import_module = "cordis")]
    unsafe extern "C" {
        pub fn read(ptr: *mut u8);
        pub fn write(ptr: *const u8, len: usize);
        pub fn log(level: i32, ptr: *const u8, len: usize);
        pub fn provide(key: *const u8, key_len: usize, value: *const u8, value_len: usize) -> i32;
        pub fn view(key: *const u8, key_len: usize) -> i32;
        pub fn invoke(
            key: *const u8,
            key_len: usize,
            method: *const u8,
            method_len: usize,
            args: *const u8,
            args_len: usize,
        ) -> i32;
    }
}

/// The host functions of a dylib guest; mirrors `HostApi` in `kardia-hmr`.
#[cfg(not(target_family = "wasm"))]
#[repr(C)]
pub struct HostApi {
    pub(crate) host: *mut std::ffi::c_void,
    pub(crate) read: unsafe extern "C" fn(*mut std::ffi::c_void, *mut u8),
    pub(crate) write: unsafe extern "C" fn(*mut std::ffi::c_void, *const u8, usize),
    pub(crate) log: unsafe extern "C" fn(*mut std::ffi::c_void, i32, *const u8, usize),
    pub(crate) provide: unsafe extern "C" fn(*mut std::ffi::c_void, *const u8, usize, *const u8, usize) -> i32,
    pub(crate) view: unsafe extern "C" fn(*mut std::ffi::c_void, *const u8, usize) -> i32,
    pub(crate) invoke:
        unsafe extern "C" fn(*mut std::ffi::c_void, *const u8, usize, *const u8, usize, *const u8, usize) -> i32,
}

// SAFETY (all host calls): the host implements the ABI and outlives the
// export that handed out this `Host`; slices are valid for their lengths.
#[cfg(target_family = "wasm")]
struct WasmHost;

#[cfg(target_family = "wasm")]
impl HostCalls for WasmHost {
    fn raw_read(&self, len: usize) -> Vec<u8> {
        let mut buffer = vec![0; len];
        unsafe { imports::read(buffer.as_mut_ptr()) };
        buffer
    }

    fn raw_write(&self, bytes: &[u8]) {
        unsafe { imports::write(bytes.as_ptr(), bytes.len()) }
    }

    fn raw_log(&self, level: i32, text: &[u8]) {
        unsafe { imports::log(level, text.as_ptr(), text.len()) }
    }

    fn raw_provide(&self, key: &[u8], value: &[u8]) -> i32 {
        unsafe { imports::provide(key.as_ptr(), key.len(), value.as_ptr(), value.len()) }
    }

    fn raw_view(&self, key: &[u8]) -> i32 {
        unsafe { imports::view(key.as_ptr(), key.len()) }
    }

    fn raw_invoke(&self, key: &[u8], method: &[u8], args: &[u8]) -> i32 {
        let (key, method, args) =
            ((key.as_ptr(), key.len()), (method.as_ptr(), method.len()), (args.as_ptr(), args.len()));
        unsafe { imports::invoke(key.0, key.1, method.0, method.1, args.0, args.1) }
    }

    fn detached(&self) -> Arc<dyn HostCalls> {
        Arc::new(WasmHost)
    }
}

#[cfg(not(target_family = "wasm"))]
struct DylibHost {
    host: *mut std::ffi::c_void,
    api: *const HostApi,
}

// SAFETY: the host's `HostApi` and what its `host` points to are valid for
// the export that handed out this host, and every raw call stays in that export.
#[cfg(not(target_family = "wasm"))]
unsafe impl Send for DylibHost {}
#[cfg(not(target_family = "wasm"))]
unsafe impl Sync for DylibHost {}

#[cfg(not(target_family = "wasm"))]
impl HostCalls for DylibHost {
    fn raw_read(&self, len: usize) -> Vec<u8> {
        let mut buffer = vec![0; len];
        unsafe { ((*self.api).read)(self.host, buffer.as_mut_ptr()) };
        buffer
    }

    fn raw_write(&self, bytes: &[u8]) {
        unsafe { ((*self.api).write)(self.host, bytes.as_ptr(), bytes.len()) }
    }

    fn raw_log(&self, level: i32, text: &[u8]) {
        unsafe { ((*self.api).log)(self.host, level, text.as_ptr(), text.len()) }
    }

    fn raw_provide(&self, key: &[u8], value: &[u8]) -> i32 {
        unsafe { ((*self.api).provide)(self.host, key.as_ptr(), key.len(), value.as_ptr(), value.len()) }
    }

    fn raw_view(&self, key: &[u8]) -> i32 {
        unsafe { ((*self.api).view)(self.host, key.as_ptr(), key.len()) }
    }

    fn raw_invoke(&self, key: &[u8], method: &[u8], args: &[u8]) -> i32 {
        let api = self.api;
        unsafe {
            ((*api).invoke)(
                self.host,
                key.as_ptr(),
                key.len(),
                method.as_ptr(),
                method.len(),
                args.as_ptr(),
                args.len(),
            )
        }
    }

    fn detached(&self) -> Arc<dyn HostCalls> {
        Arc::new(Unreachable)
    }
}

// The exports, shared by both targets. An instance lives in a slot that is
// filled by `apply` and emptied by `dispose`.

pub(crate) fn meta<G: Guest>(host: Host) -> i32 {
    finish(&host, guard(|| Ok(Some(serde_json::to_vec(&G::meta())?))))
}

#[derive(Deserialize)]
struct Input {
    #[serde(default)]
    config: Value,
    #[serde(default)]
    imports: Map<String, Value>,
}

pub(crate) fn apply<G: Guest>(host: Host, len: usize, slot: &mut Option<G>) -> i32 {
    finish(
        &host,
        guard(|| {
            let input: Input = serde_json::from_slice(&host.raw_read(len))?;
            *slot = Some(G::apply(&host, decode(input.config)?, input.imports)?);
            Ok(None)
        }),
    )
}

pub(crate) fn call<G: Guest>(host: Host, len: usize, slot: &mut Option<G>) -> i32 {
    finish(
        &host,
        guard(|| {
            let request: Request = serde_json::from_slice(&host.raw_read(len))?;
            let guest = slot.as_ref().ok_or_else(|| Error::msg("the guest has not been applied"))?;
            Ok(Some(serde_json::to_vec(&guest.call(&host, request)?)?))
        }),
    )
}

/// Answers a call on a guest shared between threads (a process guest under
/// `kardia-guest/3`); the status and output conventions are those of `call`.
#[allow(dead_code)] // only the process transport shares its guest
pub(crate) fn call_shared<G: Guest>(host: Host, len: usize, guest: &G) -> i32 {
    finish(
        &host,
        guard(|| {
            let request: Request = serde_json::from_slice(&host.raw_read(len))?;
            Ok(Some(serde_json::to_vec(&guest.call(&host, request)?)?))
        }),
    )
}

pub(crate) fn dispose<G: Guest>(host: Host, slot: Option<G>) {
    if let Some(guest) = slot {
        // a panic is contained; the host has nothing to report it to
        drop(guard(|| {
            guest.dispose(&host);
            Ok(())
        }));
    }
}

/// Like the host's config decoding: `null` falls back to `{}`.
fn decode<C: DeserializeOwned>(config: Value) -> Result<C> {
    match serde_json::from_value(config.clone()) {
        Err(_) if config.is_null() => Ok(serde_json::from_value(Value::Object(Map::new()))?),
        result => Ok(result?),
    }
}

/// Writes the output or the error; returns the export's status.
fn finish(host: &Host, result: Result<Option<Vec<u8>>>) -> i32 {
    match result {
        Ok(output) => {
            if let Some(output) = output {
                host.raw_write(&output);
            }
            0
        }
        Err(error) => {
            host.raw_write(error.message().as_bytes());
            1
        }
    }
}

/// A panic must not unwind into the host: it becomes the export's error.
fn guard<T>(body: impl FnOnce() -> Result<T>) -> Result<T> {
    catch_unwind(AssertUnwindSafe(body)).unwrap_or_else(|panic| {
        let message = panic
            .downcast_ref::<&str>()
            .map(|text| text.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "the guest panicked".into());
        Err(Error::msg(format!("the guest panicked: {message}")))
    })
}

#[cfg(target_family = "wasm")]
mod wasm {
    use std::any::Any;
    use std::cell::RefCell;
    use std::sync::Arc;

    use super::*;

    thread_local! {
        // a WASM instance runs one plugin instance
        static INSTANCE: RefCell<Option<Box<dyn Any>>> = const { RefCell::new(None) };
    }

    fn take<G: Guest>() -> Option<G> {
        INSTANCE.take().and_then(|instance| instance.downcast::<G>().ok()).map(|guest| *guest)
    }

    fn put<G: Guest>(slot: Option<G>) {
        INSTANCE.set(slot.map(|guest| Box::new(guest) as Box<dyn Any>));
    }

    fn host() -> Host {
        Host::new(Arc::new(WasmHost))
    }

    pub fn wasm_meta<G: Guest>() -> i32 {
        meta::<G>(host())
    }

    pub fn wasm_apply<G: Guest>(len: usize) -> i32 {
        let mut slot = None;
        let status = apply::<G>(host(), len, &mut slot);
        put(slot);
        status
    }

    pub fn wasm_call<G: Guest>(len: usize) -> i32 {
        let mut slot = take::<G>();
        let status = call::<G>(host(), len, &mut slot);
        put(slot);
        status
    }

    pub fn wasm_dispose<G: Guest>() {
        dispose::<G>(host(), take::<G>());
    }
}

#[cfg(target_family = "wasm")]
pub use wasm::*;

#[cfg(not(target_family = "wasm"))]
mod dylib {
    use std::sync::Arc;

    use super::*;

    /// A boxed `Option<G>`, owned by the host between `cordis_new` and `cordis_dispose`.
    pub type State = *mut std::ffi::c_void;

    pub fn dylib_new<G: Guest>() -> State {
        Box::into_raw(Box::new(None::<G>)).cast()
    }

    /// # Safety
    /// `host` is the host's `HostApi` for this call.
    pub unsafe fn dylib_meta<G: Guest>(host: *const HostApi) -> i32 {
        // SAFETY: the caller passes the host's `HostApi` for this call.
        unsafe { meta::<G>(Host::new(Arc::new(DylibHost { host: (*host).host, api: host }))) }
    }

    /// # Safety
    /// `state` comes from `dylib_new::<G>` and is not disposed.
    pub unsafe fn dylib_apply<G: Guest>(state: State, host: *const HostApi, len: usize) -> i32 {
        // SAFETY: `host` is valid for this call; `state` comes from `dylib_new` and is not disposed.
        unsafe {
            let host = Host::new(Arc::new(DylibHost { host: (*host).host, api: host }));
            apply::<G>(host, len, &mut *state.cast::<Option<G>>())
        }
    }

    /// # Safety
    /// As for [`dylib_apply`].
    pub unsafe fn dylib_call<G: Guest>(state: State, host: *const HostApi, len: usize) -> i32 {
        // SAFETY: as for `dylib_apply`.
        unsafe {
            let host = Host::new(Arc::new(DylibHost { host: (*host).host, api: host }));
            call::<G>(host, len, &mut *state.cast::<Option<G>>())
        }
    }

    /// # Safety
    /// As for [`dylib_apply`]; `state` is not used afterwards.
    pub unsafe fn dylib_dispose<G: Guest>(state: State, host: *const HostApi) {
        // SAFETY: as for `dylib_apply`; `state` is not used afterwards.
        unsafe {
            let slot = Box::from_raw(state.cast::<Option<G>>());
            dispose::<G>(Host::new(Arc::new(DylibHost { host: (*host).host, api: host })), *slot);
        }
    }
}

#[cfg(not(target_family = "wasm"))]
pub use dylib::*;
