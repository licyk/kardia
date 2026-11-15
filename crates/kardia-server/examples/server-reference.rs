use std::io::{self, BufRead, Write};

use futures::stream;
use kardia_core::{Context, Error};
use kardia_server::{
    Body, Bytes, HeaderName, HeaderValue, Response, Server, ServerConfig, ServerExt, ServerPlugin, StatusCode,
};
use serde_json::json;

#[tokio::main]
async fn main() -> kardia_core::Result<()> {
    let root = Context::new();
    root.plugin::<ServerPlugin>(ServerConfig::local(0))?.settled().await?;
    let server = root.get::<Server>()?;
    server.get("/hello", |_, res, _| async move {
        res.set_text("world");
        Ok(None)
    })?;
    server.get("/params/:name", |req, res, _| async move {
        res.set_text(req.params["name"].as_str().unwrap());
        Ok(None)
    })?;
    server.get("/query", |req, res, _| async move {
        res.set_json(&req.query)?;
        Ok(None)
    })?;
    server.post("/body", |req, res, _| async move {
        res.set_body(req.take_body()?);
        Ok(None)
    })?;
    server.get("/status", |_, res, _| async move {
        res.set_status(StatusCode::NO_CONTENT);
        Ok(None)
    })?;
    server.get("/error", |_, _, _| async move { Err(Error::msg("reference error")) })?;
    server.get("/claimed-error", |_, res, _| async move {
        res.set_status(StatusCode::SERVICE_UNAVAILABLE);
        Err(Error::msg("reference error"))
    })?;
    server.get("/stream", |_, res, _| async move {
        res.set_body(Body::stream(stream::iter(["hello", " ", "world"].map(|s| Ok(Bytes::from_static(s.as_bytes()))))));
        let (a, b) = tokio::try_join!(res.text(), res.text())?;
        res.set_header(HeaderName::from_static("x-inspected"), HeaderValue::from_str(&format!("{a}|{b}"))?);
        Ok(None)
    })?;
    server.get("/next", |_, _, next| next)?;
    server.get("/next", |_, _, _| async {
        let res = Response::new();
        res.set_json(&json!({"ok":true}))?;
        Ok(Some(res))
    })?;
    server.all("/allow", |_, _, next| next)?;
    root.intercept("server", json!({"path":"/api"})).get::<Server>()?.get("/scoped", |_, res, _| async move {
        res.set_text("scoped");
        Ok(None)
    })?;
    println!("{}", json!({"url":server.public_url()}));
    io::stdout().flush()?;
    tokio::task::spawn_blocking(|| io::stdin().lock().lines().next()).await?;
    root.fiber().dispose().await
}
