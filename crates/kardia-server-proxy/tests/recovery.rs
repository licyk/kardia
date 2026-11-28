use futures::{SinkExt, StreamExt};
use kardia_core::{Context, Error, FiberState, Inject, Plugin, RecoveryAction, RecoveryPolicy, Result};
use kardia_http::{Http, HttpPlugin};
use kardia_server::{Body, Bytes, Server, ServerConfig, ServerExt, ServerPlugin, WsExt, ws_handler};
use kardia_server_proxy::{ProxyConfig, ProxyPlugin};
use serde_json::json;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_tungstenite::{connect_async, tungstenite::Message};

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
            res.set_body("alive");
            Ok(None)
        })?;
        server.ws(
            "/healthy",
            Some(ws_handler(|_, accept| async move {
                let ws = accept.accept().await?;
                while let Some(m) = ws.recv().await {
                    let m = m?;
                    if m.is_close() {
                        break;
                    }
                    ws.send(m)?;
                }
                Ok(())
            })),
            json!({}),
        )?;
        Ok(())
    }
}
async fn inactive(http: &Http) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while http.active_operations() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn proxy_and_http_provider_failures_preserve_independent_http_and_websocket_in_every_policy() {
    for dependency in [false, true] {
        for mode in 0..3 {
            let upstream = Context::new();
            upstream.plugin::<ServerPlugin>(ServerConfig::local(0)).unwrap().settled().await.unwrap();
            upstream.plugin::<Healthy>(()).unwrap().settled().await.unwrap();
            upstream
                .get::<Server>()
                .unwrap()
                .get("/pending", |_, res, _| async move {
                    res.set_body(Body::stream(
                        futures::stream::once(async { Ok(Bytes::from_static(b"first")) })
                            .chain(futures::stream::pending()),
                    ));
                    Ok(None)
                })
                .unwrap();
            let root = Context::new();
            root.plugin::<ServerPlugin>(ServerConfig::local(0)).unwrap().settled().await.unwrap();
            root.plugin::<Healthy>(()).unwrap().settled().await.unwrap();
            let provider = root.plugin::<HttpPlugin>(Default::default()).unwrap();
            provider.settled().await.unwrap();
            // A target is a prefix that ends with `/`.
            let config = json!({"target": format!("{}/", upstream.get::<Server>().unwrap().public_url())});
            let proxy =
                root.intercept("server", json!({"path":"/proxy"})).plugin_value::<ProxyPlugin>(config.clone()).unwrap();
            proxy.settled().await.unwrap();
            let failed = if dependency { &provider } else { &proxy };
            failed
                .set_recovery(match mode {
                    0 => RecoveryPolicy::Manual,
                    1 => RecoveryPolicy::Retry { delay: Duration::from_millis(40) },
                    _ => RecoveryPolicy::host(|_| async { Ok(RecoveryAction::RetryAfter(Duration::from_millis(40))) }),
                })
                .unwrap();
            let server = root.get::<Server>().unwrap();
            let identity = server.impl_id();
            let url = server.public_url();
            let progress = Arc::new(AtomicUsize::new(0));
            let observed = progress.clone();
            let monitor_url = url.clone();
            let monitor = tokio::spawn(async move {
                let client = reqwest::Client::builder().timeout(Duration::from_secs(2)).build().unwrap();
                let (mut ws, _) =
                    connect_async(format!("{}/healthy", monitor_url.replace("http:", "ws:"))).await.unwrap();
                loop {
                    assert_eq!(
                        client.get(format!("{monitor_url}/healthy")).send().await.unwrap().text().await.unwrap(),
                        "alive"
                    );
                    ws.send(Message::Text("alive".into())).await.unwrap();
                    assert_eq!(ws.next().await.unwrap().unwrap(), Message::Text("alive".into()));
                    observed.fetch_add(1, Ordering::SeqCst);
                }
            });
            for _ in 0..3 {
                let http = root.get::<Http>().unwrap();
                let mut body = reqwest::get(format!("{url}/proxy/pending")).await.unwrap().bytes_stream();
                assert_eq!(body.next().await.unwrap().unwrap(), b"first"[..]);
                let (mut socket, _) =
                    connect_async(format!("{}/proxy/healthy", url.replace("http:", "ws:"))).await.unwrap();
                socket.send(Message::Binary(vec![0, 255].into())).await.unwrap();
                assert_eq!(socket.next().await.unwrap().unwrap(), Message::Binary(vec![0, 255].into()));
                let before = progress.load(Ordering::SeqCst);
                failed.fail(Error::msg("injected proxy/provider failure")).await.unwrap_err();
                inactive(&http).await;
                drop(body);
                drop(socket);
                assert_eq!(server.route_count(), 1);
                if mode == 0 {
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    assert_eq!(failed.state(), FiberState::Failed);
                    failed.update(if dependency { json!({}) } else { config.clone() }).unwrap().await.unwrap();
                }
                tokio::time::timeout(Duration::from_secs(3), async {
                    while proxy.state() != FiberState::Active {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                assert_eq!(server.route_count(), 2);
                assert_eq!(root.get::<Server>().unwrap().impl_id(), identity);
                assert_eq!(reqwest::get(format!("{url}/proxy/healthy")).await.unwrap().text().await.unwrap(), "alive");
                assert!(progress.load(Ordering::SeqCst) > before);
            }
            proxy.dispose().await.unwrap();
            inactive(&root.get::<Http>().unwrap()).await;
            assert_eq!(server.route_count(), 1);
            monitor.abort();
            let _ = monitor.await;
            root.fiber().dispose().await.unwrap();
            upstream.fiber().dispose().await.unwrap();
        }
    }
}
#[tokio::test]
async fn unavailable_upstream_is_request_local_and_recovers_without_restarting_proxy() {
    let upstream = Context::new();
    let listener = upstream.plugin::<ServerPlugin>(ServerConfig::local(0)).unwrap();
    listener.settled().await.unwrap();
    let address = upstream.get::<Server>().unwrap().address();
    // A target is a prefix that ends with `/`.
    let target = format!("{}/", upstream.get::<Server>().unwrap().public_url());
    let root = Context::new();
    root.plugin::<ServerPlugin>(ServerConfig::local(0)).unwrap().settled().await.unwrap();
    root.plugin::<Healthy>(()).unwrap().settled().await.unwrap();
    root.plugin::<HttpPlugin>(Default::default()).unwrap().settled().await.unwrap();
    let proxy = root
        .intercept("server", json!({"path":"/proxy"}))
        .plugin::<ProxyPlugin>(ProxyConfig { target, isolate_origin: false, request_headers: Default::default() })
        .unwrap();
    proxy.settled().await.unwrap();
    let url = root.get::<Server>().unwrap().public_url();
    let identity = root.get::<Server>().unwrap().impl_id();
    listener.dispose().await.unwrap();
    assert_eq!(reqwest::get(format!("{url}/proxy/healthy")).await.unwrap().status(), 500);
    assert!(connect_async(format!("{}/proxy/healthy", url.replace("http:", "ws:"))).await.is_err());
    inactive(&root.get::<Http>().unwrap()).await;
    assert_eq!(proxy.state(), FiberState::Active);
    upstream.plugin::<ServerPlugin>(ServerConfig::local(address.port())).unwrap().settled().await.unwrap();
    upstream.plugin::<Healthy>(()).unwrap().settled().await.unwrap();
    assert_eq!(reqwest::get(format!("{url}/proxy/healthy")).await.unwrap().text().await.unwrap(), "alive");
    assert_eq!(root.get::<Server>().unwrap().impl_id(), identity);
    root.fiber().dispose().await.unwrap();
    upstream.fiber().dispose().await.unwrap();
}
