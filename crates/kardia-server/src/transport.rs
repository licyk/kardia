use std::sync::Arc;

use futures::StreamExt;
use http_body_util::{BodyExt, StreamBody, combinators::UnsyncBoxBody};
use hyper::{
    body::{Frame, Incoming},
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use tokio::{net::TcpListener, sync::oneshot, task::JoinSet};

use crate::{Body, Bytes, Request, Server};

type WireBody = UnsyncBoxBody<Bytes, std::io::Error>;

pub(crate) async fn listen(server: Arc<Server>, listener: TcpListener, mut stop: oneshot::Receiver<()>) {
    let mut connections = JoinSet::new();
    let result = crate::dispatch::contain(async {
        loop {
            tokio::select! {
                _ = &mut stop => break Ok(()),
                accepted = listener.accept() => {
                    let (socket, _) = accepted?;
                    socket.set_nodelay(true)?;
                    let server = server.clone();
                    connections.spawn(async move {
                        let service = service_fn(move |req| serve(server.clone(), req));
                        let _ = http1::Builder::new().serve_connection(TokioIo::new(socket), service).with_upgrades().await;
                    });
                }
                result = connections.join_next(), if !connections.is_empty() => {
                    if let Some(Err(error)) = result { server.ctx.logger().log_error(&error.into()); }
                }
            }
        }
    })
    .await;
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    if let Err(error) = result {
        drop(server.ctx.fiber().fail(error));
    }
}

async fn serve(
    server: Arc<Server>,
    mut request: hyper::Request<Incoming>,
) -> std::io::Result<hyper::Response<WireBody>> {
    let websocket = request.headers().get("upgrade").is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"));
    let upgrade = websocket.then(|| hyper::upgrade::on(&mut request));
    let (parts, body) = request.into_parts();
    let req = Request::new(
        parts.method.clone(),
        parts.uri.to_string(),
        parts.headers.clone(),
        Body::stream(body.into_data_stream().map(|chunk| chunk.map_err(Into::into))),
    );
    let response = if let Some(upgrade) = upgrade {
        let mut handshake = http::Request::new(());
        *handshake.method_mut() = parts.method.clone();
        *handshake.uri_mut() = parts.uri;
        *handshake.headers_mut() = parts.headers;
        if handshake.headers().get("sec-websocket-version").is_some_and(|v| v == "8") {
            handshake.headers_mut().insert("sec-websocket-version", http::HeaderValue::from_static("13"));
        }
        let res = match tokio_tungstenite::tungstenite::handshake::server::create_response(&handshake) {
            Ok(handshake) => {
                let res = crate::Response::new();
                res.set_status(handshake.status());
                for (name, value) in handshake.headers() {
                    res.append_header(name.clone(), value.clone());
                }
                if let Some(protocol) = req
                    .headers
                    .get("sec-websocket-protocol")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.split(',').next())
                {
                    if let Ok(protocol) = http::HeaderValue::from_str(protocol.trim()) {
                        res.set_header(http::HeaderName::from_static("sec-websocket-protocol"), protocol);
                    }
                }
                res
            }
            Err(_) => {
                let res = crate::Response::new();
                res.set_status(if parts.method == http::Method::GET {
                    http::StatusCode::BAD_REQUEST
                } else {
                    http::StatusCode::METHOD_NOT_ALLOWED
                });
                res
            }
        };
        server
            .upgrade(req, upgrade, res)
            .await
            .ok_or_else(|| std::io::Error::other("WebSocket handler did not accept"))?
    } else {
        server.dispatch(req).await
    };
    let data = response.take();
    let body = if parts.method == http::Method::HEAD
        || data.status == http::StatusCode::NO_CONTENT
        || data.status == http::StatusCode::NOT_MODIFIED
    {
        Body::default()
    } else {
        data.body
    };
    let stream = body
        .into_stream()
        .map(|chunk| chunk.map(Frame::data).map_err(|error| std::io::Error::other(error.to_string())));
    let mut response = hyper::Response::new(StreamBody::new(stream).boxed_unsync());
    *response.status_mut() = data.status;
    *response.headers_mut() = data.headers;
    let reason = data.status_text.unwrap_or_else(|| {
        let text = match data.status.as_u16() {
            418 => "I'm a Teapot",
            509 => "Bandwidth Limit Exceeded",
            _ => data.status.canonical_reason().unwrap_or("unknown"),
        };
        hyper::ext::ReasonPhrase::from_static(text.as_bytes())
    });
    response.extensions_mut().insert(reason);
    Ok(response)
}
