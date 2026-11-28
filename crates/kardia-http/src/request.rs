use crate::{Body, Http, RequestConfig, body::Operation, service::Active};
use futures::{FutureExt, StreamExt, stream};
use kardia_core::{Disposer, Error, Result, Svc};
use parking_lot::Mutex;
use std::{panic::AssertUnwindSafe, sync::Arc, time::Duration};
use tokio::sync::{mpsc, oneshot};

pub struct HttpResponse {
    pub url: url::Url,
    pub status: http::StatusCode,
    pub status_text: String,
    pub headers: http::HeaderMap,
    pub body: Body,
}
pub trait HttpExt {
    fn request(&self, url: &str, config: RequestConfig) -> impl Future<Output = Result<HttpResponse>> + Send;
}
impl HttpExt for Svc<Http> {
    async fn request(&self, url: &str, config: RequestConfig) -> Result<HttpResponse> {
        let request = config.resolve(self, url)?;
        // A `data:` URL carries its content, so no transport is involved.
        if request.url.scheme() == "data" {
            return crate::data_url::response(&request.url, &request.method);
        }
        let client = self.client(request.proxy_agent.as_deref(), &request.url, &request.no_proxy)?;
        let timeout = request.timeout;
        let method = request.method.clone();
        let mut stop = self.stop.subscribe();
        let active = Active::new(&self.active);
        let (head, ready) = oneshot::channel();
        let (send, receive) = mpsc::channel(1);
        let error = Arc::new(Mutex::new(None));
        let failed = error.clone();
        let task = async move {
            let _active = active;
            let mut head = Some(head);
            let operation = async {
                let fetch = crate::transport::fetch(client, request);
                let response = if let Some(ms) = timeout.filter(|ms| *ms > 0) {
                    tokio::time::timeout(Duration::from_millis(ms), fetch)
                        .await
                        .map_err(|_| crate::HttpError::Timeout)??
                } else {
                    fetch.await?
                };
                let meta = (
                    response.url().clone(),
                    response.status(),
                    response
                        .extensions()
                        .get::<hyper::ext::ReasonPhrase>()
                        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
                        .unwrap_or_else(|| response.status().canonical_reason().unwrap_or_default().to_owned()),
                    response.headers().clone(),
                );
                if head.take().unwrap().send(Ok(meta)).is_err() {
                    return Ok::<(), Error>(());
                }
                let mut source = crate::decode::decode(response, &method).await?;
                loop {
                    let Ok(permit) = send.reserve().await else { return Ok(()) };
                    let Some(chunk) = source.next().await else { return Ok(()) };
                    permit.send(chunk?);
                }
            };
            let result = tokio::select! {_ = stop.wait_for(|v|*v).map(|_|())=>Err(Error::msg("HTTP provider disposed")),result=AssertUnwindSafe(operation).catch_unwind()=>result.unwrap_or_else(|p|Err(Error::from_panic(p)))};
            if let Err(error) = result {
                *failed.lock() = Some(error.clone());
                if let Some(head) = head.take() {
                    let _ = head.send(Err(error));
                }
            }
        };
        let effect = self.caller().effect_sync("http request", |scope| {
            let task = tokio::spawn(task);
            scope.defer(Disposer::future(move || async move {
                task.abort();
                let _ = task.await;
            }));
            Ok(())
        })?;
        let operation = Operation(effect);
        let (url, status, status_text, headers) = ready.await.map_err(|_| Error::msg("HTTP request cancelled"))??;
        let reader = Reader { receive, error, _operation: operation };
        let body = Body::stream(stream::unfold(reader, |mut reader| async move {
            match reader.receive.recv().await {
                Some(bytes) => Some((Ok(bytes), reader)),
                None => {
                    let error = reader.error.lock().take();
                    error.map(|e| (Err(e), reader))
                }
            }
        }));
        Ok(HttpResponse { url, status, status_text, headers, body })
    }
}
struct Reader {
    receive: mpsc::Receiver<bytes::Bytes>,
    error: Arc<Mutex<Option<Error>>>,
    _operation: Operation,
}
