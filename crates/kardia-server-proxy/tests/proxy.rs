use kardia_core::{Context, Result};
use kardia_http::HttpPlugin;
use kardia_server::{Server, ServerConfig, ServerExt, ServerPlugin, StatusCode};
use kardia_server_proxy::{ProxyConfig, ProxyPlugin};
use serde_json::json;

struct Fixture {
    upstream: Context,
    proxy: Context,
    url: String,
}
impl Fixture {
    async fn new(prefix: &str) -> Self {
        let upstream = Context::new();
        upstream.plugin::<ServerPlugin>(ServerConfig::local(0)).unwrap().settled().await.unwrap();
        let proxy = Context::new();
        proxy.plugin::<ServerPlugin>(ServerConfig::local(0)).unwrap().settled().await.unwrap();
        proxy.plugin::<HttpPlugin>(Default::default()).unwrap().settled().await.unwrap();
        proxy
            .plugin::<ProxyPlugin>(ProxyConfig {
                // A target is a prefix that ends with `/`; an empty prefix is the
                // root of the upstream.
                target: {
                    // A target is a prefix that ends with `/`; an empty prefix is
                    // the root of the upstream.
                    let base = upstream.get::<Server>().unwrap().public_url();
                    if prefix.is_empty() { format!("{base}/") } else { format!("{base}{prefix}") }
                },
                isolate_origin: false,
                request_headers: Default::default(),
            })
            .unwrap()
            .settled()
            .await
            .unwrap();
        let url = proxy.get::<Server>().unwrap().public_url();
        Self { upstream, proxy, url }
    }
    async fn close(self) {
        self.proxy.fiber().dispose().await.unwrap();
        self.upstream.fiber().dispose().await.unwrap();
    }
}
#[tokio::test]
async fn should_proxy_a_simple_get_request() {
    let f = Fixture::new("").await;
    f.upstream
        .get::<Server>()
        .unwrap()
        .get("/hello", |_, res, _| async move {
            res.set_body("world");
            Ok(None)
        })
        .unwrap();
    assert_eq!(reqwest::get(format!("{}/hello", f.url)).await.unwrap().text().await.unwrap(), "world");
    f.close().await;
}
#[tokio::test]
async fn should_proxy_nested_paths() {
    let f = Fixture::new("").await;
    f.upstream
        .get::<Server>()
        .unwrap()
        .get("/api/users/:id", |req, res, _| async move {
            res.set_body(req.params["id"].as_str().unwrap().to_owned());
            Ok(None)
        })
        .unwrap();
    assert_eq!(reqwest::get(format!("{}/api/users/42", f.url)).await.unwrap().text().await.unwrap(), "42");
    f.close().await;
}
#[tokio::test]
async fn should_forward_query_string() {
    let f = Fixture::new("").await;
    f.upstream
        .get::<Server>()
        .unwrap()
        .get("/search", |req, res, _| async move {
            res.set_body(req.query[0].1.clone());
            Ok(None)
        })
        .unwrap();
    assert_eq!(reqwest::get(format!("{}/search?q=hello", f.url)).await.unwrap().text().await.unwrap(), "hello");
    f.close().await;
}
#[tokio::test]
async fn should_proxy_root_path() {
    let f = Fixture::new("").await;
    f.upstream
        .get::<Server>()
        .unwrap()
        .get("/", |_, res, _| async move {
            res.set_body("root");
            Ok(None)
        })
        .unwrap();
    assert_eq!(reqwest::get(format!("{}/", f.url)).await.unwrap().text().await.unwrap(), "root");
    f.close().await;
}
#[tokio::test]
async fn should_forward_request_body() {
    let f = Fixture::new("").await;
    f.upstream
        .get::<Server>()
        .unwrap()
        .post("/echo", |req, res, _| async move {
            res.set_body(req.take_body()?);
            Ok(None)
        })
        .unwrap();
    assert_eq!(
        reqwest::Client::new()
            .post(format!("{}/echo", f.url))
            .body("hello proxy")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "hello proxy"
    );
    f.close().await;
}
#[tokio::test]
async fn should_forward_json_body() -> Result<()> {
    let f = Fixture::new("").await;
    f.upstream.get::<Server>()?.post("/json", |req, res, _| async move {
        res.set_json(&req.json().await?)?;
        Ok(None)
    })?;
    let r = reqwest::Client::new()
        .post(format!("{}/json", f.url))
        .header("content-type", "application/json")
        .body("{\"foo\":\"bar\"}")
        .send()
        .await?;
    assert_eq!(r.headers()["content-type"], "application/json");
    assert_eq!(r.text().await?, "{\"foo\":\"bar\"}");
    f.close().await;
    Ok(())
}
#[tokio::test]
async fn should_forward_upstream_status_codes() {
    let f = Fixture::new("").await;
    f.upstream
        .get::<Server>()
        .unwrap()
        .get("/not-found", |_, res, _| async move {
            res.set_status(StatusCode::NOT_FOUND);
            Ok(None)
        })
        .unwrap();
    assert_eq!(reqwest::get(format!("{}/not-found", f.url)).await.unwrap().status(), 404);
    f.close().await;
}
#[tokio::test]
async fn should_forward_upstream_response_headers() {
    let f = Fixture::new("").await;
    f.upstream
        .get::<Server>()
        .unwrap()
        .get("/headers", |_, res, _| async move {
            res.set_header("x-custom".parse().unwrap(), "value".parse().unwrap());
            res.set_body("ok");
            Ok(None)
        })
        .unwrap();
    assert_eq!(reqwest::get(format!("{}/headers", f.url)).await.unwrap().headers()["x-custom"], "value");
    f.close().await;
}
#[tokio::test]
async fn should_correctly_join_with_target_path() {
    let f = Fixture::new("/api/v1/").await;
    f.upstream
        .get::<Server>()
        .unwrap()
        .get("/api/v1/data", |_, res, _| async move {
            res.set_body("scoped");
            Ok(None)
        })
        .unwrap();
    assert_eq!(reqwest::get(format!("{}/data", f.url)).await.unwrap().text().await.unwrap(), "scoped");
    f.close().await;
}

