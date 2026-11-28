use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use kardia_core::{Context, Error, FiberState, RecoveryAction, RecoveryPolicy};
use kardia_server::{Message, Server, ServerConfig, ServerExt, ServerPlugin, WsExt, ws_handler};
use kardia_server_echo::EchoPlugin;
use serde_json::json;
use tokio_tungstenite::connect_async;

#[tokio::test]
async fn echo_failure_recovery_preserves_other_http_and_ws_plugins_in_all_modes() {
    for mode in 0..3 {
        let root = Context::new();
        root.plugin::<ServerPlugin>(ServerConfig::local(0)).unwrap().settled().await.unwrap();
        let server = root.get::<Server>().unwrap();
        let url = server.public_url();
        let ws = url.replace("http:", "ws:");
        server
            .get("/healthy", |_, res, _| async move {
                res.set_text("healthy");
                Ok(None)
            })
            .unwrap();
        let healthy = server
            .ws(
                "/healthy",
                Some(ws_handler(|_, accept| async move {
                    let socket = accept.accept().await?;
                    while let Some(message) = socket.recv().await {
                        let message = message?;
                        if message.is_close() {
                            break;
                        }
                        socket.send(message)?;
                    }
                    Ok(())
                })),
                json!({}),
            )
            .unwrap();
        let echo = root.intercept("std:server", json!({"path":"/echo"})).plugin::<EchoPlugin>(json!({})).unwrap();
        echo.settled().await.unwrap();
        echo.set_recovery(match mode {
            0 => RecoveryPolicy::Manual,
            1 => RecoveryPolicy::retry(Duration::from_millis(20)),
            _ => RecoveryPolicy::host(|_| async { Ok(RecoveryAction::RetryAfter(Duration::from_millis(20))) }),
        })
        .unwrap();
        let progress = Arc::new(AtomicUsize::new(0));
        let observed = progress.clone();
        let healthy_url = url.clone();
        let (mut monitor_socket, _) = connect_async(format!("{ws}/healthy")).await.unwrap();
        let monitor = tokio::spawn(async move {
            let client = reqwest::Client::builder().timeout(Duration::from_secs(2)).build().unwrap();
            loop {
                monitor_socket.send(Message::Text("alive".into())).await.unwrap();
                assert_eq!(monitor_socket.next().await.unwrap().unwrap(), Message::Text("alive".into()));
                assert_eq!(
                    client.get(format!("{healthy_url}/healthy")).send().await.unwrap().text().await.unwrap(),
                    "healthy"
                );
                observed.fetch_add(1, Ordering::SeqCst);
            }
        });
        for _ in 0..3 {
            let (mut socket, _) = connect_async(format!("{ws}/echo")).await.unwrap();
            socket.send(Message::Binary(vec![0, 255].into())).await.unwrap();
            assert_eq!(socket.next().await.unwrap().unwrap(), Message::Binary(vec![0, 255].into()));
            let before = progress.load(Ordering::SeqCst);
            echo.fail(Error::msg("echo failure")).await.unwrap_err();
            assert!(socket.next().await.unwrap().unwrap().is_close());
            assert_eq!(server.route_count(), 1);
            if mode == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
                assert_eq!(echo.state(), FiberState::Failed);
                echo.update(json!({})).unwrap().await.unwrap();
            } else {
                tokio::time::timeout(Duration::from_secs(2), async {
                    while echo.state() != FiberState::Active {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
            }
            assert!(progress.load(Ordering::SeqCst) > before);
            assert_eq!(healthy.client_count(), 1);
            assert_eq!(server.route_count(), 2);
            let res = reqwest::Client::new()
                .post(format!("{url}/echo"))
                .header("content-type", "text/custom")
                .body("recovered")
                .send()
                .await
                .unwrap();
            assert_eq!(res.headers()["content-type"], "text/custom");
            assert_eq!(res.text().await.unwrap(), "recovered");
        }
        echo.dispose().await.unwrap();
        assert_eq!(server.route_count(), 1);
        monitor.abort();
        let _ = monitor.await;
        root.fiber().dispose().await.unwrap();
        assert_eq!(healthy.client_count(), 0);
    }
}
