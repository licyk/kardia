use std::convert::Infallible;
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
                    let server = server.clone();
                    connections.spawn(async move {
                        let service = service_fn(move |req| serve(server.clone(), req));
                        let _ = http1::Builder::new().serve_connection(TokioIo::new(socket), service).await;
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
    request: hyper::Request<Incoming>,
) -> std::result::Result<hyper::Response<WireBody>, Infallible> {
    let (parts, body) = request.into_parts();
    let req = Request::new(
        parts.method.clone(),
        parts.uri.to_string(),
        parts.headers,
        Body::stream(body.into_data_stream().map(|chunk| chunk.map_err(Into::into))),
    );
    let response = server.dispatch(req).await;
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
    if let Some(text) = data.status_text {
        response.extensions_mut().insert(text);
    }
    Ok(response)
}
