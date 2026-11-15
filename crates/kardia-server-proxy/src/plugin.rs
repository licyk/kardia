use kardia_core::{Context, Inject, Plugin, Result, Service};
use kardia_http::{Http, HttpExt, RequestConfig, Url, WebSocketExt};
use kardia_server::{Body, Request, Server, ServerExt, WsExt, ws_handler};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

// encodeURIComponent's unescaped characters plus RFC 3986 path sub-delimiters.
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')')
    .remove(b'$')
    .remove(b'&')
    .remove(b'+')
    .remove(b',')
    .remove(b':')
    .remove(b';')
    .remove(b'=')
    .remove(b'@');

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProxyConfig {
    pub target: String,
}
pub struct ServerProxy {
    ctx: Context,
}
impl Service for ServerProxy {
    const NAME: &'static str = "server.proxy";
}
impl ServerProxy {
    pub fn public_url(&self) -> Result<String> {
        Ok(self.ctx.get::<Server>()?.public_url())
    }
}
pub struct ProxyPlugin;
impl Plugin for ProxyPlugin {
    const NAME: &'static str = "server:proxy";
    type Config = ProxyConfig;
    fn inject() -> Inject {
        Inject::new().with("http").with("server")
    }
    fn provide() -> Vec<kardia_core::Key> {
        vec![ServerProxy::NAME.into()]
    }
    async fn apply(ctx: Context, config: Arc<ProxyConfig>) -> Result<()> {
        let mut target = Url::parse(&config.target)
            .map_err(|error| kardia_core::ValidationError::message(format!("target: {error}")))?;
        target.set_path(&format!("{}/", target.path().trim_end_matches('/')));
        let server = ctx.get::<Server>()?;
        let http = ctx.get::<Http>()?;
        ctx.provide(ServerProxy { ctx: ctx.clone() })?;
        let http_target = target.clone();
        let client = http.clone();
        server.all("{/*path}", move |req, _, _| {
            let target = http_target.clone();
            let client = client.clone();
            async move {
                let Some(url) = destination(&target, &req)? else {
                    let response = kardia_server::Response::new();
                    response.set_status(kardia_server::StatusCode::BAD_REQUEST);
                    response.set_status_text("Bad Request".into())?;
                    return Ok(Some(response));
                };
                let body = if matches!(req.method.as_str(), "POST" | "PUT" | "PATCH") {
                    Some(kardia_http::Body::stream(req.take_body()?.into_stream()))
                } else {
                    None
                };
                let response = client
                    .request(
                        url.as_str(),
                        RequestConfig {
                            method: Some(req.method),
                            headers: Some(req.headers),
                            body,
                            ..Default::default()
                        },
                    )
                    .await?;
                let forwarded = kardia_server::Response::new();
                forwarded.set_status(response.status);
                forwarded.set_status_text(response.status_text)?;
                for (name, value) in &response.headers {
                    if name != "content-length" && name != "content-encoding" {
                        forwarded.append_header(name.clone(), value.clone());
                    }
                }
                forwarded.set_body(Body::stream(response.body.into_stream()));
                Ok(Some(forwarded))
            }
        })?;
        server.ws(
            "{/*path}",
            Some(ws_handler(move |req, accept| {
                let target = target.clone();
                let http = http.clone();
                async move {
                    let Some(mut url) = destination(&target, &req)? else { return Ok(()) };
                    let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
                    url.set_scheme(scheme).map_err(|_| kardia_core::Error::msg("invalid proxy WebSocket target"))?;
                    let mut headers = req.headers;
                    for name in [
                        "host",
                        "connection",
                        "upgrade",
                        "sec-websocket-key",
                        "sec-websocket-version",
                        "sec-websocket-extensions",
                        "sec-websocket-accept",
                        "content-length",
                    ] {
                        headers.remove(name);
                    }
                    let upstream = match http
                        .ws(url.as_str(), RequestConfig { headers: Some(headers), ..Default::default() })
                        .await
                    {
                        Ok(socket) => socket,
                        Err(error) => {
                            http.logger().log_error(&error);
                            return Ok(());
                        }
                    };
                    let downstream = accept.accept().await?;
                    crate::socket::forward(upstream, downstream).await
                }
            })),
            json!({}),
        )?;
        Ok(())
    }
}
fn destination(target: &Url, req: &Request) -> Result<Option<Url>> {
    let path = req.params["path"]
        .as_str()
        .unwrap_or_default()
        .split('/')
        .map(|segment| utf8_percent_encode(segment, SEGMENT).to_string())
        .collect::<Vec<_>>()
        .join("/");
    let mut url = target.join(&format!("./{path}"))?;
    if url.origin() != target.origin() || !url.path().starts_with(target.path()) {
        return Ok(None);
    }
    url.set_query(None);
    if !req.query.is_empty() {
        url.query_pairs_mut().extend_pairs(&req.query);
    }
    Ok(Some(url))
}
