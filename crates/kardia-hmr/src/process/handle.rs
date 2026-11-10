//! The handle of a running process guest, its exit report and the events
//! the bridge emits about it.

use std::fmt;
use std::future::Future;
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use kardia_core::{Error, Event, Fiber, Result};
use serde_json::json;
use tokio::sync::{oneshot, watch};

use crate::guest::exited;
use crate::process::pipes::Shared;

/// A running process guest, for its host: its process, a watch on its exit
/// and a liveness check.
#[derive(Clone)]
pub struct GuestHandle(pub(crate) Arc<Shared>);

impl GuestHandle {
    /// The process id the spawner reported.
    pub fn id(&self) -> Option<u32> {
        self.0.id
    }

    /// The protocol the guest speaks: `1` or `2` (`kardia-guest/2`).
    pub fn protocol(&self) -> u8 {
        self.0.protocol.load(Ordering::Acquire)
    }

    /// The exit report, once the process has ended.
    pub fn exit(&self) -> Option<Arc<GuestExit>> {
        self.0.exit.borrow().clone()
    }

    /// A watch on the exit report: `None` while the process runs.
    pub fn subscribe(&self) -> watch::Receiver<Option<Arc<GuestExit>>> {
        self.0.exit.subscribe()
    }

    /// Resolves with the exit report when the process ends.
    pub fn exited(&self) -> impl Future<Output = Arc<GuestExit>> + Send + 'static {
        let (shared, mut exit) = (self.0.clone(), self.0.exit.subscribe());
        async move {
            // `shared` keeps the sender, so the watch never closes
            let exit = exit.wait_for(Option::is_some).await.map(|exit| exit.clone());
            drop(shared);
            exit.ok().flatten().expect("the exit watch stays open while its handle lives")
        }
    }

    /// The last 16 KiB the process wrote to stderr so far.
    pub fn stderr_tail(&self) -> Vec<u8> {
        self.0.tail()
    }

    /// Kills the process; its exit report says it was killed by the host.
    pub fn kill(&self) {
        self.0.kill(ExitCause::Killed("killed by the host".into()));
    }

    /// Checks that the guest answers (`kardia-guest/2` only), even while it
    /// runs a call; resolves with the round trip. A late answer does not stop
    /// the guest: its host decides.
    pub fn ping(&self, timeout: Duration) -> impl Future<Output = Result<Duration>> + Send + 'static {
        let shared = self.0.clone();
        let protocol = self.protocol();
        async move {
            if protocol < 2 {
                return Err(Error::msg("ping needs a guest speaking kardia-guest/2"));
            }
            if shared.exit.borrow().is_some() {
                return Err(exited());
            }
            let id = shared.pings.fetch_add(1, Ordering::Relaxed);
            let (sender, pong) = oneshot::channel();
            shared.pongs.lock().insert(id, sender);
            let start = Instant::now();
            shared.send(&json!({ "ping": id }));
            match tokio::time::timeout(timeout, pong).await {
                Ok(Ok(())) => Ok(start.elapsed()),
                Ok(Err(_)) => Err(exited()),
                Err(_) => {
                    shared.pongs.lock().remove(&id);
                    Err(Error::msg("the guest did not answer the ping in time"))
                }
            }
        }
    }
}

/// How a guest process ended.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExitCause {
    /// It exited on its own.
    Exited,
    /// It was killed, for the reason given: a call outlived its deadline, the
    /// guest broke the protocol, or its host killed it.
    Killed(String),
    /// Its fiber disposed it.
    Disposed,
}

/// The report of a guest process that ended.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct GuestExit {
    /// `None` if waiting for the process failed.
    pub status: Option<ExitStatus>,
    pub cause: ExitCause,
    /// The last 16 KiB the process wrote to stderr.
    pub stderr_tail: Vec<u8>,
}

impl fmt::Display for GuestExit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.cause {
            ExitCause::Exited => write!(f, "the guest process exited")?,
            ExitCause::Killed(reason) => write!(f, "the guest process was killed: {reason}")?,
            ExitCause::Disposed => write!(f, "the guest process was disposed")?,
        }
        match &self.status {
            Some(status) => write!(f, " ({status})"),
            None => Ok(()),
        }
    }
}

/// Emitted on a guest fiber when its process has started, before `apply`.
pub struct GuestStarted {
    pub fiber: Fiber,
    pub handle: GuestHandle,
}

impl Event for GuestStarted {
    type Output = ();
}

/// Emitted on a guest fiber when its process ends before the fiber disposes
/// it; the fiber then fails with the report as its error.
pub struct GuestExited {
    pub fiber: Fiber,
    pub exit: Arc<GuestExit>,
}

impl Event for GuestExited {
    type Output = ();
}
