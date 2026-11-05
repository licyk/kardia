//! Provides `greeter`, whose view is `{ version, input }` and whose calls
//! answer `{ version, request }`.

use std::ffi::c_void;

const VERSION: &str = if cfg!(feature = "v2") { "v2" } else { "v1" };

#[repr(C)]
pub struct Host {
    host: *mut c_void,
    read: unsafe extern "C" fn(*mut c_void, *mut u8),
    write: unsafe extern "C" fn(*mut c_void, *const u8, usize),
    log: unsafe extern "C" fn(*mut c_void, i32, *const u8, usize),
    provide: unsafe extern "C" fn(*mut c_void, *const u8, usize, *const u8, usize) -> i32,
    #[allow(dead_code)]
    view: unsafe extern "C" fn(*mut c_void, *const u8, usize) -> i32,
    #[allow(dead_code)]
    invoke: unsafe extern "C" fn(*mut c_void, *const u8, usize, *const u8, usize, *const u8, usize) -> i32,
}

impl Host {
    fn read(&self, len: usize) -> String {
        let mut buffer = vec![0; len];
        unsafe { (self.read)(self.host, buffer.as_mut_ptr()) };
        String::from_utf8(buffer).unwrap()
    }

    fn write(&self, text: &str) {
        unsafe { (self.write)(self.host, text.as_ptr(), text.len()) }
    }

    fn log(&self, text: &str) {
        unsafe { (self.log)(self.host, 2, text.as_ptr(), text.len()) }
    }

    fn provide(&self, key: &str, value: &str) -> i32 {
        unsafe { (self.provide)(self.host, key.as_ptr(), key.len(), value.as_ptr(), value.len()) }
    }
}

pub struct State {
    calls: usize,
}

#[unsafe(no_mangle)]
pub extern "C" fn cordis_new() -> *mut State {
    Box::into_raw(Box::new(State { calls: 0 }))
}

#[unsafe(no_mangle)]
pub extern "C" fn cordis_meta(_: *mut State, host: &Host) -> i32 {
    host.write(r#"{ "plugin": { "name": "greeter", "provide": ["greeter"] } }"#);
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn cordis_apply(_: *mut State, host: &Host, len: usize) -> i32 {
    let input = host.read(len);
    if host.provide("greeter", &format!(r#"{{ "version": "{VERSION}", "input": {input} }}"#)) < 0 {
        host.write("cannot provide greeter");
        return 1;
    }
    host.log(&format!("greeter {VERSION} applied"));
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn cordis_call(state: *mut State, host: &Host, len: usize) -> i32 {
    let state = unsafe { &mut *state };
    state.calls += 1;
    let request = host.read(len);
    host.write(&format!(r#"{{ "version": "{VERSION}", "calls": {}, "request": {request} }}"#, state.calls));
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn cordis_dispose(state: *mut State, host: &Host) {
    drop(unsafe { Box::from_raw(state) });
    host.log(&format!("greeter {VERSION} disposed"));
}
