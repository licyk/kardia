#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

/// A shared, ordered record of observations.
#[derive(Clone, Default)]
pub struct Seq<T>(Arc<Mutex<Vec<T>>>);

impl<T: Clone> Seq<T> {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(Vec::new())))
    }

    pub fn push(&self, value: T) {
        self.0.lock().push(value);
    }

    pub fn get(&self) -> Vec<T> {
        self.0.lock().clone()
    }

    pub fn len(&self) -> usize {
        self.0.lock().len()
    }

    pub fn clear(&self) {
        self.0.lock().clear();
    }

    /// A closure pushing `value`, for use as a disposer.
    pub fn pusher(&self, value: T) -> impl FnOnce() + Send + 'static
    where
        T: Send + 'static,
    {
        let seq = self.clone();
        move || seq.push(value)
    }
}

/// Lets every runnable task make progress (the analogue of `setImmediate`).
pub async fn settle() {
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
}

/// Advances the paused clock by `ms`, firing due timers, then settles.
pub async fn advance(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
    settle().await;
}

pub async fn sleep(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

/// A one-shot gate (the analogue of `Promise.withResolvers<void>()`).
#[derive(Clone)]
pub struct Gate(tokio::sync::watch::Sender<bool>);

impl Gate {
    pub fn new() -> Self {
        Self(tokio::sync::watch::channel(false).0)
    }

    pub fn open(&self) {
        self.0.send_replace(true);
    }

    pub async fn wait(&self) {
        let mut rx = self.0.subscribe();
        let _ = rx.wait_for(|open| *open).await;
    }
}
