use futures::StreamExt;
use kardia_core::{Context, Error, FiberState, Inject, Plugin, RecoveryAction, RecoveryPolicy, Result};
use kardia_http::{Http, HttpConfig, HttpExt, HttpPlugin, RequestConfig};
use kardia_server::{Body, Bytes, Server, ServerConfig, ServerExt, ServerPlugin};
use serde_json::json;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
struct Healthy;
impl Plugin for Healthy {
    const NAME: &'static str = "healthy";
    type Config = ();
    fn inject() -> Inject {
        Inject::new().with("server")
    }
    async fn apply(ctx: Context, _: Arc<()>) -> Result<()> {
        let server = ctx.get::<Server>()?;
        server.get("/healthy", |_, res, _| async move {
            res.set_body("healthy");
            Ok(None)
        })?;
        server.get("/pending", |_, res, _| async move {
            res.set_body(Body::stream(
                futures::stream::once(async { Ok(Bytes::from_static(b"first")) }).chain(futures::stream::pending()),
            ));
            Ok(None)
        })?;
        Ok(())
    }
}
struct Consumer;
impl Plugin for Consumer {
    const NAME: &'static str = "consumer";
    type Config = ();
    fn inject() -> Inject {
        Inject::new().with("server").with("http")
    }
    async fn apply(ctx: Context, _: Arc<()>) -> Result<()> {
        let server = ctx.get::<Server>()?;
        let http = ctx.get::<Http>()?;
        let url = server.public_url();
        server.get("/consumer", move |_, res, _| {
            let http = http.clone();
            let url = url.clone();
            async move {
                let response = http.request(&format!("{url}/healthy"), RequestConfig::default()).await?;
                res.set_body(Body::stream(response.body.into_stream()));
                Ok(None)
            }
        })?;
        Ok(())
    }
}
#[tokio::test]
async fn http_service_recovery_rebuilds_consumers_without_stopping_independent_plugins() {
    for mode in 0..3 {
        let root = Context::new();
        root.plugin::<ServerPlugin>(ServerConfig::local(0)).unwrap().settled().await.unwrap();
        root.plugin::<Healthy>(()).unwrap().settled().await.unwrap();
        let provider = root.plugin::<HttpPlugin>(HttpConfig::default()).unwrap();
        provider.settled().await.unwrap();
        provider
            .set_recovery(match mode {
                0 => RecoveryPolicy::Manual,
                1 => RecoveryPolicy::Retry { delay: Duration::from_millis(30) },
                _ => RecoveryPolicy::host(|_| async { Ok(RecoveryAction::RetryAfter(Duration::from_millis(30))) }),
            })
            .unwrap();
        let consumer = root.plugin::<Consumer>(()).unwrap();
        consumer.settled().await.unwrap();
        let server = root.get::<Server>().unwrap();
        let identity = server.impl_id();
        let url = server.public_url();
        let observed = Arc::new(AtomicUsize::new(0));
        let progress = observed.clone();
        let healthy_url = url.clone();
        let monitor = tokio::spawn(async move {
            let client = reqwest::Client::builder().timeout(Duration::from_secs(2)).build().unwrap();
            loop {
                assert_eq!(
                    client.get(format!("{healthy_url}/healthy")).send().await.unwrap().text().await.unwrap(),
                    "healthy"
                );
                progress.fetch_add(1, Ordering::SeqCst);
            }
        });
        for _ in 0..3 {
            let http = consumer.context().get::<Http>().unwrap();
            let response = http.request(&format!("{url}/pending"), RequestConfig::default()).await.unwrap();
            let mut body = response.body.into_stream();
            assert_eq!(body.next().await.unwrap().unwrap(), Bytes::from_static(b"first"));
            let before = observed.load(Ordering::SeqCst);
            provider.fail(Error::msg("HTTP instance failed")).await.unwrap_err();
            assert_eq!(http.active_operations(), 0);
            assert_eq!(server.route_count(), 2);
            drop(body);
            if mode == 0 {
                tokio::time::sleep(Duration::from_millis(30)).await;
                assert_eq!(provider.state(), FiberState::Failed);
                provider.update(json!({})).unwrap().await.unwrap();
            }
            tokio::time::timeout(Duration::from_secs(3), async {
                while consumer.state() != FiberState::Active {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(provider.state(), FiberState::Active);
            assert_eq!(server.route_count(), 3);
            assert_eq!(root.get::<Server>().unwrap().impl_id(), identity);
            assert!(observed.load(Ordering::SeqCst) > before);
            assert_eq!(reqwest::get(format!("{url}/consumer")).await.unwrap().text().await.unwrap(), "healthy");
        }
        provider.dispose().await.unwrap();
        assert_eq!(server.route_count(), 2);
        monitor.abort();
        let _ = monitor.await;
        root.fiber().dispose().await.unwrap();
    }
}
