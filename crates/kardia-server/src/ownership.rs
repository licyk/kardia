use crate::{Body, Handler, Response};
use futures::{FutureExt, StreamExt};
use kardia_core::{Error, Result};
use parking_lot::Mutex;
use std::{future::Future, sync::Arc};
use tokio::{
    sync::{mpsc, oneshot},
    task::{AbortHandle, JoinSet},
};

#[derive(Default)]
pub(crate) struct Calls(Mutex<Option<JoinSet<()>>>);
impl Calls {
    pub fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(Some(JoinSet::new()))))
    }
    pub fn close(&self) -> impl Future<Output = ()> + Send + 'static + use<> {
        let mut tasks = self.0.lock().take().unwrap_or_default();
        tasks.abort_all();
        async move { while tasks.join_next().await.is_some() {} }
    }
    fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) -> Result<Running> {
        let mut state = self.0.lock();
        let tasks = state.as_mut().ok_or_else(|| Error::msg("server callback owner disposed"))?;
        while tasks.try_join_next().is_some() {}
        Ok(Running(tasks.spawn(task)))
    }
    pub async fn run<T: Send + 'static>(&self, work: impl Future<Output = Result<T>> + Send + 'static) -> Result<T> {
        let (send, receive) = oneshot::channel();
        let _running = self.spawn(async move {
            let _ = send.send(crate::dispatch::contain(work).await);
        })?;
        receive.await.map_err(|_| Error::msg("server callback owner disposed"))?
    }
    pub fn handler(self: &Arc<Self>, callback: Handler) -> Handler {
        let calls = self.clone();
        Arc::new(move |req, res, next| {
            let calls = calls.clone();
            let callback = callback.clone();
            async move {
                let owner = calls.clone();
                calls
                    .run(async move {
                        let result = crate::dispatch::contain(async { callback(req, res.clone(), next).await }).await;
                        owner.response(result.as_ref().ok().and_then(Option::as_ref).unwrap_or(&res))?;
                        result
                    })
                    .await
            }
            .boxed()
        })
    }
    pub fn response(&self, response: &Response) -> Result<()> {
        if !response.claimed() {
            return Ok(());
        }
        let mut source = response.take_body().into_stream();
        let (send, receive) = mpsc::channel(1);
        let ended = Arc::new(Mutex::new(Some(Error::msg("server response owner disposed"))));
        let result = ended.clone();
        let running = self.spawn(async move {
            let outcome = crate::dispatch::contain(async {
                loop {
                    let Ok(permit) = send.reserve().await else { return Ok(()) };
                    let Some(chunk) = source.next().await else { return Ok(()) };
                    permit.send(chunk?);
                }
            })
            .await;
            *result.lock() = outcome.err();
        })?;
        let reader = Reader { receive, ended, _running: running };
        response.set_body(Body::stream(futures::stream::unfold(reader, |mut reader| async move {
            match reader.receive.recv().await {
                Some(bytes) => Some((Ok(bytes), reader)),
                None => {
                    let error = reader.ended.lock().take();
                    error.map(|error| (Err(error), reader))
                }
            }
        })));
        Ok(())
    }
}
struct Running(AbortHandle);
impl Drop for Running {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct Reader {
    receive: mpsc::Receiver<crate::Bytes>,
    ended: Arc<Mutex<Option<Error>>>,
    _running: Running,
}
