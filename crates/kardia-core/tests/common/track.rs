//! Recording when spawned futures settle.

use super::Seq;

/// Spawns a future and records when it settles (`true` on success).
pub fn track<F>(future: F, settled: &Seq<bool>) -> tokio::task::JoinHandle<()>
where
    F: std::future::Future<Output = kardia_core::Result<()>> + Send + 'static,
{
    let settled = settled.clone();
    tokio::spawn(async move {
        let ok = future.await.is_ok();
        settled.push(ok);
    })
}
