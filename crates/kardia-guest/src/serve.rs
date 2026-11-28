//! The process transport: [`serve`] answers the exports a host sends as JSON
//! lines on stdin: protocol version 1, or `kardia-guest/3`, where a reader
//! thread takes the host's messages as they come, so a cancelled export is
//! told at once, a host call waits for its own reply, and an event reaches
//! the guest between exports.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::Guest;
use crate::ffi::{Host, HostCalls};

const PROTOCOL: &str = "kardia-guest/3";

/// Serves `G` as a process guest: answers the exports the host sends as JSON
/// lines on stdin, until it disposes the instance or stdin closes. stdout
/// carries the protocol, so the guest must not print to it; log through
/// [`Host::log`] or stderr.
///
/// A host that opens with the handshake gets `kardia-guest/3`: a reader
/// thread takes the host's messages, so each `call` runs on a thread of its
/// own and the calls of the instance overlap, a host call waits only for its
/// own reply, an event the host sends reaches
/// [`Guest::event`](crate::Guest::event) as it comes, a cancelled export sees
/// [`Host::cancelled`], and a host the guest keeps ([`Host::detached`])
/// reaches the host from any thread. A host that opens with an export instead
/// is served protocol version 1.
pub fn serve<G: Guest>() {
    let stdin = Arc::new(Mutex::new(std::io::stdin()));
    let Some(first) = next(&stdin) else { return };
    // the host offers the version a kardia-guest 0.2 guest knows; a guest answers
    // with the newest version it speaks, and the host serves that
    if first["export"] == "hello" {
        return serve_v3::<G>(stdin);
    }
    serve_v1::<G>(stdin, first);
}

/// Serves a version 1 host: one export at a time, its host calls answered
/// from stdin on this thread.
fn serve_v1<G: Guest>(stdin: SharedReader, first: Value) {
    let message = Arc::new(Mutex::new(Vec::new()));
    let host = Host::new(Arc::new(V1 { stdin: stdin.clone(), message: message.clone() }));
    let mut slot: Option<G> = None;
    let mut request = Some(first);
    while let Some(req) = request.take().or_else(|| next(&stdin)) {
        let export = req["export"].as_str().unwrap_or_default().to_string();
        *message.lock().unwrap() = match &req["input"] {
            Value::Null => Vec::new(),
            input => serde_json::to_vec(input).expect("a JSON value"),
        };
        let len = message.lock().unwrap().len();
        let status = match export.as_str() {
            "meta" => crate::ffi::meta::<G>(host.clone()),
            "apply" => crate::ffi::apply::<G>(host.clone(), len, &mut slot),
            "call" => crate::ffi::call::<G>(host.clone(), len, &mut slot),
            "dispose" => {
                crate::ffi::dispose::<G>(host.clone(), slot.take());
                send(&json!({ "done": 0 }));
                return;
            }
            _ => 1,
        };
        send(&json!({ "done": status }));
    }
}

/// Serves `kardia-guest/3`: the reader thread answers pings, records
/// cancellations and replies to host calls at once, and queues exports and
/// events. This thread runs each `call` on a thread of its own, so calls of
/// the instance overlap; `meta`, `apply` and `dispose` run alone, after the
/// calls in flight; an event is delivered as it comes.
fn serve_v3<G: Guest>(stdin: SharedReader) {
    let link = Arc::new(Link {
        stdin,
        next_seq: AtomicU64::new(0),
        replies: Mutex::new(HashMap::new()),
        cancels: Mutex::new(HashMap::new()),
    });
    let (sender, receiver) = mpsc::channel::<Value>();
    let reader = link.clone();
    if std::thread::Builder::new().name("kardia-guest-stdin".into()).spawn(move || read(&reader, &sender)).is_err() {
        std::process::exit(1);
    }
    send(&json!({ "ready": PROTOCOL }));
    let instance: Arc<Mutex<Option<Arc<G>>>> = Arc::new(Mutex::new(None));
    while let Some(request) = receiver.recv().ok().filter(|request| !request.is_null()) {
        if let Some(event) = request.get("event").and_then(Value::as_str) {
            // the host of no export: a call through it is answered by `seq`
            let host = Host::new(Arc::new(Detached {
                link: link.clone(),
                id: 0,
                cancelled: None,
                message: Arc::new(Mutex::new(Vec::new())),
            }));
            let payload = request.get("payload").cloned().unwrap_or(Value::Null);
            if let Some(guest) = instance.lock().unwrap().clone() {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| guest.event(&host, event, payload)));
            }
            continue;
        }
        let id = request.get("id").and_then(Value::as_u64).unwrap_or(0);
        let cancelled = link.cancel_of(id);
        let export = request["export"].as_str().unwrap_or_default().to_string();
        let message = Arc::new(Mutex::new(match &request["input"] {
            Value::Null => Vec::new(),
            input => serde_json::to_vec(input).expect("a JSON value"),
        }));
        let len = message.lock().unwrap().len();
        let host = Host::new(Arc::new(Detached { link: link.clone(), id, cancelled: Some(cancelled), message }));
        match export.as_str() {
            // a call runs on a thread of its own: the next export is read at once
            "call" => {
                let guest = instance.lock().unwrap().clone();
                let Some(guest) = guest else {
                    send(&json!({ "id": id, "output": "the guest has not been applied", "done": 1 }));
                    continue;
                };
                RUNNING.fetch_add(1, Ordering::AcqRel);
                std::thread::spawn(move || {
                    let status = crate::ffi::call_shared::<G>(host, len, &guest);
                    send(&json!({ "id": id, "done": status }));
                    RUNNING.fetch_sub(1, Ordering::AcqRel);
                });
            }
            "dispose" => {
                while RUNNING.load(Ordering::Acquire) > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                let guest = instance.lock().unwrap().take();
                if let Some(guest) = guest {
                    crate::ffi::dispose::<G>(
                        host,
                        Some(Arc::try_unwrap(guest).unwrap_or_else(|_| unreachable!("no call in flight"))),
                    );
                }
                send(&json!({ "id": id, "done": 0 }));
                return;
            }
            _ => {
                while RUNNING.load(Ordering::Acquire) > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                let mut slot = instance
                    .lock()
                    .unwrap()
                    .take()
                    .map(|guest| Arc::try_unwrap(guest).unwrap_or_else(|_| unreachable!("no call in flight")));
                let status = match export.as_str() {
                    "meta" => crate::ffi::meta::<G>(host.clone()),
                    "apply" => crate::ffi::apply::<G>(host.clone(), len, &mut slot),
                    _ => 1,
                };
                *instance.lock().unwrap() = slot.map(Arc::new);
                send(&json!({ "id": id, "done": status }));
            }
        }
    }
}

