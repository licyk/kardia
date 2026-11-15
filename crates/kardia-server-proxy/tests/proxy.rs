use kardia_core::{Context, Result};
use kardia_http::HttpPlugin;
use kardia_server::{Server, ServerConfig, ServerExt, ServerPlugin, StatusCode};
use kardia_server_proxy::{ProxyConfig, ProxyPlugin, ServerProxy};

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
                target: format!("{}{prefix}", upstream.get::<Server>().unwrap().public_url()),
            })
            .unwrap()
            .settled()
            .await
            .unwrap();
        let url = proxy.get::<ServerProxy>().unwrap().public_url().unwrap();
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
        ("http:%2F%2Fevil.test/x", "/api/v1/http://evil.test/x"),
        ("%2F%2Fevil.test/x", "/api/v1///evil.test/x"),
        ("a%2F..%2Fb", "/api/v1/b"),
    ] {
        assert_eq!(reqwest::get(format!("{}/{path}", f.url)).await.unwrap().text().await.unwrap(), expected);
    }
    let before = hits.load(Ordering::SeqCst);
    for path in ["..%2Fsecret", "%2e%2e%2fsecret", "..%5Csecret"] {
        let response = reqwest::get(format!("{}/{path}", f.url)).await.unwrap();
        // A literal backslash is a segment character, not a path separator.
        if path.contains("%5C") {
            assert_eq!(response.text().await.unwrap(), "/api/v1/..%5Csecret");
        } else {
            assert_eq!(response.status(), 400);
        }
    }
    assert_eq!(hits.load(Ordering::SeqCst), before + 1);
    assert_eq!(reqwest::get(format!("{}/healthy", f.url)).await.unwrap().text().await.unwrap(), "/api/v1/healthy");
    f.close().await;
}

#[tokio::test]
async fn should_treat_target_path_as_directory_with_query_or_fragment() {
    for suffix in ["?key=value", "#fragment"] {
        let f = Fixture::new(&format!("/api/v1{suffix}")).await;
        f.upstream
            .get::<Server>()
            .unwrap()
            .get("/api/v1/users", |req, res, _| async move {
                res.set_json(&req.query)?;
                Ok(None)
            })
            .unwrap();
        assert_eq!(
            reqwest::get(format!("{}/users?q=a+b", f.url)).await.unwrap().text().await.unwrap(),
            "[[\"q\",\"a b\"]]"
        );
        f.close().await;
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
    let f = Fixture::new("/api/v1?ignored=yes").await;
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
