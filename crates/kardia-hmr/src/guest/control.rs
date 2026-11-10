//! The deadline and the cancellation of a guest call.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use parking_lot::Mutex;

type Wake = Box<dyn FnOnce() + Send>;

/// What bounds a guest call: a deadline, at which a transport stops the
/// guest, and a cancellation, after which the caller no longer wants the
/// result. Clones share the cancellation.
#[derive(Clone, Default)]
pub struct CallControl {
    deadline: Option<Instant>,
    cancel: Arc<Cancel>,
}

#[derive(Default)]
struct Cancel {
    cancelled: AtomicBool,
    wakers: Mutex<Vec<Wake>>,
}

impl CallControl {
    /// A call ending at `deadline`; `None` leaves it to the transport's default.
    pub fn new(deadline: Option<Instant>) -> Self {
        Self { deadline, cancel: Arc::default() }
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.cancelled.load(Ordering::Acquire)
    }

    /// Cancels the call and runs its wakers.
    pub fn cancel(&self) {
        if self.cancel.cancelled.swap(true, Ordering::AcqRel) {
            return;
        }
        let wakers = std::mem::take(&mut *self.cancel.wakers.lock());
        for wake in wakers {
            wake();
        }
    }

    /// Runs `wake` when the call is cancelled, at once if it already is.
    pub fn on_cancel(&self, wake: impl FnOnce() + Send + 'static) {
        let mut wakers = self.cancel.wakers.lock();
        if self.is_cancelled() {
            drop(wakers);
            wake();
            return;
        }
        wakers.push(Box::new(wake));
    }
}