/// The calls of the instance running on their own threads.
static RUNNING: AtomicU64 = AtomicU64::new(0);

type SharedReader = Arc<Mutex<std::io::Stdin>>;

/// What answers a host call: its message, or its error message.
type Answer = Result<Vec<u8>, Vec<u8>>;

/// What the threads of a version 3 guest share.
struct Link {
    stdin: SharedReader,
    next_seq: AtomicU64,
    /// What answers host call `seq`, by `seq`.
    replies: Mutex<HashMap<u64, Sender<Answer>>>,
    /// The exports the host cancelled, by id.
    cancels: Mutex<HashMap<u64, Arc<AtomicBool>>>,
}

impl Link {
    /// The cancellation flag of export `id`, made when it is first asked for.
    fn cancel_of(&self, id: u64) -> Arc<AtomicBool> {
        self.cancels.lock().unwrap().entry(id).or_default().clone()
    }
}

/// The host of one export of a version 3 guest, or of no export (the host
/// an event or a detached `Host` gets): its calls go out under a `seq` of
/// their own, so they do not wait for each other. Its message is the
/// export's input, then the result of its last host call.
struct Detached {
    link: Arc<Link>,
    id: u64,
    cancelled: Option<Arc<AtomicBool>>,
    message: Arc<Mutex<Vec<u8>>>,
}

impl HostCalls for Detached {
    fn raw_read(&self, len: usize) -> Vec<u8> {
        self.message.lock().unwrap()[..len].to_vec()
    }

    fn raw_write(&self, bytes: &[u8]) {
        send(&json!({ "id": self.id, "output": String::from_utf8_lossy(bytes) }));
    }

    fn raw_log(&self, level: i32, text: &[u8]) {
        send(&json!({ "call": "log", "level": level, "text": String::from_utf8_lossy(text) }));
    }

    fn raw_provide(&self, key: &[u8], value: &[u8]) -> i32 {
        self.request(json!({ "call": "provide", "key": String::from_utf8_lossy(key), "value": text(value) }))
    }

    fn raw_view(&self, key: &[u8]) -> i32 {
        self.request(json!({ "call": "view", "key": String::from_utf8_lossy(key) }))
    }

    fn raw_invoke(&self, key: &[u8], method: &[u8], args: &[u8]) -> i32 {
        self.request(json!({
            "call": "invoke",
            "key": String::from_utf8_lossy(key),
            "method": String::from_utf8_lossy(method),
            "args": text(args),
        }))
    }

    fn detached(&self) -> Arc<dyn HostCalls> {
        Arc::new(Detached {
            link: self.link.clone(),
            id: self.id,
            cancelled: self.cancelled.clone(),
            message: Arc::new(Mutex::new(Vec::new())),
        })
    }

    fn is_detached(&self) -> bool {
        true
    }

    fn cancelled(&self) -> bool {
        self.cancelled.as_ref().is_some_and(|flag| flag.load(Ordering::Acquire))
    }
}

