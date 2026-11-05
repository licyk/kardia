//! Dynamic libraries as guest modules, for development: same machine, no
//! sandbox, but no WASM toolchain either.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::{fs, slice};

use cordis_core::{Error, Result};
use libloading::Library;

use crate::graph::{ModuleCode, ModuleSource};
use crate::guest::{self, Code, Export, Host, Instance};

/// Loads dynamic libraries exporting the C guest ABI; module ids are their
/// paths.
///
/// Each load copies the library to a fresh path, because the platform loader
/// hands out the library it already loaded for a known path. Libraries are
/// never unloaded: unloading Rust code is unsound once it registered
/// thread-local destructors or handed out `'static` references. Guests run
/// in the host process, so a crashing guest crashes the host.
#[derive(Default)]
pub struct DylibSource;

impl ModuleSource for DylibSource {
    fn load(&self, id: &str) -> Result<ModuleCode> {
        guest::module(id, Arc::new(DylibCode(open(Path::new(id))?)))
    }
}

static COPIES: AtomicU64 = AtomicU64::new(0);

fn open(path: &Path) -> Result<Symbols> {
    let name = path.file_name().ok_or_else(|| Error::msg(format!("{} is not a file", path.display())))?;
    let dir = std::env::temp_dir().join(format!("cordis-dylib-{}", std::process::id()));
    fs::create_dir_all(&dir)?;
    let copy = dir.join(format!("{}-{}", COPIES.fetch_add(1, Ordering::Relaxed), name.to_string_lossy()));
    fs::copy(path, &copy)?;
    // SAFETY: loading runs the library's initializers; trusting them is what
    // loading a dylib guest means.
    let library = unsafe { Library::new(&copy) }?;
    // the mapping outlives the file on Unix; elsewhere the copy stays behind
    let _ = fs::remove_file(&copy);
    // SAFETY: the guest declares these symbols with the documented signatures.
    let symbols = unsafe {
        Symbols {
            new: *library.get(b"cordis_new\0")?,
            meta: *library.get(b"cordis_meta\0")?,
            apply: *library.get(b"cordis_apply\0")?,
            call: *library.get(b"cordis_call\0")?,
            dispose: *library.get(b"cordis_dispose\0")?,
        }
    };
    std::mem::forget(library);
    Ok(symbols)
}

type State = *mut c_void;

#[derive(Clone, Copy)]
struct Symbols {
    new: unsafe extern "C" fn() -> State,
    meta: unsafe extern "C" fn(State, *const HostApi) -> i32,
    apply: unsafe extern "C" fn(State, *const HostApi, usize) -> i32,
    call: unsafe extern "C" fn(State, *const HostApi, usize) -> i32,
    dispose: unsafe extern "C" fn(State, *const HostApi),
}

/// The host functions, passed to every export (`cordis_host` in the ABI).
#[repr(C)]
struct HostApi {
    host: *mut c_void,
    read: unsafe extern "C" fn(*mut c_void, *mut u8),
    write: unsafe extern "C" fn(*mut c_void, *const u8, usize),
    log: unsafe extern "C" fn(*mut c_void, i32, *const u8, usize),
    provide: unsafe extern "C" fn(*mut c_void, *const u8, usize, *const u8, usize) -> i32,
    view: unsafe extern "C" fn(*mut c_void, *const u8, usize) -> i32,
    invoke: unsafe extern "C" fn(*mut c_void, *const u8, usize, *const u8, usize, *const u8, usize) -> i32,
}

struct DylibCode(Symbols);

impl Code for DylibCode {
    fn instantiate(&self, host: Host) -> Result<Box<dyn Instance>> {
        // SAFETY: see `open`.
        let state = unsafe { (self.0.new)() };
        Ok(Box::new(DylibInstance { symbols: self.0, state, host: Box::new(host) }))
    }
}

struct DylibInstance {
    symbols: Symbols,
    state: State,
    host: Box<Host>,
}

// SAFETY: the guest state is only used by calls into the guest, which the
// owning slot's mutex serializes; guests must not tie it to a thread.
unsafe impl Send for DylibInstance {}

impl Instance for DylibInstance {
    fn host(&mut self) -> &mut Host {
        &mut self.host
    }

    fn call(&mut self, export: Export) -> Result<i32> {
        let len = self.host.message.len();
        let api = HostApi {
            host: std::ptr::from_mut::<Host>(&mut self.host).cast(),
            read,
            write,
            log,
            provide,
            view,
            invoke,
        };
        let (symbols, state) = (self.symbols, self.state);
        // SAFETY: see `open`; `api` and the host outlive the call, and
        // `Dispose` is the last call on `state`.
        Ok(unsafe {
            match export {
                Export::Meta => (symbols.meta)(state, &api),
                Export::Apply => (symbols.apply)(state, &api, len),
                Export::Call => (symbols.call)(state, &api, len),
                Export::Dispose => {
                    (symbols.dispose)(state, &api);
                    0
                }
            }
        })
    }
}

// The host functions trust the guest's pointers and lengths, like any C API.

unsafe extern "C" fn read(host: *mut c_void, ptr: *mut u8) {
    let host = unsafe { host_ref(host) };
    unsafe { std::ptr::copy_nonoverlapping(host.message.as_ptr(), ptr, host.message.len()) };
}

unsafe extern "C" fn write(host: *mut c_void, ptr: *const u8, len: usize) {
    unsafe { host_ref(host).write(bytes(ptr, len)) }
}

unsafe extern "C" fn log(host: *mut c_void, level: i32, ptr: *const u8, len: usize) {
    unsafe { host_ref(host).log(level, bytes(ptr, len)) }
}

unsafe extern "C" fn provide(
    host: *mut c_void,
    key: *const u8,
    key_len: usize,
    value: *const u8,
    value_len: usize,
) -> i32 {
    unsafe { host_ref(host).provide(bytes(key, key_len), bytes(value, value_len)) }
}

unsafe extern "C" fn view(host: *mut c_void, key: *const u8, key_len: usize) -> i32 {
    unsafe { host_ref(host).view(bytes(key, key_len)) }
}

unsafe extern "C" fn invoke(
    host: *mut c_void,
    key: *const u8,
    key_len: usize,
    method: *const u8,
    method_len: usize,
    args: *const u8,
    args_len: usize,
) -> i32 {
    unsafe { host_ref(host).invoke(bytes(key, key_len), bytes(method, method_len), bytes(args, args_len)) }
}

unsafe fn host_ref<'a>(host: *mut c_void) -> &'a mut Host {
    unsafe { &mut *host.cast::<Host>() }
}

unsafe fn bytes<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if len == 0 { &[] } else { unsafe { slice::from_raw_parts(ptr, len) } }
}
