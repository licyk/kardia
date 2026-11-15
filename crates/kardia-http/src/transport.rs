use crate::{Body, Redirect, config::Resolved};
use http::{HeaderValue, Method};
use kardia_core::{Error, Result};

pub(crate) async fn fetch(client: reqwest::Client, mut request: Resolved) -> Result<reqwest::Response> {
    let upper = request.method.as_str().to_ascii_uppercase();
    if matches!(upper.as_str(), "CONNECT" | "TRACE" | "TRACK") {
        return Err(Error::msg("method is forbidden by fetch"));
    }
    if matches!(request.method, Method::GET | Method::HEAD) && request.body.is_some() {
        return Err(Error::msg("GET/HEAD requests cannot have a body"));
    }
    if request.keep_alive && matches!(request.body, Some(Body::Stream(_))) {
        return Err(Error::msg("keepAlive cannot be used with a streaming request body"));
    }
    if request.headers.contains_key("expect") {
        return Err(Error::msg("Expect is unsupported by fetch"));
    }
    if request.body.is_none() {
        request.headers.remove("content-length");
    }
    for redirects in 0..=20 {
        if !matches!(request.url.scheme(), "http" | "https")
            || !request.url.username().is_empty()
            || request.url.password().is_some()
        {
            return Err(Error::msg("invalid HTTP request URL"));
        }
        let replay = request.body.as_ref().and_then(Body::replay);
        let streaming = matches!(request.body, Some(Body::Stream(_)));
        let mut headers = request.headers.clone();
        for (name, value) in [
            ("accept", "*/*"),
            ("accept-language", "*"),
            ("sec-fetch-mode", "cors"),
            ("user-agent", "node"),
            ("accept-encoding", if request.url.scheme() == "https" { "br, gzip, deflate" } else { "gzip, deflate" }),
        ] {
            headers.entry(name).or_insert(HeaderValue::from_static(value));
        }
        let mut builder = client.request(request.method.clone(), request.url.clone()).headers(headers);
        if let Some(body) = request.body.take() {
            builder = builder.body(match body {
                Body::Bytes(bytes) => reqwest::Body::from(bytes),
                Body::Stream(source) => {
                    reqwest::Body::wrap_stream(source.map_err(|e| std::io::Error::other(e.to_string())))
                }
            });
        }
        let response = builder.send().await?;
        let status = response.status().as_u16();
        if !matches!(status, 301 | 302 | 303 | 307 | 308) {
            return Ok(response);
        }
        match request.redirect {
            Redirect::Manual => return Ok(response),
            Redirect::Error => return Err(Error::msg("redirect is disallowed")),
            Redirect::Follow => {}
        }
        let Some(location) = response.headers().get("location") else { return Ok(response) };
        if redirects == 20 {
            return Err(Error::msg("redirect count exceeded"));
        }
        let next = request.url.join(location.to_str()?)?;
        if streaming && status != 303 {
            return Err(Error::msg("cannot replay a streaming request body after redirect"));
        }
        request.body = replay;
        if ((status == 301 || status == 302) && request.method == Method::POST)
            || (status == 303 && !matches!(request.method, Method::GET | Method::HEAD))
        {
            request.method = Method::GET;
            request.body = None;
            for name in ["content-encoding", "content-language", "content-location", "content-type", "content-length"] {
                request.headers.remove(name);
            }
        }
        if request.url.origin() != next.origin() {
            for name in ["authorization", "proxy-authorization", "cookie", "host"] {
                request.headers.remove(name);
            }
        }
        request.url = next;
    }
    unreachable!()
}
use futures::TryStreamExt;