impl Detached {
    /// Sends a host call under a `seq` of its own and waits for its reply.
    fn request(&self, mut request: Value) -> i32 {
        let seq = self.link.next_seq.fetch_add(1, Ordering::Relaxed);
        request["seq"] = json!(seq);
        request["id"] = json!(self.id);
        let (sender, receiver) = mpsc::channel();
        self.link.replies.lock().unwrap().insert(seq, sender);
        send(&request);
        // the host is gone, and with it everything this guest serves
        let Ok(reply) = receiver.recv() else { std::process::exit(1) };
        *self.message.lock().unwrap() = match &reply {
            Ok(message) => message.clone(),
            Err(error) => error.clone(),
        };
        match reply {
            Ok(message) => message.len() as i32,
            Err(message) => -1 - message.len() as i32,
        }
    }
}

/// Reads stdin for `kardia-guest/3`: answers pings, records cancellations
/// and replies to host calls at once, and queues the rest.
fn read(link: &Link, sender: &Sender<Value>) {
    while let Some(message) = next(&link.stdin) {
        if let Some(id) = message.get("ping") {
            send(&json!({ "pong": id }));
        } else if let Some(id) = message.get("cancel").and_then(Value::as_u64) {
            link.cancel_of(id).store(true, Ordering::Release);
        } else if let Some(seq) = message.get("seq").and_then(Value::as_u64) {
            if message.get("result").is_some() || message.get("error").is_some() {
                let reply = match message.get("error") {
                    Some(error) => Err(error.as_str().unwrap_or_default().as_bytes().to_vec()),
                    None => Ok(message
                        .get("result")
                        .filter(|result| !result.is_null())
                        .map(|result| serde_json::to_vec(result).expect("a JSON value"))
                        .unwrap_or_default()),
                };
                if let Some(waiting) = link.replies.lock().unwrap().remove(&seq) {
                    let _ = waiting.send(reply);
                }
            }
        } else if sender.send(message).is_err() {
            return;
        }
    }
    let _ = sender.send(Value::Null);
}

/// The host of a version 1 guest: one message at a time, the reply read
/// from stdin by the call that waits for it.
struct V1 {
    stdin: SharedReader,
    message: Arc<Mutex<Vec<u8>>>,
}

impl HostCalls for V1 {
    fn raw_read(&self, len: usize) -> Vec<u8> {
        self.message.lock().unwrap()[..len].to_vec()
    }

    fn raw_write(&self, bytes: &[u8]) {
        send(&json!({ "output": String::from_utf8_lossy(bytes) }));
    }

    fn raw_log(&self, level: i32, text: &[u8]) {
        send(&json!({ "call": "log", "level": level, "text": String::from_utf8_lossy(text) }));
    }

    fn raw_provide(&self, key: &[u8], value: &[u8]) -> i32 {
        self.request(json!({ "call": "provide", "key": String::from_utf8_lossy(key), "value": text(value) }))
    }

    fn raw_view(&self, key: &[u8]) -> i32 {
        self.request(json!({ "call": "view", "key": String::from_utf8_lossy(key) }))
    }

    fn raw_invoke(&self, key: &[u8], method: &[u8], args: &[u8]) -> i32 {
        self.request(json!({
            "call": "invoke",
            "key": String::from_utf8_lossy(key),
            "method": String::from_utf8_lossy(method),
            "args": text(args),
        }))
    }

    fn detached(&self) -> Arc<dyn HostCalls> {
        Arc::new(crate::ffi::unreachable())
    }
}

impl V1 {
    /// Sends a host call and reads its reply from stdin.
    fn request(&self, request: Value) -> i32 {
        send(&request);
        let reply = next(&self.stdin).unwrap_or_else(|| std::process::exit(1));
        let message = match reply.get("error") {
            Some(error) => Err(error.as_str().unwrap_or_default().as_bytes().to_vec()),
            None => match reply.get("result") {
                None | Some(Value::Null) => Ok(Vec::new()),
                Some(result) => Ok(serde_json::to_vec(result).expect("a JSON value")),
            },
        };
        let mut reply = self.message.lock().unwrap();
        match message {
            Ok(message) => {
                *reply = message;
                reply.len() as i32
            }
            Err(error) => {
                *reply = error;
                -1 - reply.len() as i32
            }
        }
    }
}

/// The next line of stdin as JSON; `None` at its end.
fn next(stdin: &SharedReader) -> Option<Value> {
    let mut line = String::new();
    let mut stdin = stdin.lock().unwrap().lock();
    match stdin.read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => serde_json::from_str(&line).ok(),
    }
}

/// Writes one message; the host is gone if it cannot.
fn send(message: &Value) {
    let mut stdout = std::io::stdout().lock();
    if writeln!(stdout, "{message}").and_then(|()| stdout.flush()).is_err() {
        std::process::exit(1);
    }
}

/// A JSON value from bytes, or null.
fn text(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap_or(Value::Null)
}
