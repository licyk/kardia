use std::time::Duration;

use futures::{SinkExt, StreamExt};
use kardia_core::{Context, Error, Svc};
use kardia_server::{Message, Server, ServerConfig, ServerExt, ServerPlugin, WsExt, ws_handler};
use serde_json::json;
use tokio_tungstenite::{connect_async, tungstenite};

async fn setup() -> (Context, Svc<Server>, String) {
    let root = Context::new();
    root.plugin::<ServerPlugin>(ServerConfig::local(0)).unwrap().settled().await.unwrap();
    let server = root.get::<Server>().unwrap();
    let url = server.public_url().replace("http:", "ws:");
    (root, server, url)
}
async fn until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn should_accept_ws_connection_on_matched_route() {
    let (root, server, url) = setup().await;
    server.ws("/ws", None, json!({})).unwrap();
    let (mut socket, response) = connect_async(format!("{url}/ws")).await.unwrap();
    assert_eq!(response.status(), 101);
    socket.close(None).await.unwrap();
    root.fiber().dispose().await.unwrap();
}
#[tokio::test]
async fn should_return_404_for_unmatched_ws_route() {
    let (root, _, url) = setup().await;
    let tungstenite::Error::Http(response) = connect_async(format!("{url}/missing")).await.unwrap_err() else {
        panic!("expected HTTP refusal")
    };
    assert_eq!(response.status(), 404);
    root.fiber().dispose().await.unwrap();
}
#[tokio::test]
async fn should_track_clients_and_clean_up_on_close() {
    let (root, server, url) = setup().await;
    let route = server.ws("/ws", None, json!({})).unwrap();
    let (mut socket, _) = connect_async(format!("{url}/ws")).await.unwrap();
    until(|| route.client_count() == 1).await;
    socket.close(None).await.unwrap();
    let _ = socket.next().await;
    until(|| route.client_count() == 0).await;
    root.fiber().dispose().await.unwrap();
}
#[tokio::test]
async fn should_close_clients_on_route_dispose() {
    let (root, server, url) = setup().await;
    let route = server.ws("/ws", None, json!({})).unwrap();
    let (mut socket, _) = connect_async(format!("{url}/ws")).await.unwrap();
    until(|| route.client_count() == 1).await;
    route.dispose().await.unwrap();
    assert!(socket.next().await.unwrap().unwrap().is_close());
    assert_eq!(route.client_count(), 0);
    root.fiber().dispose().await.unwrap();
}
#[tokio::test]
async fn should_support_ws_handler_with_accept() {
    let (root, server, url) = setup().await;
    server
        .ws(
            "/ws",
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
    let (mut socket, _) = connect_async(format!("{url}/ws")).await.unwrap();
    for message in [Message::Text("hello".into()), Message::Binary(vec![0, 1, 255].into())] {
        socket.send(message.clone()).await.unwrap();
        assert_eq!(socket.next().await.unwrap().unwrap(), message);
    }
    socket.close(None).await.unwrap();
    root.fiber().dispose().await.unwrap();
}
#[tokio::test]
async fn should_return_500_when_ws_handler_throws() {
    let (root, server, url) = setup().await;
    server.ws("/ws", Some(ws_handler(|_, _| async { Err(Error::msg("ws error")) })), json!({})).unwrap();
    let tungstenite::Error::Http(response) = connect_async(format!("{url}/ws")).await.unwrap_err() else {
        panic!("expected HTTP refusal")
    };
    assert_eq!(response.status(), 500);
    root.fiber().dispose().await.unwrap();
}
#[tokio::test]
async fn should_destroy_socket_when_handler_does_not_accept() {
    let (root, server, url) = setup().await;
    server.ws("/ws", Some(ws_handler(|_, _| async { Ok(()) })), json!({})).unwrap();
    assert!(connect_async(format!("{url}/ws")).await.is_err());
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn panics_before_and_after_accept_leave_other_connections_and_http_serving() {
    let (root, server, url) = setup().await;
    server
        .ws(
            "/good",
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
    server
        .ws(
            "/before",
            Some(ws_handler(|_, _| {
                panic!("before future");
                #[allow(unreachable_code)]
                async {
                    Ok(())
                }
            })),
            json!({}),
        )
        .unwrap();
    server
        .ws(
            "/after",
            Some(ws_handler(|_, accept| async move {
                let _socket = accept.accept().await?;
                tokio::task::yield_now().await;
                panic!("after accept")
            })),
            json!({}),
        )
        .unwrap();
    server
        .get("/healthy", |_, res, _| async move {
            res.set_body("healthy");
            Ok(None)
        })
        .unwrap();
    let (mut good, _) = connect_async(format!("{url}/good")).await.unwrap();
    for _ in 0..3 {
        assert!(connect_async(format!("{url}/before")).await.is_err());
        let (mut bad, _) = connect_async(format!("{url}/after")).await.unwrap();
        assert!(bad.next().await.unwrap().unwrap().is_close());
        good.send(Message::Text("alive".into())).await.unwrap();
        assert_eq!(good.next().await.unwrap().unwrap(), Message::Text("alive".into()));
        assert_eq!(
            reqwest::get(format!("{}/healthy", server.public_url())).await.unwrap().text().await.unwrap(),
            "healthy"
        );
    }
    good.close(None).await.unwrap();
    root.fiber().dispose().await.unwrap();
}
