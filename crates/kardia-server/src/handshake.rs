use crate::{Response, StatusCode};
use http::{HeaderName, HeaderValue, Method, Request};
use std::collections::HashSet;

pub(crate) fn response(mut request: Request<()>) -> Response {
    if request.method() != Method::GET {
        return reject(StatusCode::METHOD_NOT_ALLOWED, "Invalid HTTP method");
    }
    if !request.headers().get("upgrade").is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket")) {
        return reject(StatusCode::BAD_REQUEST, "Invalid Upgrade header");
    }
    let key = request.headers().get("sec-websocket-key").map(|v| v.as_bytes()).unwrap_or_default();
    if key.len() != 24
        || &key[22..] != b"=="
        || !key[..22].iter().all(|b| b.is_ascii_alphanumeric() || *b == b'+' || *b == b'/')
    {
        return reject(StatusCode::BAD_REQUEST, "Missing or invalid Sec-WebSocket-Key header");
    }
    let version = request
        .headers()
        .get("sec-websocket-version")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<f64>().ok());
    if version != Some(13.0) && version != Some(8.0) {
        let response = reject(StatusCode::BAD_REQUEST, "Missing or invalid Sec-WebSocket-Version header");
        response.set_header(HeaderName::from_static("sec-websocket-version"), HeaderValue::from_static("13, 8"));
        return response;
    }
    let mut first = None;
    if let Some(protocols) = request.headers().get("sec-websocket-protocol") {
        let mut seen = HashSet::new();
        for protocol in protocols.as_bytes().split(|b| *b == b',') {
            let protocol = protocol.trim_ascii();
            if HeaderName::from_bytes(protocol).is_err() || !seen.insert(protocol) {
                return reject(StatusCode::BAD_REQUEST, "Invalid Sec-WebSocket-Protocol header");
            }
            if first.is_none() {
                first = HeaderValue::from_bytes(protocol).ok();
            }
        }
    }
    request.headers_mut().insert("sec-websocket-version", HeaderValue::from_static("13"));
    let handshake = match tokio_tungstenite::tungstenite::handshake::server::create_response(&request) {
        Ok(response) => response,
        Err(_) => return reject(StatusCode::BAD_REQUEST, "Bad Request"),
    };
    let response = Response::new();
    response.set_status(handshake.status());
    for (name, value) in handshake.headers() {
        response.set_header(name.clone(), value.clone());
    }
    if let Some(protocol) = first {
        response.set_header(HeaderName::from_static("sec-websocket-protocol"), protocol);
    }
    response
}
fn reject(status: StatusCode, message: &'static str) -> Response {
    let response = Response::new();
    response.set_status(status);
    response.set_header(HeaderName::from_static("connection"), HeaderValue::from_static("close"));
    response.set_header(HeaderName::from_static("content-type"), HeaderValue::from_static("text/html"));
    response.set_header(HeaderName::from_static("content-length"), HeaderValue::from(message.len()));
    response.set_body(message);
    response
}
