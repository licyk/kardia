use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::{FutureExt, future::BoxFuture};
use http::StatusCode;
use hyper::upgrade::OnUpgrade;
use hyper_util::rt::TokioIo;
use kardia_core::{Context, Disposal, Disposer, EffectHandle, Error, Result, Svc};
use parking_lot::Mutex;
use serde_json::Value;
use tokio::{
    sync::{oneshot, watch},
    task::JoinSet,
};
use tokio_tungstenite::{WebSocketStream, tungstenite::protocol::Role};

use crate::pattern::Pattern;
use crate::{Next, Path, Request, Response, RouteInfo, Server, WebSocket};

pub type WsHandler = Arc<dyn Fn(Request, Accept) -> BoxFuture<'static, Result<()>> + Send + Sync>;
pub type UpgradeHook = Arc<dyn Fn(Request, Next) -> Next + Send + Sync>;

pub fn ws_handler<F, Fut>(callback: F) -> WsHandler
where
    F: Fn(Request, Accept) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    Arc::new(move |req, accept| callback(req, accept).boxed())
}

pub struct WsRoute {
    pub info: Arc<RouteInfo>,
    effect: EffectHandle,
    state: Arc<WsState>,
}
impl WsRoute {
    pub fn client_count(&self) -> usize {
        self.state.clients.load(Ordering::SeqCst)
    }
    pub fn dispose(&self) -> Disposal {
        self.effect.dispose()
    }
}

pub(crate) struct WsEntry {
    pub id: u64,
    info: Arc<RouteInfo>,
    handler: Option<WsHandler>,
    state: Arc<WsState>,
}
struct WsState {
    ctx: Context,
    stop: watch::Sender<bool>,
    tasks: Mutex<JoinSet<()>>,
    clients: Arc<AtomicUsize>,
}
impl WsState {
    fn spawn(&self, future: impl Future<Output = Result<()>> + Send + 'static) {
        let ctx = self.ctx.clone();
        let mut tasks = self.tasks.lock();
        while tasks.try_join_next().is_some() {}
        tasks.spawn(async move {
            if let Err(error) = crate::dispatch::contain(future).await {
                ctx.logger().log_error(&error);
            }
        });
    }
    async fn dispose(&self) {
        self.stop.send_replace(true);
        let mut tasks = std::mem::take(&mut *self.tasks.lock());
        if tokio::time::timeout(Duration::from_secs(5), async { while tasks.join_next().await.is_some() {} })
            .await
            .is_err()
        {
            tasks.shutdown().await;
        }
    }
}

/// Consuming acceptance token; the HTTP handshake is sent only when accepted.
pub struct Accept {
    upgrade: OnUpgrade,
    response: Response,
    ready: oneshot::Sender<Response>,
    state: Arc<WsState>,
    closed: watch::Receiver<bool>,
}
impl Accept {
    pub async fn accept(self) -> Result<WebSocket> {
        let Self { upgrade, response, ready, state, closed } = self;
        let accepted = response.status() == StatusCode::SWITCHING_PROTOCOLS;
        ready.send(response).map_err(|_| Error::msg("upgrade request was cancelled"))?;
        if !accepted {
            return Err(Error::msg("invalid WebSocket handshake"));
        }
        let upgraded = upgrade.await?;
        let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(100 * 1024 * 1024))
            .max_frame_size(Some(100 * 1024 * 1024));
        let stream = WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, Some(config)).await;
        let (socket, task) = crate::socket::channels(stream, state.stop.subscribe(), closed, state.clients.clone());
        state.spawn(task);
        Ok(socket)
    }
}