#[tokio::test]
async fn should_preserve_encoded_path_characters_and_confine_requests() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let f = Fixture::new("/api/v1/").await;
    let hits = Arc::new(AtomicUsize::new(0));
    let observed = hits.clone();
    f.upstream
        .get::<Server>()
        .unwrap()
        .all("{/*path}", move |req, res, _| {
            observed.fetch_add(1, Ordering::SeqCst);
            async move {
                res.set_body(req.path);
                Ok(None)
            }
        })
        .unwrap();
    for (path, expected) in [
        ("a%3Fb%23c+d", "/api/v1/a%3Fb%23c+d"),
        ("a%25b", "/api/v1/a%25b"),
        // The path is forwarded as the client sent it: an encoded separator stays
        // literal in the upstream path.
        ("http:%2F%2Fevil.test/x", "/api/v1/http:%2F%2Fevil.test/x"),
        ("%2F%2Fevil.test/x", "/api/v1/%2F%2Fevil.test/x"),
        ("a%2F..%2Fb", "/api/v1/a%2F..%2Fb"),
    ] {
        assert_eq!(reqwest::get(format!("{}/{path}", f.url)).await.unwrap().text().await.unwrap(), expected);
    }
    let before = hits.load(Ordering::SeqCst);
    for path in ["..%2Fsecret", "%2e%2e%2fsecret", "..%5Csecret"] {
        let response = reqwest::get(format!("{}/{path}", f.url)).await.unwrap();
        // A path that would leave the target is refused before it is forwarded.
        assert_eq!(response.status(), 400, "{path}");
    }
    // None of the refused paths reached the upstream.
    assert_eq!(hits.load(Ordering::SeqCst), before);
    assert_eq!(reqwest::get(format!("{}/healthy", f.url)).await.unwrap().text().await.unwrap(), "/api/v1/healthy");
    f.close().await;
}

#[tokio::test]
async fn should_map_request_headers_and_consume_their_sources() {
    let f = Fixture::new("").await;
    f.upstream
        .get::<Server>()
        .unwrap()
        .get("/inspect", |req, res, _| async move {
            res.set_json(&json!({
                "authorization": req.headers.get("authorization").and_then(|v| v.to_str().ok()),
                "x-auth": req.headers.get("x-auth").and_then(|v| v.to_str().ok()),
                "x-untouched": req.headers.get("x-untouched").and_then(|v| v.to_str().ok()),
            }))?;
            Ok(None)
        })
        .unwrap();

    // A dedicated proxy instance so the fixture's own routes stay unchanged.
    let root = Context::new();
    root.plugin::<ServerPlugin>(ServerConfig::local(0)).unwrap().settled().await.unwrap();
    root.plugin::<HttpPlugin>(Default::default()).unwrap().settled().await.unwrap();
    let mut headers = std::collections::BTreeMap::new();
    headers.insert("authorization".to_owned(), "x-auth".to_owned());
    root.plugin::<ProxyPlugin>(ProxyConfig {
        target: format!("{}/", f.upstream.get::<Server>().unwrap().public_url()),
        isolate_origin: false,
        request_headers: headers,
    })
    .unwrap()
    .settled()
    .await
    .unwrap();
    let url = root.get::<Server>().unwrap().public_url();
    let body = reqwest::Client::new()
        .get(format!("{url}/inspect"))
        .header("x-auth", "secret")
        .header("x-untouched", "kept")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    // The mapping copies its source into the named upstream header and consumes it.
    assert_eq!(value["authorization"], "secret");
    assert_eq!(value["x-auth"], serde_json::Value::Null);
    assert_eq!(value["x-untouched"], "kept");
    root.fiber().dispose().await.unwrap();
    f.close().await;
}

