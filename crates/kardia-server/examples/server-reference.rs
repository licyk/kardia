use std::io::{self, BufRead, Write};

use futures::stream;
use kardia_core::{Context, Error};
use kardia_server::{
    Body, Bytes, HeaderName, HeaderValue, Response, Server, ServerConfig, ServerExt, ServerPlugin, StatusCode,
};
use serde_json::json;

#[path = "support/client.rs"]
mod client;
#[path = "support/fs.rs"]
mod fs;

#[tokio::main]
async fn main() -> kardia_core::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("cert") {
        return client::certificate();
    }
    let root = Context::new();
    root.plugin::<ServerPlugin>(ServerConfig::local(0))?.settled().await?;
    let server = root.get::<Server>()?;
    match std::env::args().nth(1).as_deref().unwrap_or("core") {
        "core" => core(&root, &server)?,
        "client" => client::install(&root).await?,
        "fs" => fs::install(&root).await?,
        "static" => {
            use kardia_fs::FsExt;
            root.plugin::<kardia_fs::FsPlugin>(Default::default())?.settled().await?;
            root.plugin::<kardia_fs::LocalFsPlugin>(Default::default())?.settled().await?;
            let config: serde_json::Value = serde_json::from_str(&std::env::var("KARDIA_STATIC_CONFIG")?)?;
            let base = kardia_fs::Url::parse(&format!("{}/", config["root"].as_str().unwrap()))?;
            let fs = root.get::<kardia_fs::Fs>()?;
            server.get("/__file-error", move |req, res, _| {
                let fs = fs.clone();
                let base = base.clone();
                async move {
                    let path = req.query.iter().find(|(key, _)| key == "path").unwrap().1.as_str();
                    let result = fs.file(base.join(path)?.as_str())?.bytes().await;
                    let error =
                        result.err().map(|e| format!("{:?}", e.downcast_ref::<kardia_fs::FsError>().unwrap().code));
                    res.set_json(&error)?;
                    Ok(None)
                }
            })?;
            root.plugin_value::<kardia_server_static::StaticPlugin>(config)?.settled().await?;
            server.get("{/*path}", |_, res, _| async move {
                res.set_body("fallback");
                Ok(None)
            })?;
        }
        "proxy" => {
            let target = Context::new();
            target.plugin::<ServerPlugin>(ServerConfig::local(0))?.settled().await?;
            target.plugin::<kardia_server_echo::EchoPlugin>(json!({}))?.settled().await?;
            let url = std::env::var("KARDIA_PROXY_TARGET").unwrap_or(target.get::<Server>()?.public_url());
            root.defer(
                "proxy fixture",
                kardia_core::Disposer::future(move || async move {
                    let _ = target.fiber().dispose().await;
                }),
            )?;
            root.plugin::<kardia_http::HttpPlugin>(Default::default())?.settled().await?;
            let context = root.clone();
            server.get("/__proxy_url", move |_, res, _| {
                let ctx = context.clone();
                async move {
                    let base = ctx.get::<Server>()?.public_url();
                    let plain = ctx.get::<kardia_server_proxy::ServerProxy>()?.public_url()?.replace(&base, "$BASE");
                    let intercepted = ctx
                        .intercept("server", json!({"path":"/caller"}))
                        .get::<kardia_server_proxy::ServerProxy>()?
                        .public_url()?
                        .replace(&base, "$BASE");
                    res.set_json(&json!({"root":plain,"intercepted":intercepted}))?;
                    Ok(None)
                }
            })?;
            root.intercept("server", json!({"path":std::env::var("KARDIA_PROXY_PATH").unwrap_or_default()}))
                .plugin::<kardia_server_proxy::ProxyPlugin>(kardia_server_proxy::ProxyConfig { target: url })?
                .settled()
                .await?;
        }
        "echo" => {
            root.plugin::<kardia_server_echo::EchoPlugin>(json!({}))?.settled().await?;
        }
        "acl" | "acl-list" => {
            let list = std::env::args().nth(1).as_deref() == Some("acl-list");
            let config = if list { json!({"allowedHosts":[".example.com"]}) } else { json!({"allowedHosts":false}) };
            root.plugin_value::<kardia_server_acl::AclPlugin>(config)?.settled().await?;
            acl(&server)?;
        }
        scenario => return Err(Error::msg(format!("unknown scenario {scenario}"))),
    }
    println!("{}", json!({"url":server.public_url()}));
    io::stdout().flush()?;
    tokio::task::spawn_blocking(|| io::stdin().lock().lines().next()).await?;
    root.fiber().dispose().await
}

fn core(root: &Context, server: &kardia_core::Svc<Server>) -> kardia_core::Result<()> {
    use kardia_server::{WsExt, ws_handler};
    server.ws(
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
    )?;
    for flags in ["g", "y", "gy"] {
        server.get(
            kardia_server::Path::regex(&format!("^/state/{flags}/(\\d+)"), flags)?,
            |req, res, _| async move {
                res.set_json(&req.params)?;
                Ok(None)
            },
        )?;
    }
    for flags in ["i", "iu"] {
        server.get(kardia_server::Path::regex(&format!("^/fold/{flags}/(K)$"), flags)?, |req, res, _| async move {
            res.set_json(&req.params)?;
            Ok(None)
        })?;
    }
    server.get("/returned-headers", |_, _, _| async move {
        let response = Response::new();
        response.set_body("headers");
        for (name, value) in [("set-cookie", "a=1"), ("set-cookie", "b=2"), ("x-multi", "one"), ("x-multi", "two")] {
            response.append_header(name.parse()?, value.parse()?);
        }
        Ok(Some(response))
    })?;
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
    Ok(())
}

fn acl(server: &kardia_core::Svc<Server>) -> kardia_core::Result<()> {
    use kardia_server::{WsExt, handler};
    for (path, options, body) in [
        ("/default", json!({}), "default"),
        ("/public", json!({"allowedHosts":true}), "public"),
        ("/local", json!({"allowedHosts":false}), "local"),
        ("/list", json!({"allowedHosts":["partner.com"]}), "list"),
        ("/fallthrough", json!({"allowedHosts":false}), "local"),
        ("/fallthrough", json!({"allowedHosts":true}), "public"),
    ] {
        server.route(
            "get",
            path,
            handler(move |_, res, _| async move {
                res.set_body(body);
                Ok(None)
            }),
            options.clone(),
        )?;
        server.ws(path, None, options)?;
    }
    Ok(())
}
