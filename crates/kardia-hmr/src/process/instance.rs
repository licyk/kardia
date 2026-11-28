//! A guest instance in a child process: the handshake and the exchange of
//! exports with their host calls. Version 1 is serial; `kardia-guest/3`
//! multiplexes: a thread of its own reads the guest's messages and hands
//! each to the export or host call waiting for it, so exports run at the
//! same time, the guest may call the host from anywhere, and the host may
//! send an event between exports.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use kardia_core::{Error, Result};
use parking_lot::{Condvar, Mutex};
use serde_json::{Value, json};

use crate::guest::{
    CallControl, Export, GuestFailure, GuestFailureKind, GuestHost, GuestInstance, cancelled, exited, timed_out,
};
use crate::process::pipes::{self, Incoming, Shared};
use crate::process::{ExitCause, GuestHandle, GuestProcess};

/// The protocol version a host offers in its handshake; a guest may answer
/// with version 2 instead, which it is then served.
pub(crate) const PROTOCOL: &str = "kardia-guest/3";
/// The version before, which a guest built with kardia-guest 0.2 speaks.
const LEGACY: &str = "kardia-guest/2";
/// How long `dispose` may take when the guest has no timeout of its own.
const DISPOSE_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) struct Options {
    pub(crate) timeout: Option<Duration>,
    pub(crate) handshake_timeout: Duration,
    pub(crate) frame_limit: usize,
}

/// One export the host sent and waits for.
struct Pending {
    done: Reply<Result<Result<Vec<u8>>, Fault>>,
    output: Vec<u8>,
    cancelled: bool,
}

/// What the reader thread and the callers share: the exports in flight.
struct Exchanges {
    pending: Mutex<HashMap<u64, Pending>>,
    /// Why the guest is gone, once it is: the fault every waiter fails with.
    closed: Mutex<Option<Fault>>,
}

/// A one-shot answer that a blocking thread can wait for with a deadline.
struct Reply<T>(Arc<Slot<T>>);

struct Slot<T> {
    answer: Mutex<Option<T>>,
    arrived: Condvar,
}

impl<T> Reply<T> {
    fn new() -> (Self, Waiter<T>) {
        let slot = Arc::new(Slot { answer: Mutex::new(None), arrived: Condvar::new() });
        (Self(slot.clone()), Waiter(slot))
    }

    fn send(self, answer: T) {
        *self.0.answer.lock() = Some(answer);
        self.0.arrived.notify_all();
    }
}

struct Waiter<T>(Arc<Slot<T>>);

impl<T> Waiter<T> {
    /// The answer; a timeout kills the guest, and a channel dropped without
    /// one means the guest is gone.
    fn wait(&self, deadline: Option<Instant>) -> Result<T, Fault> {
        let mut answer = self.0.answer.lock();
        match deadline {
            Some(deadline) => loop {
                if answer.is_some() {
                    break;
                }
                if self.0.arrived.wait_until(&mut answer, deadline).timed_out() {
                    if answer.is_none() {
                        return Err(Fault::Kill(GuestFailureKind::Timeout, timed_out().to_string()));
                    }
                    break;
                }
            },
            None => {
                while answer.is_none() {
                    self.0.arrived.wait(&mut answer);
                }
            }
        }
        answer.take().ok_or(Fault::Exited)
    }
}

impl Exchanges {
    /// Ends every exchange: the guest is gone for `fault`.
    fn close(&self, fault: Fault) {
        *self.closed.lock() = Some(fault.clone());
        // a waiter still waiting fails with the fault; one already answered keeps its result
        for (_, pending) in self.pending.lock().drain() {
            pending.done.send(Err(fault.clone()));
        }
    }
}

pub(crate) struct ProcessInstance {
    host: Arc<GuestHost>,
    shared: Arc<Shared>,
    wake: Sender<Incoming>,
    exchanges: Arc<Exchanges>,
    timeout: Option<Duration>,
    calls: Mutex<u64>,
    /// Held by every export of a version 1 guest, which speaks one exchange
    /// at a time, and by the lifecycle exports of a version 3 guest.
    serial: Mutex<()>,
    /// Out of step with the guest, or without one: every later call fails.
    broken: Mutex<Option<Error>>,
    /// What a version 1 guest sends, read by the export it belongs to.
    legacy: Mutex<Option<Receiver<Incoming>>>,
}

