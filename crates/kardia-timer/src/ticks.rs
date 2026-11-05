//! Ticks of an interval, read one at a time.

use std::sync::Arc;

use kardia_core::{EffectHandle, Result};
use parking_lot::Mutex;
use tokio::sync::Notify;

use crate::service::Disposed;

pub(crate) struct TickState {
    pub(crate) notify: Notify,
    pub(crate) closed: Mutex<bool>,
}

/// Ticks of an interval. Dropping it disposes the interval.
pub struct Ticks {
    pub(crate) state: Arc<TickState>,
    pub(crate) handle: EffectHandle,
}

impl Ticks {
    /// Waits for the next tick; fails with [`Disposed`] once the interval
    /// (or the context that started it) has been disposed.
    pub async fn tick(&mut self) -> Result<(), Disposed> {
        let notified = self.state.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if *self.state.closed.lock() {
            return Err(Disposed);
        }
        notified.await;
        if *self.state.closed.lock() { Err(Disposed) } else { Ok(()) }
    }

    pub fn close(self) {}
}

impl Drop for Ticks {
    fn drop(&mut self) {
        drop(self.handle.dispose());
    }
}
