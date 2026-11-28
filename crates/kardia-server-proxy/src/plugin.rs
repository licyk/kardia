use std::sync::Arc;

use kardia_core::{Context, Inject, Plugin, Result};
use kardia_http::{Http, HttpExt, RequestConfig, WebSocketExt};
use kardia_server::{Body, HeaderMap, HeaderName, HeaderValue, Request, Server, ServerExt, WsExt, ws_handler};
use serde_json::json;

use crate::config::{ProxyConfig, RESERVED_REQUEST, RESERVED_RESPONSE, RESERVED_WEBSOCKET, Target, connection_fields};

pub struct ProxyPlugin;
impl Plugin for ProxyPlugin {
    const NAME: &'static str = "server:proxy";
    type Config = ProxyConfig;
    fn inject() -> Inject {
        Inject::new().with("http").with("server")
    }
    async fn apply(ctx: Context, config: Arc<ProxyConfig>) -> Result<()> {
        let target = Target::parse(&config.target)?;
        // The router removes the mount prefix before it matches the routes, so
        // the same prefix is removed from the path that is forwarded.
        let server = ctx.get::<Server>()?;
        let http = ctx.get::<Http>()?;
        let prefix = server.meta()["path"].as_str().unwrap_or_default().to_owned();
        let routes = Routes { config: config.clone(), target, prefix };

        let request_routes = routes.clone();
        let client = http.clone();
        server.all("{/*path}", move |req, _, _| {
            let routes = request_routes.clone();
            let client = client.clone();
            async move {
                let Some(url) = routes.destination(&req) else {
                    return Ok(Some(bad_request()));
                };
                let body = if matches!(req.method.as_str(), "POST" | "PUT" | "PATCH") {
                    Some(kardia_http::Body::stream(req.take_body()?.into_stream()))
                } else {
                    None
                };
                let headers = routes.forward_request_headers(&req.headers, RESERVED_REQUEST);
                let response = client
                    .request(
                        url.as_str(),
                        RequestConfig { method: Some(req.method), headers: Some(headers), body, ..Default::default() },
                    )
                    .await?;
                let forwarded = kardia_server::Response::new();
                forwarded.set_status(response.status);
                forwarded.set_status_text(response.status_text)?;
                let blocked = blocked_headers(&response.headers, RESERVED_RESPONSE);
                for (name, value) in &response.headers {
                    if blocked.iter().any(|field| field == name.as_str()) {
                        continue;
                    }
                    forwarded.append_header(name.clone(), value.clone());
                }
                if routes.config.isolate_origin {
                    forwarded.remove_header("set-cookie");
                    forwarded.remove_header("clear-site-data");
                    forwarded.set_header(
                        HeaderName::from_static("content-security-policy"),
                        HeaderValue::from_static("sandbox"),
                    );
                    forwarded.set_header(
                        HeaderName::from_static("x-content-type-options"),
                        HeaderValue::from_static("nosniff"),
                    );
                }
                forwarded.set_body(Body::stream(response.body.into_stream()));
                Ok(Some(forwarded))
            }
        })?;

        let upgrade_routes = routes.clone();
        let logger = ctx.logger();
        server.ws(
            "{/*path}",
            Some(ws_handler(move |req, accept| {
                let routes = upgrade_routes.clone();
                let http = http.clone();
                let logger = logger.clone();
                async move {
                    let Some(url) = routes.destination_ws(&req) else {
                        logger.debug(format_args!("WS {} is outside the target", req.url));
                        return Ok(());
                    };
                    let mut reserved = RESERVED_REQUEST.to_vec();
                    reserved.extend_from_slice(RESERVED_WEBSOCKET);
                    let headers = routes.forward_request_headers(&req.headers, &reserved);
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

#[derive(Clone)]
struct Routes {
    config: Arc<ProxyConfig>,
    target: Target,
    prefix: String,
}

impl Routes {
    /// The URL of a request: the target followed by the path under the routes,
    /// without its leading `/`, and the whole query as the client sent it.
    fn destination(&self, req: &Request) -> Option<kardia_http::Url> {
        let prefix = self.prefix();
        let path = req.path.strip_prefix(prefix.as_str()).unwrap_or(&req.path);
        let path = path.strip_prefix('/').unwrap_or(path);
        let query = req.url.get(req.path.len()..).unwrap_or_default();
        self.target.locate(&self.config.target, path, query)
    }

    fn destination_ws(&self, req: &Request) -> Option<kardia_http::Url> {
        let mut url = self.destination(req)?;
        let scheme = match url.scheme() {
            "http" => "ws",
            "https" => "wss",
            "ws" | "wss" => return Some(url),
            _ => return None,
        };
        url.set_scheme(scheme).ok()?;
        Some(url)
    }

    /// The path prefix the routes are mounted under, which the router removes
    /// before it matches them.
    fn prefix(&self) -> String {
        self.prefix.clone()
    }

    /// The headers to send upstream: the incoming ones minus the reserved and
    /// hop-by-hop fields, minus each mapped source, plus each mapping.
    fn forward_request_headers(&self, incoming: &HeaderMap, reserved: &[&str]) -> HeaderMap {
        let blocked = blocked_headers(incoming, reserved);
        let sources: Vec<(String, String)> = self
            .config
            .request_headers
            .iter()
            .map(|(name, source)| (name.to_ascii_lowercase(), source.to_ascii_lowercase()))
            .collect();
        let mut headers = HeaderMap::new();
        for (name, value) in incoming {
            let field = name.as_str();
            if blocked.iter().any(|entry| entry == field) || sources.iter().any(|(_, source)| source == field) {
                continue;
            }
            headers.append(name.clone(), value.clone());
        }
        for (name, source) in &sources {
            if blocked.iter().any(|entry| entry == name) || blocked.iter().any(|entry| entry == source) {
                continue;
            }
            if let (Ok(name), Some(value)) = (HeaderName::from_bytes(name.as_bytes()), incoming.get(source.as_str())) {
                headers.insert(name, value.clone());
            }
        }
        headers
    }
}

/// The reserved fields plus the ones an incoming `Connection` marks as
/// hop-by-hop for this hop.
pub(crate) fn blocked_headers(incoming: &HeaderMap, reserved: &[&str]) -> Vec<String> {
    let mut blocked: Vec<String> = reserved.iter().map(|field| (*field).to_owned()).collect();
    blocked.extend(connection_fields(incoming.get("connection").and_then(|value| value.to_str().ok())));
    blocked
}

fn bad_request() -> kardia_server::Response {
    let response = kardia_server::Response::new();
    response.set_status(kardia_server::StatusCode::BAD_REQUEST);
    let _ = response.set_status_text("Bad Request".into());
    response
}