enum Received {
    Message(Value),
    Cancel,
}

/// Why an exchange cannot go on.
#[derive(Clone)]
enum Fault {
    /// The process is gone.
    Exited,
    /// The reader killed the guest, for this reason.
    Closed(String),
    /// The guest must be killed, for this reason.
    Kill(GuestFailureKind, String),
}

impl Fault {
    fn error(&self) -> Error {
        match self {
            Fault::Exited => exited(),
            Fault::Closed(reason) => GuestFailure::new(GuestFailureKind::Killed, reason.clone()).into(),
            Fault::Kill(kind, reason) => GuestFailure::new(*kind, reason.clone()).into(),
        }
    }
}

impl ProcessInstance {
    /// Starts the threads of a spawned process and completes the handshake.
    pub(crate) fn start(process: GuestProcess, host: GuestHost, options: &Options) -> Result<Self> {
        let (shared, incoming, wake) = pipes::start(process, options.frame_limit, host.logger())?;
        let instance = Self {
            host: Arc::new(host),
            shared,
            wake,
            exchanges: Arc::new(Exchanges { pending: Mutex::new(HashMap::new()), closed: Mutex::new(None) }),
            timeout: options.timeout,
            calls: Mutex::new(0),
            serial: Mutex::new(()),
            broken: Mutex::new(None),
            legacy: Mutex::new(None),
        };
        let handshake = instance.handshake(&incoming, options.handshake_timeout);
        match handshake {
            Ok(()) => {}
            Err(fault) => return Err(instance.fault(fault)),
        }
        if instance.shared.protocol.load(Ordering::Acquire) >= 3 {
            instance.spawn_reader(incoming);
        } else {
            *instance.legacy.lock() = Some(incoming);
        }
        Ok(instance)
    }

    /// Offers the handshake under the name a kardia-guest 0.2 guest knows;
    /// the guest answers with the newest version it speaks, which is served.
    /// A version 1 guest answers with a failure status and is served version 1.
    fn handshake(&self, incoming: &Receiver<Incoming>, timeout: Duration) -> Result<(), Fault> {
        let deadline = Some(Instant::now() + timeout);
        self.shared.send(&json!({ "export": "hello", "input": { "protocol": LEGACY } }));
        match self.offered(incoming, deadline)?.as_ref().and_then(Value::as_str) {
            Some(PROTOCOL) => self.shared.protocol.store(3, Ordering::Release),
            Some(LEGACY) => self.shared.protocol.store(2, Ordering::Release),
            // the guest answered a version the host does not speak
            Some(other) => return Err(protocol_fault(format!("the guest speaks an unknown protocol: {other}"))),
            None => {}
        }
        Ok(())
    }

    /// What the guest answered to the handshake: `Some` with the version it
    /// speaks, `None` when it answered with a failure status.
    fn offered(&self, incoming: &Receiver<Incoming>, deadline: Option<Instant>) -> Result<Option<Value>, Fault> {
        loop {
            let Received::Message(message) = receive(incoming, deadline)? else { continue };
            if let Some(version) = message.get("ready").and_then(Value::as_str) {
                return Ok(Some(Value::String(version.to_string())));
            }
            if message.get("done").is_some() {
                return Ok(None);
            }
            if message.get("output").is_none() {
                return Err(protocol_fault(format!("unexpected message from guest: {message}")));
            }
        }
    }

    /// Starts the thread that reads the guest's messages under
    /// `kardia-guest/3` and answers each one: an export's output or end goes
    /// to the caller waiting for it; a host call runs here, and its reply
    /// goes back at once, whichever export the call belongs to.
    fn spawn_reader(&self, incoming: Receiver<Incoming>) {
        let (shared, host, exchanges) = (self.shared.clone(), self.host.clone(), self.exchanges.clone());
        let failed = {
            let (shared, exchanges) = (shared.clone(), exchanges.clone());
            move |reason: String| {
                // the reason reaches the waiters before the reader's end of stdout does
                exchanges.close(Fault::Kill(GuestFailureKind::Killed, reason.clone()));
                shared.kill(ExitCause::Killed(reason));
                shared.reap();
            }
        };
        if let Err(error) =
            crate::contained::spawn("guest-reader", move || reader(incoming, &shared, &host, &exchanges), failed)
        {
            // no reader: nothing would ever answer
            self.fault(Fault::Kill(
                GuestFailureKind::Killed,
                format!("cannot start the guest's reader thread: {error}"),
            ));
        }
    }