#[tokio::test]
async fn isolate_origin_drops_credentials_and_sandboxes_documents() {
    let f = Fixture::new("").await;
    f.upstream
        .get::<Server>()
        .unwrap()
        .get("/cookie", |_, res, _| async move {
            res.set_header(
                kardia_server::HeaderName::from_static("set-cookie"),
                kardia_server::HeaderValue::from_static("session=1"),
            );
            res.set_header(
                kardia_server::HeaderName::from_static("clear-site-data"),
                kardia_server::HeaderValue::from_static("\"cookies\""),
            );
            res.set_body("document");
            Ok(None)
        })
        .unwrap();
    let root = Context::new();
    root.plugin::<ServerPlugin>(ServerConfig::local(0)).unwrap().settled().await.unwrap();
    root.plugin::<HttpPlugin>(Default::default()).unwrap().settled().await.unwrap();
    root.plugin::<ProxyPlugin>(ProxyConfig {
        target: format!("{}/", f.upstream.get::<Server>().unwrap().public_url()),
        isolate_origin: true,
        request_headers: Default::default(),
    })
    .unwrap()
    .settled()
    .await
    .unwrap();
    let url = root.get::<Server>().unwrap().public_url();
    let response = reqwest::get(format!("{url}/cookie")).await.unwrap();
    assert!(response.headers().get("set-cookie").is_none());
    assert!(response.headers().get("clear-site-data").is_none());
    assert_eq!(response.headers()["content-security-policy"], "sandbox");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    root.fiber().dispose().await.unwrap();
    f.close().await;
}

#[tokio::test]
async fn should_reject_a_target_with_a_query_or_a_fragment() {
    for suffix in ["?key=value", "#fragment"] {
        // A URL prefix ends with `/` and carries no query or fragment; only the
        // empty string, a scheme, or such a URL is forwarded.
        let upstream = Context::new();
        upstream.plugin::<ServerPlugin>(ServerConfig::local(0)).unwrap().settled().await.unwrap();
        let base = upstream.get::<Server>().unwrap().public_url();
        let root = Context::new();
        root.plugin::<ServerPlugin>(ServerConfig::local(0)).unwrap().settled().await.unwrap();
        root.plugin::<HttpPlugin>(Default::default()).unwrap().settled().await.unwrap();
        let result = root.plugin::<ProxyPlugin>(ProxyConfig {
            target: format!("{base}/api/v1{suffix}"),
            isolate_origin: false,
            request_headers: Default::default(),
        });
        // The plugin is created, and its load reports the rejection.
        assert!(result.unwrap().settled().await.is_err(), "{suffix} is rejected");
        root.fiber().dispose().await.unwrap();
        upstream.fiber().dispose().await.unwrap();
    }
}

#[tokio::test]
async fn should_confine_websockets_and_preserve_path_characters() {
    use futures::{SinkExt, StreamExt};
    use kardia_server::{WsExt, ws_handler};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio_tungstenite::{connect_async, tungstenite::Message};
    let f = Fixture::new("/api/v1/").await;
    let hits = Arc::new(AtomicUsize::new(0));
    let observed = hits.clone();
    f.upstream
        .get::<Server>()
        .unwrap()
        .ws(
            "{/*path}",
            Some(ws_handler(move |req, accept| {
                observed.fetch_add(1, Ordering::SeqCst);
                async move {
                    let socket = accept.accept().await?;
                    socket.send(kardia_server::Message::Text(req.path.into()))?;
                    while let Some(message) = socket.recv().await {
                        if message?.is_close() {
                            break;
                        }
                    }
                    Ok(())
                }
            })),
            serde_json::json!({}),
        )
        .unwrap();
    let url = f.url.replace("http:", "ws:");
    assert!(connect_async(format!("{url}/..%2Fsecret")).await.is_err());
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    let (mut socket, _) = connect_async(format!("{url}/a%3Fb%23c+d")).await.unwrap();
    assert_eq!(socket.next().await.unwrap().unwrap(), Message::Text("/api/v1/a%3Fb%23c+d".into()));
    socket.send(Message::Close(None)).await.unwrap();
    f.close().await;
}
