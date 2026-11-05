//! The guest ABI of `cordis-hmr` (see its crate docs), and the glue that
//! [`export!`](crate::export) calls into.
#![allow(unsafe_code)]

use std::panic::{AssertUnwindSafe, catch_unwind};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::{Error, Guest, Request, Result};

/// The host side of the running export, for host calls.
#[derive(Clone, Copy)]
pub struct Host {
    #[cfg(not(target_family = "wasm"))]
    api: *const HostApi,
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

/// The host functions of a dylib guest; mirrors `HostApi` in `cordis-hmr`.
#[cfg(not(target_family = "wasm"))]
#[repr(C)]
pub struct HostApi {
    host: *mut std::ffi::c_void,
    read: unsafe extern "C" fn(*mut std::ffi::c_void, *mut u8),
    write: unsafe extern "C" fn(*mut std::ffi::c_void, *const u8, usize),
    log: unsafe extern "C" fn(*mut std::ffi::c_void, i32, *const u8, usize),
    provide: unsafe extern "C" fn(*mut std::ffi::c_void, *const u8, usize, *const u8, usize) -> i32,
    view: unsafe extern "C" fn(*mut std::ffi::c_void, *const u8, usize) -> i32,
    invoke: unsafe extern "C" fn(*mut std::ffi::c_void, *const u8, usize, *const u8, usize, *const u8, usize) -> i32,
}

// SAFETY (all host calls): the host implements the ABI and outlives the
// export that handed out this `Host`; slices are valid for their lengths.
#[cfg(target_family = "wasm")]
impl Host {
    pub(crate) fn raw_read(&self, len: usize) -> Vec<u8> {
        let mut buffer = vec![0; len];
        unsafe { imports::read(buffer.as_mut_ptr()) };
        buffer
    }

    pub(crate) fn raw_write(&self, bytes: &[u8]) {
        unsafe { imports::write(bytes.as_ptr(), bytes.len()) }
    }

    pub(crate) fn raw_log(&self, level: i32, text: &[u8]) {
        unsafe { imports::log(level, text.as_ptr(), text.len()) }
    }

    pub(crate) fn raw_provide(&self, key: &[u8], value: &[u8]) -> i32 {
        unsafe { imports::provide(key.as_ptr(), key.len(), value.as_ptr(), value.len()) }
    }

    pub(crate) fn raw_view(&self, key: &[u8]) -> i32 {
        unsafe { imports::view(key.as_ptr(), key.len()) }
    }

    pub(crate) fn raw_invoke(&self, key: &[u8], method: &[u8], args: &[u8]) -> i32 {
        let (key, method, args) =
            ((key.as_ptr(), key.len()), (method.as_ptr(), method.len()), (args.as_ptr(), args.len()));
        unsafe { imports::invoke(key.0, key.1, method.0, method.1, args.0, args.1) }
    }
}

#[cfg(not(target_family = "wasm"))]
impl Host {
    fn api(&self) -> &HostApi {
        unsafe { &*self.api }
    }

    pub(crate) fn raw_read(&self, len: usize) -> Vec<u8> {
        let mut buffer = vec![0; len];
        unsafe { (self.api().read)(self.api().host, buffer.as_mut_ptr()) };
        buffer
    }

    pub(crate) fn raw_write(&self, bytes: &[u8]) {
        unsafe { (self.api().write)(self.api().host, bytes.as_ptr(), bytes.len()) }
    }

    pub(crate) fn raw_log(&self, level: i32, text: &[u8]) {
        unsafe { (self.api().log)(self.api().host, level, text.as_ptr(), text.len()) }
    }

    pub(crate) fn raw_provide(&self, key: &[u8], value: &[u8]) -> i32 {
        unsafe { (self.api().provide)(self.api().host, key.as_ptr(), key.len(), value.as_ptr(), value.len()) }
    }

    pub(crate) fn raw_view(&self, key: &[u8]) -> i32 {
        unsafe { (self.api().view)(self.api().host, key.as_ptr(), key.len()) }
    }

