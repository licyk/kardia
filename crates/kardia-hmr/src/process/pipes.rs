//! The threads around a guest process (a writer for its stdin, a reader for
//! its stdout, a tail of its stderr) and the state they share. Blocking I/O
//! on threads of their own keeps calls bounded by their deadline, whether a
//! caller waits on a Tokio worker, a blocking thread or no runtime at all.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use kardia_core::{Level, Logger, Result};
use parking_lot::{Condvar, Mutex};
use serde_json::Value;
use tokio::sync::{oneshot, watch};

use crate::guest::level;
use crate::process::{ExitCause, GuestChild, GuestExit, GuestProcess};

/// How much of its stderr a guest's exit report keeps.
pub(crate) const STDERR_TAIL: usize = 16 * 1024;
/// How long a process that closed its stdout has to exit before it is killed,
/// and how long its exit report waits for the rest of its stderr.
const EXIT_GRACE: Duration = Duration::from_millis(200);

/// What the instance receives from its guest.
pub(crate) enum Incoming {
    Message(Value),
    /// The caller of call `id` cancelled it.
    Cancel(u64),
    /// The guest's stdout closed: on its own (`None`), or because the guest
    /// broke the protocol and was killed.
    Closed(Option<String>),
}

pub(crate) struct Shared {
    pub(crate) id: Option<u32>,
    pub(crate) protocol: AtomicU8,
    /// Dropped once the process is reaped, which ends the writer thread.
    writer: Mutex<Option<Sender<Vec<u8>>>>,
    child: Mutex<Box<dyn GuestChild>>,
    /// Why the process was ended, by whoever ended it first.
    cause: Mutex<Option<ExitCause>>,
    tail: Mutex<Vec<u8>>,
    stderr_open: Mutex<bool>,
    stderr_closed: Condvar,
    pub(crate) exit: watch::Sender<Option<Arc<GuestExit>>>,
    pub(crate) pongs: Mutex<HashMap<u64, oneshot::Sender<()>>>,
    pub(crate) pings: AtomicU64,
}

impl Shared {
    /// Queues a message for the guest's stdin.
    pub(crate) fn send(&self, message: &Value) {
        let mut line = serde_json::to_vec(message).expect("a JSON value");
        line.push(b'\n');
        if let Some(writer) = &*self.writer.lock() {
            let _ = writer.send(line);
        }
    }

    /// Records why the process is ending; the first cause given is the one
    /// reported.
    pub(crate) fn mark(&self, cause: ExitCause) {
        self.cause.lock().get_or_insert(cause);
    }

    /// Ends the process, recording `cause` first.
    pub(crate) fn kill(&self, cause: ExitCause) {
        self.mark(cause);
        let _ = self.child.lock().kill();
    }

    /// Waits for the process to end and publishes its exit report, once.
    pub(crate) fn reap(&self) {
        if self.exit.borrow().is_some() {
            return;
        }
        let status = self.settle();
        let mut open = self.stderr_open.lock();
        let deadline = Instant::now() + EXIT_GRACE;
        while *open && !self.stderr_closed.wait_until(&mut open, deadline).timed_out() {}
        drop(open);
        let cause = self.cause.lock().clone().unwrap_or(ExitCause::Exited);
        let exit = Arc::new(GuestExit { status, cause, stderr_tail: self.tail() });
        self.exit.send_if_modified(|current| current.is_none() && current.replace(exit).is_none());
        self.pongs.lock().clear();
        self.writer.lock().take();
    }

    pub(crate) fn tail(&self) -> Vec<u8> {
        self.tail.lock().clone()
    }

    /// The exit status, after a grace for a process that is ending anyway.
    fn settle(&self) -> Option<ExitStatus> {
        let deadline = Instant::now() + EXIT_GRACE;
        loop {
            let mut child = self.child.lock();
            match child.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) if Instant::now() < deadline => {}
                _ => {
                    self.cause.lock().get_or_insert_with(|| ExitCause::Killed("the guest closed its stdout".into()));
                    let _ = child.kill();
                    return child.wait().ok();
                }
            }
            drop(child);
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn pong(&self, id: u64) {
        if let Some(pong) = self.pongs.lock().remove(&id) {
            let _ = pong.send(());
        }
    }

    fn push_stderr(&self, bytes: &[u8]) {
        let mut tail = self.tail.lock();
        tail.extend_from_slice(bytes);
        let overflow = tail.len().saturating_sub(STDERR_TAIL);
        tail.drain(..overflow);
    }
}

