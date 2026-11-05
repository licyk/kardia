//! Helpers for letting tasks run and advancing the paused clock.

use std::time::Duration;

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