    pub(crate) fn raw_invoke(&self, key: &[u8], method: &[u8], args: &[u8]) -> i32 {
        let api = self.api();
        unsafe {
            (api.invoke)(api.host, key.as_ptr(), key.len(), method.as_ptr(), method.len(), args.as_ptr(), args.len())
        }
    }
}

// The exports, shared by both targets. An instance lives in a slot that is
// filled by `apply` and emptied by `dispose`.

fn meta<G: Guest>(host: Host) -> i32 {
    finish(host, guard(|| Ok(Some(serde_json::to_vec(&G::meta())?))))
}

#[derive(Deserialize)]
struct Input {
    #[serde(default)]
    config: Value,
    #[serde(default)]
    imports: Map<String, Value>,
}

fn apply<G: Guest>(host: Host, len: usize, slot: &mut Option<G>) -> i32 {
    finish(
        host,
        guard(|| {
            let input: Input = serde_json::from_slice(&host.raw_read(len))?;
            *slot = Some(G::apply(&host, decode(input.config)?, input.imports)?);
            Ok(None)
        }),
    )
}

fn call<G: Guest>(host: Host, len: usize, slot: &mut Option<G>) -> i32 {
    finish(
        host,
        guard(|| {
            let request: Request = serde_json::from_slice(&host.raw_read(len))?;
            let guest = slot.as_mut().ok_or_else(|| Error::msg("the guest has not been applied"))?;
            Ok(Some(serde_json::to_vec(&guest.call(&host, request)?)?))
        }),
    )
}

fn dispose<G: Guest>(host: Host, slot: Option<G>) {
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
fn finish(host: Host, result: Result<Option<Vec<u8>>>) -> i32 {
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

    pub fn wasm_meta<G: Guest>() -> i32 {
        meta::<G>(Host {})
    }

    pub fn wasm_apply<G: Guest>(len: usize) -> i32 {
        let mut slot = None;
        let status = apply::<G>(Host {}, len, &mut slot);
        put(slot);
        status
    }

    pub fn wasm_call<G: Guest>(len: usize) -> i32 {
        let mut slot = take::<G>();
        let status = call::<G>(Host {}, len, &mut slot);
        put(slot);
        status
    }

    pub fn wasm_dispose<G: Guest>() {
        dispose::<G>(Host {}, take::<G>());
    }
}

#[cfg(target_family = "wasm")]
pub use wasm::*;

#[cfg(not(target_family = "wasm"))]
mod dylib {
    use super::*;

    /// A boxed `Option<G>`, owned by the host between `cordis_new` and `cordis_dispose`.
    pub type State = *mut std::ffi::c_void;

    pub fn dylib_new<G: Guest>() -> State {
        Box::into_raw(Box::new(None::<G>)).cast()
    }

    /// # Safety
    /// `host` is the host's `HostApi` for this call.
    pub unsafe fn dylib_meta<G: Guest>(host: *const HostApi) -> i32 {
        meta::<G>(Host { api: host })
    }

    /// # Safety
    /// `state` comes from `dylib_new::<G>` and is not disposed.
    pub unsafe fn dylib_apply<G: Guest>(state: State, host: *const HostApi, len: usize) -> i32 {
        apply::<G>(Host { api: host }, len, unsafe { &mut *state.cast::<Option<G>>() })
    }

    /// # Safety
    /// As for [`dylib_apply`].
    pub unsafe fn dylib_call<G: Guest>(state: State, host: *const HostApi, len: usize) -> i32 {
        call::<G>(Host { api: host }, len, unsafe { &mut *state.cast::<Option<G>>() })
    }

    /// # Safety
    /// As for [`dylib_apply`]; `state` is not used afterwards.
    pub unsafe fn dylib_dispose<G: Guest>(state: State, host: *const HostApi) {
        let slot = unsafe { Box::from_raw(state.cast::<Option<G>>()) };
        dispose::<G>(Host { api: host }, *slot);
    }
}

#[cfg(not(target_family = "wasm"))]
pub use dylib::*;

#[cfg(not(target_family = "wasm"))]
mod process {
    use std::ffi::c_void;
    use std::io::{BufRead, Lines, StdinLock, Write};

    use serde_json::json;

    use super::*;

    /// Serves `G` as a process guest: answers the exports the host sends as
    /// JSON lines on stdin, until it disposes the instance or stdin closes.
    /// stdout carries the protocol, so the guest must not print to it; log
    /// through [`Host::log`] or stderr.
    pub fn serve<G: Guest>() {
        let mut channel = Channel { lines: std::io::stdin().lock().lines(), message: Vec::new() };
        let mut slot: Option<G> = None;
        while let Some(request) = channel.receive() {
            let export = request["export"].as_str().unwrap_or_default().to_string();
            channel.message = match &request["input"] {
                Value::Null => Vec::new(),
                input => serde_json::to_vec(input).expect("a JSON value"),
            };
            let len = channel.message.len();
            let api = channel.api();
            let host = Host { api: &api };
            let status = match export.as_str() {
                "meta" => meta::<G>(host),
                "apply" => apply::<G>(host, len, &mut slot),
                "call" => call::<G>(host, len, &mut slot),
                "dispose" => {
                    dispose::<G>(host, slot.take());
                    send(&json!({ "done": 0 }));
                    return;
                }
                _ => 1,
            };
            send(&json!({ "done": status }));
        }
    }

    /// The host side of the pipe, behind the `HostApi` the glue calls.
    struct Channel {
        lines: Lines<StdinLock<'static>>,
        /// The input of the current export, then the result of the last host call.
        message: Vec<u8>,
    }

    impl Channel {
        fn receive(&mut self) -> Option<Value> {
            serde_json::from_str(&self.lines.next()?.ok()?).ok()
        }

        fn api(&mut self) -> HostApi {
            HostApi {
                host: std::ptr::from_mut(self).cast(),
                read: channel_read,
                write: channel_write,
                log: channel_log,
                provide: channel_provide,
                view: channel_view,
                invoke: channel_invoke,
            }
        }

        /// A host call: stores its result (or error) as the message and
        /// returns the length, or `-1 - len` for an error.
        fn request(&mut self, request: Value) -> i32 {
            send(&request);
            // the host is gone, and with it everything this guest serves
            let reply = self.receive().unwrap_or_else(|| std::process::exit(1));
            match reply.get("error") {
                Some(error) => {
                    self.message = error.as_str().unwrap_or_default().as_bytes().to_vec();
                    -1 - self.message.len() as i32
                }
                None => {
                    self.message = match reply.get("result") {
                        None | Some(Value::Null) => Vec::new(),
                        Some(result) => serde_json::to_vec(result).expect("a JSON value"),
                    };
                    self.message.len() as i32
                }
            }
        }
    }

    fn send(message: &Value) {
        let mut stdout = std::io::stdout().lock();
        if writeln!(stdout, "{message}").and_then(|()| stdout.flush()).is_err() {
            std::process::exit(1);
        }
    }

    // SAFETY (all of them): `host` is the `Channel` of the running export,
    // and the glue passes slices that are valid for their lengths.

    unsafe fn channel<'a>(host: *mut c_void) -> &'a mut Channel {
        unsafe { &mut *host.cast::<Channel>() }
    }

    unsafe fn text<'a>(ptr: *const u8, len: usize) -> &'a str {
        let bytes = if len == 0 { &[][..] } else { unsafe { std::slice::from_raw_parts(ptr, len) } };
        std::str::from_utf8(bytes).unwrap_or_default()
    }

    unsafe fn value(ptr: *const u8, len: usize) -> Value {
        serde_json::from_str(unsafe { text(ptr, len) }).unwrap_or(Value::Null)
    }

    unsafe extern "C" fn channel_read(host: *mut c_void, ptr: *mut u8) {
        let message = &unsafe { channel(host) }.message;
        unsafe { std::ptr::copy_nonoverlapping(message.as_ptr(), ptr, message.len()) };
    }

    unsafe extern "C" fn channel_write(_: *mut c_void, ptr: *const u8, len: usize) {
        send(&json!({ "output": unsafe { text(ptr, len) } }));
    }

    unsafe extern "C" fn channel_log(_: *mut c_void, level: i32, ptr: *const u8, len: usize) {
        send(&json!({ "call": "log", "level": level, "text": unsafe { text(ptr, len) } }));
    }

    unsafe extern "C" fn channel_provide(
        host: *mut c_void,
        key: *const u8,
        key_len: usize,
        value_ptr: *const u8,
        value_len: usize,
    ) -> i32 {
        let request = json!({ "call": "provide", "key": unsafe { text(key, key_len) }, "value": unsafe { value(value_ptr, value_len) } });
        unsafe { channel(host) }.request(request)
    }

    unsafe extern "C" fn channel_view(host: *mut c_void, key: *const u8, key_len: usize) -> i32 {
        unsafe { channel(host) }.request(json!({ "call": "view", "key": unsafe { text(key, key_len) } }))
    }

    unsafe extern "C" fn channel_invoke(
        host: *mut c_void,
        key: *const u8,
        key_len: usize,
        method: *const u8,
        method_len: usize,
        args: *const u8,
        args_len: usize,
    ) -> i32 {
        let request = json!({
            "call": "invoke",
            "key": unsafe { text(key, key_len) },
            "method": unsafe { text(method, method_len) },
            "args": unsafe { value(args, args_len) },
        });
        unsafe { channel(host) }.request(request)
    }
}

#[cfg(not(target_family = "wasm"))]
pub use process::serve;
