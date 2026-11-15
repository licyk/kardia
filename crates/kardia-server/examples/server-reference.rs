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
    server.get("/accept", |req, res, _| async move {
        let candidates: Vec<_> = req.query.iter().filter(|(key, _)| key == "type").map(|(_, v)| v.as_str()).collect();
        let chosen = req.accepts(&candidates).map_or(json!(false), |s| json!(s));
        res.set_json(&json!({"all":req.accepted_types(),"chosen":chosen}))?;
        Ok(None)
    })?;
    server.post("/form", |req, res, _| async move {
        let entries = req
            .form_data()
            .await?
            .0
            .into_iter()
            .map(|entry| {
                let value = match entry.value {
                    kardia_server::FormValue::Text(text) => json!(text),
                    kardia_server::FormValue::File { filename, blob } => {
                        json!({"filename":filename,"type":blob.content_type,"bytes":blob.bytes.to_vec()})
                    }
                };
                json!([entry.name, value])
            })
            .collect::<Vec<_>>();
        res.set_json(&entries)?;
        Ok(None)
    })?;
    server.post("/consumption", |req, res, _| async move {
        let before = req.body_used();
        let value = req.text().await?;
        let after = req.body_used();
        let rejected = req.text().await.is_err();
        res.set_json(&json!({"before":before,"after":after,"value":value,"rejected":rejected}))?;
        Ok(None)
    })?;
    server.post("/json", |req, res, _| async move {
        res.set_json(&req.json().await?)?;
        Ok(None)
    })?;
    server.get("/reason", |_, res, _| async move {
        res.set_status_text("Greeting".into())?;
        res.set_text("hello");
        Ok(None)
    })?;
    server.get("/raw", |_, res, _| async move {
        res.set_body("raw");
        Ok(None)
    })?;
    server.get("/blob", |_, res, _| async move {
        res.set_text("hello");
        res.set_header(http::header::CONTENT_TYPE, HeaderValue::from_static("text/custom"));
        let blob = res.blob().await?;
        res.set_json(&json!({"type":blob.content_type,"text":String::from_utf8_lossy(&blob.bytes)}))?;
        Ok(None)
    })?;
    for path in [
        "/optional{/:id}",
        "/wild/*path",
        "/mix/:a-:b",
        "/wildext/*name.:ext",
        "/two/*a/*b",
        "/quoted/:\"x-y\"",
        "/unicode/:名字",
    ] {
        server.get(path, |req, res, _| async move {
            res.set_json(&req.params)?;
            Ok(None)
        })?;
    }
    server.get(kardia_server::Path::regex(r"^/regex/(?<id>\d+)(?=\.json)", "")?, |req, res, _| async move {
        res.set_json(&req.params)?;
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