    /// Sends the export and waits for its end; the outer error is a fault
    /// of the transport, the inner one the guest's own.
    fn exchange(
        &self,
        export: Export,
        input: Vec<u8>,
        id: u64,
        deadline: Option<Instant>,
    ) -> Result<Result<Vec<u8>>, Fault> {
        let input = match input.as_slice() {
            [] => Value::Null,
            bytes => match serde_json::from_slice(bytes) {
                Ok(input) => input,
                Err(error) => return Ok(Err(error.into())),
            },
        };
        let (done, finished) = Reply::new();
        {
            // under the lock `close` drains under, so this export cannot be
            // registered after the close passed it
            let mut pending = self.exchanges.pending.lock();
            if let Some(fault) = self.exchanges.closed.lock().clone() {
                return Err(fault);
            }
            pending.insert(id, Pending { done, output: Vec::new(), cancelled: false });
        }
        self.shared.send(&json!({ "id": id, "export": export.name(), "input": input }));
        // the reader answers with the guest's own result, unless the guest is
        // gone by then, which is a fault of the transport
        finished.wait(deadline)?
    }

    /// Sends the export of a version 1 or 2 guest and answers its host calls
    /// until it is done: one exchange at a time, on the calling thread. Under
    /// version 2 every message carries the export's `id`, and the caller may
    /// cancel it.
    fn exchange_serial(
        &self,
        id: u64,
        export: Export,
        input: Vec<u8>,
        deadline: Option<Instant>,
    ) -> Result<Result<Vec<u8>>, Fault> {
        let v2 = self.shared.protocol.load(Ordering::Acquire) == 2;
        let input = match input.as_slice() {
            [] => Value::Null,
            bytes => match serde_json::from_slice(bytes) {
                Ok(input) => input,
                Err(error) => return Ok(Err(error.into())),
            },
        };
        let incoming = self.legacy.lock();
        let Some(incoming) = incoming.as_ref() else { return Err(Fault::Exited) };
        let tagged = |mut message: Value| {
            if v2 {
                message["id"] = json!(id);
            }
            message
        };
        self.shared.send(&tagged(json!({ "export": export.name(), "input": input })));
        let (mut output, mut cancel) = (Vec::new(), false);
        loop {
            let message = match receive(incoming, deadline)? {
                Received::Message(message) => message,
                Received::Cancel => {
                    if v2 && !cancel {
                        cancel = true;
                        self.shared.send(&json!({ "cancel": id }));
                    }
                    continue;
                }
            };
            if v2 && message.get("id").and_then(Value::as_u64) != Some(id) {
                // a host call of no running export would wait forever
                if message.get("call").is_some() {
                    self.shared.send(&json!({ "id": message.get("id"), "error": "no such call" }));
                }
                continue;
            }
            if let Some(status) = message.get("done") {
                let status = status.as_i64().unwrap_or(1) as i32;
                return Ok(match (cancel, status) {
                    (true, _) => Err(cancelled()),
                    (false, 0) => Ok(output),
                    (false, _) if output.is_empty() => Err(Error::msg(format!("guest failed with status {status}"))),
                    (false, _) => Err(Error::msg(String::from_utf8_lossy(&output).into_owned())),
                });
            }
            if let Some(text) = message.get("output") {
                output = text.as_str().unwrap_or_default().as_bytes().to_vec();
                continue;
            }
            if cancel && message.get("call").is_some() {
                continue;
            }
            let result = match answer(&self.host, &message) {
                Err(error) if error.to_string().starts_with("unexpected message") => {
                    return Err(protocol_fault(error.to_string()));
                }
                result => result,
            };
            self.shared.send(&tagged(match result {
                Ok(result) => json!({ "result": result }),
                Err(error) => json!({ "error": error.to_string() }),
            }));
        }
    }
}

