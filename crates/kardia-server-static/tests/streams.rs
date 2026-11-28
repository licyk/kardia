use futures::{StreamExt, stream};
use kardia_core::{Context, Error, FiberState, Inject, Plugin, RecoveryAction, RecoveryPolicy, Result};
use kardia_fs::{Bytes, FileStream, Fs, FsCode, FsError, FsExt, FsPlugin, FsProvider, Url};
use kardia_server::{Server, ServerConfig, ServerExt, ServerPlugin};
use kardia_server_static::StaticPlugin;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

struct Tracked(Arc<AtomicUsize>);
impl Drop for Tracked {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
struct Provider {
    bytes: Bytes,
    chunk: usize,
    active: Arc<AtomicUsize>,
}
impl FsProvider for Provider {
    fn read(&self, url: Url) -> FileStream {
        if url.path() == "/panic" {
            panic!("provider invocation");
        }
        self.active.fetch_add(1, Ordering::SeqCst);
        let guard = Tracked(self.active.clone());
        let bytes = self.bytes.clone();
        let chunk = self.chunk;
        stream::unfold((0, guard), move |(offset, guard)| {
            let bytes = bytes.clone();
            let path = url.path().to_owned();
            async move {
                if offset == 0 && matches!(path.as_str(), "/denied" | "/absent" | "/busy" | "/invalid") {
                    let code = match path.as_str() {
                        "/denied" => FsCode::PermissionDenied,
                        "/absent" => FsCode::NotFound,
                        "/invalid" => FsCode::InvalidPath,
                        _ => FsCode::Busy,
                    };
                    return Some((Err(FsError::new(code, None, "read failed").into()), (1, guard)));
                }
                if path == "/poll-panic" {
                    panic!("provider future");
                }
                if offset > 0 && path == "/broken" {
                    return Some((Err(Error::msg("read failed after first chunk")), (offset + 1, guard)));
                }
                if offset > 0 && path == "/pending" {
                    std::future::pending::<()>().await;
                }
                if offset >= bytes.len() {
                    return None;
                }
                let end = (offset + chunk).min(bytes.len());
                Some((Ok(bytes.slice(offset..end)), (end, guard)))
            }
        })
        .boxed()
    }
}
async fn setup(bytes: Bytes, chunk: usize) -> (Context, kardia_core::Fiber, Arc<AtomicUsize>) {
    let root = Context::new();
    root.plugin::<FsPlugin>(Default::default()).unwrap().settled().await.unwrap();
    root.plugin::<ServerPlugin>(ServerConfig::local(0)).unwrap().settled().await.unwrap();
    let active = Arc::new(AtomicUsize::new(0));
    root.get::<Fs>().unwrap().mount("memory:", Arc::new(Provider { bytes, chunk, active: active.clone() })).unwrap();
    let plugin = root.plugin_value::<StaticPlugin>(json!({"root":"memory:/"})).unwrap();
    plugin.settled().await.unwrap();
    (root, plugin, active)
}
async fn inactive(active: &AtomicUsize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while active.load(Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn should_detect_the_type_of_content_split_into_small_chunks() {
    let bytes = Bytes::from_static(&[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 31, 21,
        196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 218, 99, 100, 96, 248, 95, 15, 0, 2, 135, 1, 128, 235, 78, 158,
        146, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ]);
    let (root, _, active) = setup(bytes.clone(), 8).await;
    let url = root.get::<Server>().unwrap().public_url();
    let response = reqwest::get(format!("{url}/image")).await.unwrap();
    assert_eq!(response.headers()["content-type"], "image/png");
    assert_eq!(response.bytes().await.unwrap(), bytes);
    inactive(&active).await;
    root.fiber().dispose().await.unwrap();
}
#[tokio::test]
async fn should_detect_the_type_of_content_beyond_the_first_chunk() {
    let mut bytes = vec![0; 30];
    bytes[..4].copy_from_slice(&[80, 75, 3, 4]);
    bytes[18..22].copy_from_slice(&(192u32 * 1024).to_le_bytes());
    bytes.resize(30 + 192 * 1024, 0);
    bytes.extend([80, 75, 1, 2]);
    let (root, _, active) = setup(bytes.clone().into(), 65536).await;
    let url = root.get::<Server>().unwrap().public_url();
    let response = reqwest::get(format!("{url}/archive")).await.unwrap();
    assert_eq!(response.headers()["content-type"], "application/zip");
    assert_eq!(response.bytes().await.unwrap().len(), bytes.len());
    inactive(&active).await;
    root.fiber().dispose().await.unwrap();
}
#[tokio::test]
async fn read_failures_and_panics_are_contained_and_cancellation_closes_streams() {
    let (root, plugin, active) = setup(Bytes::from_static(b"BM012345678901234567890123456789"), 16).await;
    let url = root.get::<Server>().unwrap().public_url();
    let client = reqwest::Client::new();
    for (path, status) in
        [("denied", 403), ("absent", 404), ("busy", 500), ("invalid", 404), ("panic", 500), ("poll-panic", 500)]
    {
        let r = client.get(format!("{url}/{path}")).send().await.unwrap();
        assert_eq!(r.status(), status);
        r.bytes().await.unwrap();
        inactive(&active).await;
        assert_eq!(client.get(format!("{url}/ok")).send().await.unwrap().status(), 200);
    }
    let response = client.get(format!("{url}/broken")).send().await;
    assert!(match response {
        Ok(r) => r.bytes().await.is_err(),
        Err(_) => true,
    });
    inactive(&active).await;
    let mut response = client.get(format!("{url}/pending")).send().await.unwrap().bytes_stream();
    assert!(response.next().await.unwrap().is_ok());
    drop(response);
    inactive(&active).await;
    let mut response = client.get(format!("{url}/pending")).send().await.unwrap().bytes_stream();
    assert!(response.next().await.unwrap().is_ok());
    plugin.dispose().await.unwrap();
    inactive(&active).await;
    drop(response);
    root.fiber().dispose().await.unwrap();
}
struct Healthy;
impl Plugin for Healthy {
    const NAME: &'static str = "healthy";
    type Config = ();
    fn inject() -> Inject {
        Inject::new().with("std:server")
    }
    async fn apply(ctx: Context, _: Arc<()>) -> Result<()> {
        ctx.get::<Server>()?.get("/healthy", |_, res, _| async move {
            res.set_body("alive");
            Ok(None)
        })?;
        Ok(())
    }
}
#[tokio::test]
async fn every_recovery_policy_releases_failed_static_streams_and_preserves_independent_plugin() {
    for mode in 0..3 {
        let (root, initial, active) = setup(Bytes::from_static(b"BM012345678901234567890123456789"), 16).await;
        initial.dispose().await.unwrap();
        root.plugin::<Healthy>(()).unwrap().settled().await.unwrap();
        let config = json!({"root":"memory:/"});
        let plugin = root.plugin_value::<StaticPlugin>(config.clone()).unwrap();
        plugin.settled().await.unwrap();
        plugin
            .set_recovery(match mode {
                0 => RecoveryPolicy::Manual,
                1 => RecoveryPolicy::retry(Duration::from_millis(30)),
                _ => RecoveryPolicy::host(|_| async { Ok(RecoveryAction::RetryAfter(Duration::from_millis(30))) }),
            })
            .unwrap();
        let server = root.get::<Server>().unwrap();
        let url = server.public_url();
        let progress = Arc::new(AtomicUsize::new(0));
        let counter = progress.clone();
        let healthy_url = url.clone();
        let monitor = tokio::spawn(async move {
            let client = reqwest::Client::builder().timeout(Duration::from_secs(2)).build().unwrap();
            loop {
                assert_eq!(
                    client.get(format!("{healthy_url}/healthy")).send().await.unwrap().text().await.unwrap(),
                    "alive"
                );
                counter.fetch_add(1, Ordering::SeqCst);
            }
        });
        for _ in 0..3 {
            let mut response = reqwest::get(format!("{url}/pending")).await.unwrap().bytes_stream();
            assert!(response.next().await.unwrap().is_ok());
            let before = progress.load(Ordering::SeqCst);
            plugin.fail(Error::msg("static instance failed")).await.unwrap_err();
            inactive(&active).await;
            assert_eq!(server.route_count(), 1);
            drop(response);
            if mode == 0 {
                tokio::time::sleep(Duration::from_millis(30)).await;
                assert_eq!(plugin.state(), FiberState::Failed);
                plugin.update(config.clone()).unwrap().await.unwrap();
            } else {
                tokio::time::timeout(Duration::from_secs(2), async {
                    while plugin.state() != FiberState::Active {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
            }
            assert_eq!(server.route_count(), 2);
            assert!(progress.load(Ordering::SeqCst) > before);
            assert_eq!(
                reqwest::get(format!("{url}/ok")).await.unwrap().bytes().await.unwrap(),
                b"BM012345678901234567890123456789"[..]
            );
            inactive(&active).await;
        }
        plugin.dispose().await.unwrap();
        assert_eq!(server.route_count(), 1);
        monitor.abort();
        let _ = monitor.await;
        root.fiber().dispose().await.unwrap();
        inactive(&active).await;
    }
}