/// Starts the threads of a guest process. Returns its shared state, what it
/// sends, and a sender to wake the instance with.
pub(crate) fn start(
    process: GuestProcess,
    limit: usize,
    logger: Option<Logger>,
) -> Result<(Arc<Shared>, Receiver<Incoming>, Sender<Incoming>)> {
    let GuestProcess { stdin, stdout, stderr, child } = process;
    let (writer, lines) = mpsc::channel();
    let (incoming, receiver) = mpsc::channel();
    let shared = Arc::new(Shared {
        id: child.id(),
        protocol: AtomicU8::new(1),
        writer: Mutex::new(Some(writer)),
        child: Mutex::new(child),
        cause: Mutex::new(None),
        tail: Mutex::new(Vec::new()),
        stderr_open: Mutex::new(stderr.is_some()),
        stderr_closed: Condvar::new(),
        exit: watch::Sender::new(None),
        pongs: Mutex::new(HashMap::new()),
        pings: AtomicU64::new(0),
    });
    let started = (|| {
        spawn("guest-stdin", move || write_lines(stdin, lines))?;
        if let Some(stderr) = stderr {
            let (shared, logger) = (shared.clone(), logger.clone());
            spawn("guest-stderr", move || read_stderr(stderr, &shared, logger))?;
        }
        let (shared, incoming) = (shared.clone(), incoming.clone());
        spawn("guest-stdout", move || read_messages(stdout, limit, &incoming, &shared, logger))
    })();
    if let Err(error) = started {
        shared.kill(ExitCause::Killed(format!("cannot start the guest's threads: {error}")));
        return Err(error.into());
    }
    Ok((shared, receiver, incoming))
}

fn spawn(name: &str, body: impl FnOnce() + Send + 'static) -> io::Result<()> {
    thread::Builder::new().name(name.into()).spawn(body).map(drop)
}

/// Writes queued lines until the instance is gone or the pipe breaks.
fn write_lines(mut stdin: Box<dyn Write + Send>, lines: Receiver<Vec<u8>>) {
    for line in lines {
        if stdin.write_all(&line).and_then(|()| stdin.flush()).is_err() {
            return;
        }
    }
}

/// Reads one JSON message per line. A log is logged at once and a pong
/// answers its ping; the rest goes to the instance. A line that is not JSON,
/// or longer than `limit`, kills the guest.
fn read_messages(
    stdout: Box<dyn Read + Send>,
    limit: usize,
    incoming: &Sender<Incoming>,
    shared: &Shared,
    logger: Option<Logger>,
) {
    let mut reader = BufReader::new(stdout);
    let fault = loop {
        let line = match read_line(&mut reader, limit) {
            Ok(Some(line)) => line,
            Ok(None) => break None,
            Err(fault) => break fault,
        };
        let message: Value = match serde_json::from_slice(&line) {
            Ok(message) => message,
            Err(error) => break Some(format!("invalid message from guest: {error}")),
        };
        if let Some(id) = message.get("pong").and_then(Value::as_u64) {
            shared.pong(id);
            continue;
        }
        if message.get("call").and_then(Value::as_str) == Some("log") {
            if let Some(logger) = &logger {
                let level = level(message.get("level").and_then(Value::as_i64).unwrap_or(2));
                logger.log(level, message.get("text").and_then(Value::as_str).unwrap_or_default());
            }
            continue;
        }
        if incoming.send(Incoming::Message(message)).is_err() {
            break None;
        }
    };
    if let Some(reason) = &fault {
        shared.kill(ExitCause::Killed(reason.clone()));
    }
    let _ = incoming.send(Incoming::Closed(fault));
    shared.reap();
}

/// A line without its newline; `None` at the end. `Err(Some(_))` is a line
/// over the limit, `Err(None)` a broken pipe.
fn read_line(reader: &mut impl BufRead, limit: usize) -> Result<Option<Vec<u8>>, Option<String>> {
    let mut line = Vec::new();
    let read = reader.take(limit as u64 + 1).read_until(b'\n', &mut line).map_err(|_| None)?;
    if read == 0 {
        return Ok(None);
    }
    if line.last() == Some(&b'\n') {
        line.pop();
    } else if line.len() > limit {
        return Err(Some(format!("a message from the guest exceeds the limit of {limit} bytes")));
    }
    Ok(Some(line))
}

/// Keeps the tail of the guest's stderr, logging each line at debug.
fn read_stderr(stderr: Box<dyn Read + Send>, shared: &Shared, logger: Option<Logger>) {
    let mut reader = BufReader::new(stderr);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.by_ref().take(STDERR_TAIL as u64).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                shared.push_stderr(&line);
                if let Some(logger) = &logger {
                    logger.log(Level::Debug, String::from_utf8_lossy(&line).trim_end());
                }
            }
        }
    }
    *shared.stderr_open.lock() = false;
    shared.stderr_closed.notify_all();
}