/// The guest broke the protocol and must be killed.
fn protocol_fault(reason: String) -> Fault {
    Fault::Kill(GuestFailureKind::Killed, reason)
}

/// Runs the host call in `message`.
fn answer(host: &GuestHost, message: &Value) -> Result<Value> {
    let text = |key: &str| message.get(key).and_then(Value::as_str).unwrap_or_default();
    let json = |key: &str| message.get(key).cloned().unwrap_or(Value::Null);
    match message.get("call").and_then(Value::as_str) {
        Some("provide") => host.provide(text("key"), json("value")).map(|()| Value::Null),
        Some("view") => host.view(text("key")),
        Some("invoke") => host.invoke(text("key"), text("method"), json("args")),
        _ => Err(Error::msg(format!("unexpected message from guest: {message}"))),
    }
}

/// The next thing the guest sent, until `deadline`.
fn receive(incoming: &Receiver<Incoming>, deadline: Option<Instant>) -> Result<Received, Fault> {
    let received = match deadline {
        Some(deadline) => {
            incoming.recv_timeout(deadline.saturating_duration_since(Instant::now())).map_err(|error| match error {
                std::sync::mpsc::RecvTimeoutError::Timeout => {
                    Fault::Kill(GuestFailureKind::Timeout, timed_out().to_string())
                }
                std::sync::mpsc::RecvTimeoutError::Disconnected => Fault::Exited,
            })
        }
        None => incoming.recv().map_err(|_| Fault::Exited),
    }?;
    match received {
        Incoming::Message(message) => Ok(Received::Message(message)),
        Incoming::Cancel(_) => Ok(Received::Cancel),
        Incoming::Closed(None) => Err(Fault::Exited),
        Incoming::Closed(Some(reason)) => Err(Fault::Closed(reason)),
    }
}

impl ProcessInstance {
    /// Breaks the instance; the report of its exit is published before the
    /// error returns.
    fn fault(&self, fault: Fault) -> Error {
        let error = match fault {
            Fault::Kill(kind, ref reason) => {
                self.shared.kill(ExitCause::Killed(reason.clone()));
                GuestFailure::new(kind, reason.clone()).into()
            }
            ref fault => fault.error(),
        };
        *self.broken.lock() = Some(error.clone());
        self.exchanges.close(fault);
        self.shared.reap();
        error
    }
}

/// Reads the guest's messages under `kardia-guest/3` and hands each to what
/// waits for it, until the guest is gone. Host calls run here.
fn reader(incoming: Receiver<Incoming>, shared: &Shared, host: &GuestHost, exchanges: &Exchanges) {
    let broken = |reason: String| {
        shared.kill(ExitCause::Killed(reason.clone()));
        shared.reap();
        exchanges.close(Fault::Kill(GuestFailureKind::Killed, reason));
    };
    loop {
        match incoming.recv() {
            Ok(Incoming::Message(message)) => {
                if !handle(&message, shared, host, exchanges, &broken) {
                    return;
                }
            }
            Ok(Incoming::Cancel(id)) => {
                // the caller gave up: the guest is told, and ends the export when it sees it
                if let Some(pending) = exchanges.pending.lock().get_mut(&id)
                    && !pending.cancelled
                {
                    pending.cancelled = true;
                    shared.send(&json!({ "cancel": id }));
                }
            }
            Ok(Incoming::Closed(None)) | Err(_) => break,
            Ok(Incoming::Closed(Some(reason))) => {
                // the pipe threads killed the guest for this reason; it is theirs to report
                exchanges.close(Fault::Closed(reason));
                return;
            }
        }
    }
    // the guest's stdout closed on its own: its exit report says why
    shared.reap();
    exchanges.close(Fault::Exited);
}

