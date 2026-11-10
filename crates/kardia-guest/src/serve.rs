//! The process transport: [`serve`] answers the exports a host sends as JSON
//! lines on stdin, in protocol version 1 or `kardia-guest/2`.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::io::{BufRead, Lines, StdinLock, Write};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, Sender};

use serde_json::{Value, json};

use crate::Guest;
use crate::ffi::{self, Host, HostApi};
use crate::host::{CANCELLED, RUNNING};

const PROTOCOL: &str = "kardia-guest/2";

/// Serves `G` as a process guest: answers the exports the host sends as JSON
/// lines on stdin, until it disposes the instance or stdin closes. stdout
/// carries the protocol, so the guest must not print to it; log through
/// [`Host::log`] or stderr.
///
/// A host offering `kardia-guest/2` gets it: a thread then reads stdin, so
/// pings are answered while an export runs, and a cancelled export sees
/// [`Host::cancelled`]. Any other host is served protocol version 1.
pub fn serve<G: Guest>() {
    let mut lines = std::io::stdin().lock().lines();
    let Some(first) = next(&mut lines) else { return };
    if first["export"] != "hello" || first["input"]["protocol"] != PROTOCOL {
        return run::<G>(Channel { source: Source::Lines(lines), id: None, message: Vec::new() }, Some(first));
    }
    // the reader thread locks stdin itself; what this lock buffered stays in stdin's buffer
    drop(lines);
    let (sender, receiver) = mpsc::channel();
    if std::thread::Builder::new().name("kardia-guest-stdin".into()).spawn(move || read(&sender)).is_err() {
        std::process::exit(1);
    }
    send(&json!({ "ready": PROTOCOL }));
    run::<G>(Channel { source: Source::Queue(receiver), id: None, message: Vec::new() }, None);
}

fn run<G: Guest>(mut channel: Channel, mut first: Option<Value>) {
    let mut slot: Option<G> = None;
    while let Some(request) = first.take().or_else(|| channel.source.next()) {
        channel.id = request.get("id").and_then(Value::as_u64);
        RUNNING.store(channel.id.unwrap_or(0), Ordering::Release);
        let export = request["export"].as_str().unwrap_or_default().to_string();
        channel.message = match &request["input"] {
            Value::Null => Vec::new(),
            input => serde_json::to_vec(input).expect("a JSON value"),
        };
        let len = channel.message.len();
        let api = channel.api();
        let host = Host { api: &api };
        let status = match export.as_str() {
            "meta" => ffi::meta::<G>(host),
            "apply" => ffi::apply::<G>(host, len, &mut slot),
            "call" => ffi::call::<G>(host, len, &mut slot),
            "dispose" => {
                ffi::dispose::<G>(host, slot.take());
                channel.send(json!({ "done": 0 }));
                return;
            }
            _ => 1,
        };
        channel.send(json!({ "done": status }));
        RUNNING.store(0, Ordering::Release);
    }
}

/// Reads stdin for `kardia-guest/2`: answers pings and records cancellations
/// at once, and queues the rest.
fn read(sender: &Sender<Value>) {
    let mut lines = std::io::stdin().lock().lines();
    while let Some(message) = next(&mut lines) {
        if let Some(id) = message.get("ping") {
            send(&json!({ "pong": id }));
        } else if let Some(id) = message.get("cancel").and_then(Value::as_u64) {
            CANCELLED.store(id, Ordering::Release);
        } else if sender.send(message).is_err() {
            return;
        }
    }
}

fn next(lines: &mut Lines<StdinLock<'static>>) -> Option<Value> {
    serde_json::from_str(&lines.next()?.ok()?).ok()
}

/// Writes one message; the host is gone if it cannot.
fn send(message: &Value) {
    let mut stdout = std::io::stdout().lock();
    if writeln!(stdout, "{message}").and_then(|()| stdout.flush()).is_err() {
        std::process::exit(1);
    }
}

enum Source {
    /// Version 1: the lines of stdin.
    Lines(Lines<StdinLock<'static>>),
    /// `kardia-guest/2`: what the reader thread queued.
    Queue(Receiver<Value>),
}

impl Source {
    fn next(&mut self) -> Option<Value> {
        match self {
            Source::Lines(lines) => next(lines),
            Source::Queue(queue) => queue.recv().ok(),
        }
    }
}

/// The host side of the pipe, behind the `HostApi` the glue calls.
struct Channel {
    source: Source,
    /// The id of the running export, under `kardia-guest/2`.
    id: Option<u64>,
    /// The input of the current export, then the result of the last host call.
    message: Vec<u8>,
}

impl Channel {
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

    /// Sends a message of the running export.
    fn send(&self, mut message: Value) {
        if let Some(id) = self.id {
            message["id"] = json!(id);
        }
        send(&message);
    }

    /// A host call: stores its result (or error) as the message and returns
    /// the length, or `-1 - len` for an error.
    fn request(&mut self, request: Value) -> i32 {
        self.send(request);
        // the host is gone, and with it everything this guest serves
        let reply = self.source.next().unwrap_or_else(|| std::process::exit(1));
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

// SAFETY (all of them): `host` is the `Channel` of the running export, and
// the glue passes slices that are valid for their lengths.

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

unsafe extern "C" fn channel_write(host: *mut c_void, ptr: *const u8, len: usize) {
    unsafe { channel(host) }.send(json!({ "output": unsafe { text(ptr, len) } }));
}

unsafe extern "C" fn channel_log(host: *mut c_void, level: i32, ptr: *const u8, len: usize) {
    unsafe { channel(host) }.send(json!({ "call": "log", "level": level, "text": unsafe { text(ptr, len) } }));
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