pub trait WsExt {
    fn ws(&self, path: impl Into<Path>, callback: Option<WsHandler>, options: Value) -> Result<WsRoute>;
    fn on_upgrade(&self, hook: UpgradeHook) -> Result<EffectHandle>;
}
impl WsExt for Svc<Server> {
    fn ws(&self, path: impl Into<Path>, callback: Option<WsHandler>, options: Value) -> Result<WsRoute> {
        let path = path.into();
        let meta = self.meta();
        let info = Arc::new(RouteInfo {
            method: "ws".into(),
            pattern: Pattern::new(&path)?,
            path,
            options,
            prefix: meta["path"].as_str().unwrap_or_default().into(),
            fiber: self.caller().fiber(),
        });
        let (stop, _) = watch::channel(false);
        let state = Arc::new(WsState {
            ctx: self.caller().clone(),
            stop,
            tasks: Mutex::new(JoinSet::new()),
            clients: Arc::new(AtomicUsize::new(0)),
        });
        let entry =
            Arc::new(WsEntry { id: self.next_id(), info: info.clone(), handler: callback, state: state.clone() });
        let server = self.arc().clone();
        let owned = state.clone();
        let effect = self.caller().effect_sync("server WebSocket route", |scope| {
            server.ws_routes.lock().push(entry.clone());
            scope.defer(Disposer::future(move || {
                server.ws_routes.lock().retain(|r| r.id != entry.id);
                async move {
                    owned.dispose().await;
                }
            }));
            Ok(())
        })?;
        Ok(WsRoute { info, effect, state })
    }
    fn on_upgrade(&self, hook: UpgradeHook) -> Result<EffectHandle> {
        let server = self.arc().clone();
        let id = self.next_id();
        let calls = crate::ownership::Calls::new();
        let owner = calls.clone();
        let hook: UpgradeHook = Arc::new(move |req, next| {
            let calls = owner.clone();
            let hook = hook.clone();
            async move {
                let owner = calls.clone();
                calls
                    .run(async move {
                        let result = hook(req, next).await?;
                        if let Some(response) = &result {
                            owner.response(response)?;
                        }
                        Ok(result)
                    })
                    .await
            }
            .boxed()
        });
        self.caller().effect_sync("server upgrade hook", |scope| {
            server.upgrades.lock().push((id, hook));
            scope.defer(Disposer::future(move || {
                server.upgrades.lock().retain(|(key, _)| *key != id);
                calls.close()
            }));
            Ok(())
        })
    }
}

impl Server {
    pub(crate) async fn close_websockets(&self) {
        let routes = self.ws_routes.lock().clone();
        for route in routes {
            route.state.dispose().await;
        }
    }
    pub(crate) async fn upgrade(
        self: Arc<Self>,
        req: Request,
        upgrade: OnUpgrade,
        response: Response,
    ) -> Option<Response> {
        let hooks = self.upgrades.lock().iter().map(|(_, hook)| hook.clone()).collect::<Vec<_>>();
        let server = self.clone();
        let request = req.clone();
        let mut next: Next = async move { server.upgrade_route(request, upgrade, response).await }.boxed();
        for hook in hooks.into_iter().rev() {
            let request = req.clone();
            next = async move { hook(request, next).await }.boxed();
        }
        match crate::dispatch::contain(next).await {
            Ok(response) => response,
            Err(error) => {
                self.ctx.logger().log_error(&error);
                let res = Response::new();
                res.set_status(StatusCode::INTERNAL_SERVER_ERROR);
                Some(res)
            }
        }
    }
    async fn upgrade_route(
        self: Arc<Self>,
        req: Request,
        upgrade: OnUpgrade,
        response: Response,
    ) -> Result<Option<Response>> {
        let routes = self.ws_routes.lock().clone();
        let checks = self.checks.lock().iter().map(|(_, check)| check.clone()).collect::<Vec<_>>();
        for route in routes {
            let Some(request) = route.info.matches(&req, &checks)? else { continue };
            let (ready, mut response_ready) = oneshot::channel();
            let (done, mut completed) = oneshot::channel();
            let (stop, closed) = watch::channel(false);
            let accept = Accept { upgrade, response, ready, state: route.state.clone(), closed };
            let handler = route.handler.clone();
            let mut cancelled = route.state.stop.subscribe();
            route.state.spawn(async move {
                let result=tokio::select! {
                    _=cancelled.changed()=>Ok(()),
                    result=crate::dispatch::contain(async move { match handler { Some(handler)=>handler(request,accept).await, None=>accept.accept().await.map(|_| ()) } })=>result,
                };
                if result.is_err() { stop.send_replace(true); }
                let _=done.send(result.clone()); result
            });
            return tokio::select! {
                biased;
                response=&mut response_ready=>match response { Ok(response)=>Ok(Some(response)), Err(_)=>{ completed.await.map_err(|_| Error::msg("upgrade task stopped"))??; Ok(None) } },
                result=&mut completed=>{ result.map_err(|_| Error::msg("upgrade task stopped"))??; Ok(response_ready.try_recv().ok()) }
            };
        }
        let response = Response::new();
        response.set_status(StatusCode::NOT_FOUND);
        Ok(Some(response))
    }
}
