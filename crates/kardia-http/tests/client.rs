use futures::{StreamExt, stream};
use kardia_core::{Context, PluginDef};
use kardia_http::{
    Body, Bytes, Http, HttpConfig, HttpExt, HttpPlugin, Message, Method, Redirect, RequestConfig, WebSocketExt,
};
use kardia_server::{Server, ServerConfig, ServerExt, ServerPlugin, WsExt, ws_handler};
use serde_json::{Value, json};
use std::{io::Write, time::Duration};

async fn setup() -> (Context, String, Context) {
    let target = Context::new();
    target.plugin::<ServerPlugin>(ServerConfig::local(0)).unwrap().settled().await.unwrap();
    let server = target.get::<Server>().unwrap();
    server
        .get("/status", |_, res, _| async move {
            res.set_status(kardia_server::StatusCode::IM_A_TEAPOT);
            res.set_status_text("Custom reason".into())?;
            res.set_header("x-original".parse()?, "yes".parse()?);
            res.set_body("failure body");
            Ok(None)
        })
        .unwrap();
    server.all("/inspect",|req,res,_|async move{let body=req.text().await?;res.set_json(&json!({"method":req.method.as_str(),"query":req.query,"a":req.headers.get("x-a").and_then(|v|v.to_str().ok()),"b":req.headers.get("x-b").and_then(|v|v.to_str().ok()),"body":body}))?;Ok(None)}).unwrap();
    server
        .get("/slow", |_, res, _| async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            res.set_body("late");
            Ok(None)
        })
        .unwrap();
    server
        .get("/stream", |_, res, _| async move {
            res.set_body(kardia_server::Body::stream(stream::once(async { Ok(Bytes::from_static(b"first")) }).chain(
                stream::once(async {
                    tokio::time::sleep(Duration::from_millis(80)).await;
                    Ok(Bytes::from_static(b"last"))
                }),
            )));
            Ok(None)
        })
        .unwrap();
    server
        .all("/redirect/:status", |req, res, _| async move {
            res.set_status(kardia_server::StatusCode::from_u16(
                req.params["status"].as_str().unwrap().parse().unwrap(),
            )?);
            res.set_header("location".parse()?, "/inspect".parse()?);
            Ok(None)
        })
        .unwrap();
    server
        .get("/gzip", |_, res, _| async move {
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            gz.write_all(b"compressed text")?;
            let body = gz.finish()?;
            res.set_header("content-encoding".parse()?, "gzip".parse()?);
            res.set_header("content-length".parse()?, body.len().to_string().parse()?);
            res.set_body(body);
            Ok(None)
        })
        .unwrap();
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
    let url = server.public_url();
    let client = Context::new();
    client
        .plugin::<HttpPlugin>(HttpConfig {
            base_url: Some(format!("{url}/")),
            headers: Some([("x-a".into(), "default".into())].into()),
            ..Default::default()
        })
        .unwrap()
        .settled()
        .await
        .unwrap();
    (target, url, client)
}
async fn idle(http: &Http) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while http.active_operations() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn a_data_url_answers_from_its_own_content() {
    let (_, _, client) = setup().await;
    let http = client.get::<Http>().unwrap();

    let res = http.request("data:text/plain,hello", RequestConfig::default()).await.unwrap();
    assert_eq!(res.status, 200);
    assert_eq!(res.headers["content-type"], "text/plain");
    assert_eq!(res.body.bytes().await.unwrap(), b"hello"[..]);

    // No media type declares the default plain-text type with a charset.
    let res = http.request("data:,plain", RequestConfig::default()).await.unwrap();
    assert_eq!(res.headers["content-type"], "text/plain;charset=US-ASCII");

    // Base64 carries bytes that are not text.
    let res = http.request("data:application/octet-stream;base64,AAEC", RequestConfig::default()).await.unwrap();
    assert_eq!(res.body.bytes().await.unwrap(), vec![0u8, 1, 2][..]);

    // Only GET is answered, as upstream does.
    let config = RequestConfig { method: Some(kardia_http::Method::POST), ..Default::default() };
    let res = http.request("data:text/plain,hello", config).await.unwrap();
    assert_eq!(res.status, 405);

    // A malformed URL fails the request instead of answering.
    assert!(http.request("data:text/plain", RequestConfig::default()).await.is_err());
    idle(&http).await;
}

