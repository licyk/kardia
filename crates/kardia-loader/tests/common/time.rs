//! Letting runnable tasks make progress.

pub async fn settle() {
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
}
