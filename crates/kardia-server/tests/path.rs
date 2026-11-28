//! Port of `packages/server/core/tests/index.spec.ts` for malformed paths.

use kardia_core::{Context, Svc};
use kardia_server::{Server, ServerConfig, ServerExt, ServerPlugin, StatusCode};

async fn serve() -> (Context, Svc<Server>) {
    let root = Context::new();
    root.plugin::<ServerPlugin>(ServerConfig::local(0)).unwrap().settled().await.unwrap();
    let server = root.get::<Server>().unwrap();
    (root, server)
}

async fn status(server: &Svc<Server>, path: &str) -> StatusCode {
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    client.get(format!("http://{}{path}", server.address())).send().await.unwrap().status()
}

#[tokio::test]
async fn a_malformed_percent_escape_is_a_bad_request() {
    let (root, server) = serve().await;
    server
        .get("{/*path}", |_, res, _| async move {
            res.set_body("matched");
            Ok(None)
        })
        .unwrap();
    assert_eq!(status(&server, "/%zz").await, StatusCode::BAD_REQUEST);
    assert_eq!(status(&server, "/a%2").await, StatusCode::BAD_REQUEST);
    assert_eq!(status(&server, "/a%").await, StatusCode::BAD_REQUEST);
    // Escapes that do not encode UTF-8 are rejected as well.
    assert_eq!(status(&server, "/%E4%B8").await, StatusCode::BAD_REQUEST);
    assert_eq!(status(&server, "/%FF").await, StatusCode::BAD_REQUEST);
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn a_route_whose_literal_splits_an_escape_does_not_match() {
    let (root, server) = serve().await;
    server
        .get("/a:b", |req, res, _| async move {
            res.set_json(&req.params)?;
            Ok(None)
        })
        .unwrap();
    server
        .get("{/*rest}", |_, res, _| async move {
            res.set_body("fallback");
            Ok(None)
        })
        .unwrap();
    let client = reqwest::Client::new();
    // A capture that decodes as a whole is matched and decoded.
    let response = client.get(format!("http://{}/a%E4%B8%AD", server.address())).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text().await.unwrap(), r#"{"b":"中"}"#);
    root.fiber().dispose().await.unwrap();
}