/// Hands one message to what waits for it; `false` when the guest broke the
/// protocol and was killed for it.
fn handle(message: &Value, shared: &Shared, host: &GuestHost, exchanges: &Exchanges, broken: &dyn Fn(String)) -> bool {
    if message.get("call").is_some() {
        let result = answer(host, message);
        let mut reply = match result {
            Ok(result) => json!({ "result": result }),
            Err(error) => json!({ "error": error.to_string() }),
        };
        if reply.get("error").and_then(Value::as_str).is_some_and(|error| error.starts_with("unexpected message")) {
            broken(reply["error"].as_str().unwrap_or_default().to_string());
            return false;
        }
        // the reply reaches the call by `seq`, or by its export's `id` when the guest sent none
        for key in ["seq", "id"] {
            if let Some(value) = message.get(key) {
                reply[key] = value.clone();
            }
        }
        shared.send(&reply);
        return true;
    }
    let Some(id) = message.get("id").and_then(Value::as_u64) else {
        broken(format!("unexpected message from guest: {message}"));
        return false;
    };
    let mut pending = exchanges.pending.lock();
    let Some(entry) = pending.get_mut(&id) else {
        // the export ended already, or its caller gave up: nothing waits for this
        return true;
    };
    if let Some(text) = message.get("output") {
        entry.output = text.as_str().unwrap_or_default().as_bytes().to_vec();
        return true;
    }
    let Some(status) = message.get("done").and_then(Value::as_i64) else {
        drop(pending);
        broken(format!("unexpected message from guest: {message}"));
        return false;
    };
    let status = status as i32;
    let entry = pending.remove(&id).expect("found above");
    drop(pending);
    entry.done.send(Ok(match (entry.cancelled, status) {
        (true, _) => Err(cancelled()),
        (false, 0) => Ok(entry.output),
        (false, _) if entry.output.is_empty() => Err(Error::msg(format!("guest failed with status {status}"))),
        (false, _) => Err(Error::msg(String::from_utf8_lossy(&entry.output).into_owned())),
    }));
    true
}

impl GuestInstance for ProcessInstance {
    fn call(&self, export: Export, input: Vec<u8>, control: &CallControl) -> Result<Vec<u8>> {
        if self.broken.lock().is_some() {
            // disposing a guest that is gone has nothing left to do
            return if export == Export::Dispose { Ok(Vec::new()) } else { Err(exited()) };
        }
        let timeout = match export {
            Export::Dispose => {
                // a guest exits once it is disposed: its reader may reap it before this call returns
                self.shared.mark(ExitCause::Disposed);
                Some(self.timeout.unwrap_or(DISPOSE_TIMEOUT))
            }
            _ => self.timeout,
        };
        let deadline = control.deadline().or_else(|| timeout.map(|timeout| Instant::now() + timeout));
        if control.is_cancelled() {
            return Err(cancelled());
        }
        let id = {
            let mut calls = self.calls.lock();
            *calls += 1;
            *calls
        };
        let wake = self.wake.clone();
        control.on_cancel(move || {
            let _ = wake.send(Incoming::Cancel(id));
        });
        let v3 = self.shared.protocol.load(Ordering::Acquire) >= 3;
        // a version 1 guest speaks one exchange at a time; under version 3 the
        // lifecycle exports still do, as they change the instance
        let _serial = if !v3 || export.is_lifecycle() {
            Some(match control.deadline() {
                // waiting for the export ahead: this one fails, the guest is not stopped
                Some(deadline) => self.serial.try_lock_until(deadline).ok_or_else(timed_out)?,
                None => self.serial.lock(),
            })
        } else {
            None
        };
        let result = if v3 {
            self.exchange(export, input, id, deadline)
        } else {
            self.exchange_serial(id, export, input, deadline)
        };
        match result {
            Ok(result) => result,
            Err(fault) => Err(self.fault(fault)),
        }
    }

    fn emit(&self, event: &str, payload: Value) -> Result<()> {
        if self.shared.protocol.load(Ordering::Acquire) < 3 {
            return Err(Error::msg("events need a guest speaking kardia-guest/3"));
        }
        if let Some(error) = self.broken.lock().clone() {
            return Err(error);
        }
        self.shared.send(&json!({ "event": event, "payload": payload }));
        Ok(())
    }

    fn handle(&self) -> Option<GuestHandle> {
        Some(GuestHandle(self.shared.clone()))
    }
}

impl Drop for ProcessInstance {
    /// The process of a disposed (or failed) instance is never left running.
    fn drop(&mut self) {
        self.shared.kill(ExitCause::Disposed);
        self.shared.reap();
    }
}