#[tokio::test]
async fn raw_status_headers_query_and_layered_configuration() {
    let (target, _, client) = setup().await;
    let http = client.get::<Http>().unwrap();
    let res = http.request("status", RequestConfig::default()).await.unwrap();
    assert_eq!(res.status, 418);
    assert_eq!(res.status_text, "Custom reason");
    assert_eq!(res.headers["x-original"], "yes");
    assert_eq!(res.body.bytes().await.unwrap(), b"failure body"[..]);
    let scoped = client
        .intercept("std:http", json!({"headers":{"x-a":"outer"}}))
        .intercept("std:http", json!({"headers":{"x-b":"inner"}}))
        .get::<Http>()
        .unwrap();
    let res = scoped
        .request(
            "inspect?a=1",
            RequestConfig {
                method: Some(Method::POST),
                params: vec![("a".into(), "2".into()), ("中文".into(), "a b".into())],
                body: Some(Body::from("payload")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&res.body.bytes().await.unwrap()).unwrap();
    assert_eq!(
        value,
        json!({"method":"POST","query":[["a","1"],["a","2"],["中文","a b"]],"a":null,"b":"inner","body":"payload"})
    );
    idle(&http).await;
    client.fiber().dispose().await.unwrap();
    target.fiber().dispose().await.unwrap();
}
#[tokio::test]
async fn timeout_stops_at_headers_and_raw_response_keeps_compression_metadata() {
    let (target, _, client) = setup().await;
    let http = client.get::<Http>().unwrap();
    let error = http.request("slow", RequestConfig { timeout: Some(10), ..Default::default() }).await.err().unwrap();
    assert!(error.to_string().contains("TIMEOUT"));
    idle(&http).await;
    let response = http.request("stream", RequestConfig { timeout: Some(20), ..Default::default() }).await.unwrap();
    assert_eq!(response.body.bytes().await.unwrap(), b"firstlast"[..]);
    let response = http.request("gzip", RequestConfig::default()).await.unwrap();
    assert_eq!(response.headers["content-encoding"], "gzip");
    assert!(response.headers.contains_key("content-length"));
    assert_eq!(response.body.bytes().await.unwrap(), b"compressed text"[..]);
    idle(&http).await;
    client.fiber().dispose().await.unwrap();
    target.fiber().dispose().await.unwrap();
}
#[tokio::test]
async fn redirects_preserve_fetch_stream_replay_rules() {
    let (target, _, client) = setup().await;
    let http = client.get::<Http>().unwrap();
    for status in [301, 302, 303, 307, 308] {
        let response = http
            .request(
                &format!("redirect/{status}"),
                RequestConfig { method: Some(Method::POST), body: Some(Body::from("bytes")), ..Default::default() },
            )
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&response.body.bytes().await.unwrap()).unwrap();
        assert_eq!(value["method"], if status == 307 || status == 308 { "POST" } else { "GET" });
        let result = http
            .request(
                &format!("redirect/{status}"),
                RequestConfig {
                    method: Some(Method::POST),
                    body: Some(Body::stream(stream::once(async { Ok(Bytes::from_static(b"stream")) }))),
                    ..Default::default()
                },
            )
            .await;
        if status == 303 {
            assert_eq!(result.unwrap().status, 200);
        } else {
            assert!(result.is_err());
        }
    }
    assert_eq!(
        http.request("redirect/302", RequestConfig { redirect: Redirect::Manual, ..Default::default() })
            .await
            .unwrap()
            .status,
        302
    );
    assert!(
        http.request("redirect/302", RequestConfig { redirect: Redirect::Error, ..Default::default() }).await.is_err()
    );
    idle(&http).await;
    client.fiber().dispose().await.unwrap();
    target.fiber().dispose().await.unwrap();
}
#[tokio::test]
async fn caller_disposal_closes_its_http_and_websocket_operations() {
    let (target, _, client) = setup().await;
    let definition = PluginDef::new(|_, _| async { Ok(()) }).inject(["std:http"]);
    let consumer = client.plugin_def(&definition, Value::Null).unwrap();
    consumer.settled().await.unwrap();
    let http = consumer.context().get::<Http>().unwrap();
    let socket = http.ws("ws", RequestConfig::default()).await.unwrap();
    socket.send(Message::Binary(vec![0, 255].into())).unwrap();
    assert_eq!(socket.recv().await.unwrap().unwrap(), Message::Binary(vec![0, 255].into()));
    let response = http.request("stream", RequestConfig::default()).await.unwrap();
    let mut body = response.body.into_stream();
    assert_eq!(body.next().await.unwrap().unwrap(), Bytes::from_static(b"first"));
    consumer.dispose().await.unwrap();
    idle(&http).await;
    assert!(body.next().await.is_none());
    assert!(socket.recv().await.is_none());
    assert_eq!(client.get::<Http>().unwrap().request("inspect", RequestConfig::default()).await.unwrap().status, 200);
    drop(body);
    drop(socket);
    client.fiber().dispose().await.unwrap();
    target.fiber().dispose().await.unwrap();
}
#[tokio::test]
async fn dropped_connect_and_failed_handshake_release_effects() {
    let (target, _, client) = setup().await;
    let http = client.get::<Http>().unwrap();
    assert!(http.ws("missing", RequestConfig::default()).await.is_err());
    idle(&http).await;
    let request = http.request("slow", RequestConfig::default());
    assert!(tokio::time::timeout(Duration::from_millis(5), request).await.is_err());
    idle(&http).await;
    client.fiber().dispose().await.unwrap();
    target.fiber().dispose().await.unwrap();
}
