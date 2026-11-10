//! A guest instance in a child process: the handshake and the exchange of
//! one export with its host calls, in either protocol version.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use kardia_core::{Error, Result};
use serde_json::{Value, json};

use crate::guest::{CallControl, Export, GuestHost, GuestInstance, cancelled, exited, timed_out};
use crate::process::pipes::{self, Incoming, Shared};
use crate::process::{ExitCause, GuestHandle, GuestProcess};

/// The protocol version a host offers in its handshake.
pub(crate) const PROTOCOL: &str = "kardia-guest/2";
/// How long `dispose` may take when the guest has no timeout of its own.
const DISPOSE_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) struct Options {
    pub(crate) timeout: Option<Duration>,
    pub(crate) handshake_timeout: Duration,
    pub(crate) frame_limit: usize,
}

pub(crate) struct ProcessInstance {
    host: GuestHost,
    shared: Arc<Shared>,
    incoming: Receiver<Incoming>,
    wake: Sender<Incoming>,
    timeout: Option<Duration>,
    calls: u64,
    /// Out of step with the guest, or without one: every later call fails.
    broken: bool,
}

enum Received {
    Message(Value),
    Cancel(u64),
}

/// Why an exchange cannot go on.
enum Fault {
    /// The process is gone.
    Exited,
    /// The reader killed the guest, for this reason.
    Closed(String),
    /// The guest must be killed, for this reason.
    Kill(String),
}

impl ProcessInstance {
    /// Starts the threads of a spawned process and completes the handshake.
    pub(crate) fn start(process: GuestProcess, host: GuestHost, options: &Options) -> Result<Self> {
        let (shared, incoming, wake) = pipes::start(process, options.frame_limit, host.logger())?;
        let mut instance = Self { host, shared, incoming, wake, timeout: options.timeout, calls: 0, broken: false };
        instance.handshake(options.handshake_timeout).map_err(|fault| instance.fault(fault))?;
        Ok(instance)
    }

    /// Offers `kardia-guest/2` as an export a version 1 guest answers with a
    /// failure status, so either version starts without a delay.
    fn handshake(&mut self, timeout: Duration) -> Result<(), Fault> {
        self.shared.send(&json!({ "export": "hello", "input": { "protocol": PROTOCOL } }));
        let deadline = Some(Instant::now() + timeout);
        loop {
            let Received::Message(message) = self.receive(deadline)? else { continue };
            if let Some(version) = message.get("ready") {
                if version.as_str() != Some(PROTOCOL) {
                    return Err(Fault::Kill(format!("the guest speaks an unknown protocol: {version}")));
                }
                self.shared.protocol.store(2, Ordering::Release);
                return Ok(());
            }
            if message.get("done").is_some() {
                return Ok(());
            }
            if message.get("output").is_none() {
                return Err(Fault::Kill(format!("unexpected message from guest: {message}")));
            }
        }
    }

    /// Sends the export and answers the guest's host calls until it is done.
    /// The outer error is a fault of the transport, the inner one the
    /// guest's own.
    fn exchange(
        &mut self,
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
        let v2 = self.shared.protocol.load(Ordering::Acquire) == 2;
        let with_id = |mut message: Value| {
            if v2 {
                message["id"] = json!(id);
            }
            message
        };
        self.shared.send(&with_id(json!({ "export": export.name(), "input": input })));
        let (mut output, mut cancel) = (Vec::new(), false);
        loop {
            let message = match self.receive(deadline)? {
                Received::Message(message) => message,
                Received::Cancel(call) => {
                    if call == id && !cancel {
                        cancel = true;
                        if v2 {
                            self.shared.send(&json!({ "cancel": id }));
                        }
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
            let text = |key: &str| message.get(key).and_then(Value::as_str).unwrap_or_default();
            let json = |key: &str| message.get(key).cloned().unwrap_or(Value::Null);
            let result = match message.get("call").and_then(Value::as_str) {
                Some(_) if cancel => Err(cancelled()),
                Some("provide") => self.host.provide(text("key"), json("value")).map(|()| Value::Null),
                Some("view") => self.host.view(text("key")),
                Some("invoke") => self.host.invoke(text("key"), text("method"), json("args")),
                _ => return Err(Fault::Kill(format!("unexpected message from guest: {message}"))),
            };
            let reply = match result {
                Ok(result) => json!({ "result": result }),
                Err(error) => json!({ "error": error.to_string() }),
            };
            self.shared.send(&with_id(reply));
        }
    }

    fn receive(&mut self, deadline: Option<Instant>) -> Result<Received, Fault> {
        let received = match deadline {
            Some(deadline) => {
                self.incoming.recv_timeout(deadline.saturating_duration_since(Instant::now())).map_err(|error| {
                    match error {
                        RecvTimeoutError::Timeout => Fault::Kill(timed_out().to_string()),
                        RecvTimeoutError::Disconnected => Fault::Exited,
                    }
                })
            }
            None => self.incoming.recv().map_err(|_| Fault::Exited),
        }?;
        match received {
            Incoming::Message(message) => Ok(Received::Message(message)),
            Incoming::Cancel(id) => Ok(Received::Cancel(id)),
            Incoming::Closed(None) => Err(Fault::Exited),
            Incoming::Closed(Some(reason)) => Err(Fault::Closed(reason)),
        }
    }

    /// Breaks the instance; the report of its exit is published before the
    /// error returns.
    fn fault(&mut self, fault: Fault) -> Error {
        self.broken = true;
        let error = match fault {
            Fault::Exited => exited(),
            Fault::Closed(reason) => Error::msg(reason),
            Fault::Kill(reason) => {
                self.shared.kill(ExitCause::Killed(reason.clone()));
                Error::msg(reason)
            }
        };
        self.shared.reap();
        error
    }
}

impl GuestInstance for ProcessInstance {
    fn call(&mut self, export: Export, input: Vec<u8>, control: &CallControl) -> Result<Vec<u8>> {
        if self.broken {
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
        self.calls += 1;
        let (id, wake) = (self.calls, self.wake.clone());
        control.on_cancel(move || {
            let _ = wake.send(Incoming::Cancel(id));
        });
        match self.exchange(export, input, id, deadline) {
            Ok(result) => result,
            Err(fault) => Err(self.fault(fault)),
        }
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
