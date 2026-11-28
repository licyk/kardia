use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::FutureExt;
use kardia_core::{Context, Disposer, EffectHandle, Plugin, Result, Service, Svc};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use crate::pattern::Pattern;
use crate::route::{HttpRoute, RouteCheck, RouteHook};
use crate::{Handler, Next, Path, Request, Response, Route, RouteInfo, ServerConfig};

pub struct Server {
    pub(crate) ctx: Context,
    pub(crate) config: ServerConfig,
    pub(crate) address: SocketAddr,
    pub(crate) routes: Mutex<Vec<Arc<HttpRoute>>>,
    pub(crate) middleware: Mutex<Vec<(u64, Handler)>>,
    pub(crate) hooks: Mutex<Vec<(u64, RouteHook)>>,
    pub(crate) checks: Mutex<Vec<(u64, RouteCheck)>>,
    pub(crate) ws_routes: Mutex<Vec<Arc<crate::websocket::WsEntry>>>,
    pub(crate) upgrades: Mutex<Vec<(u64, crate::websocket::UpgradeHook)>>,
    sequence: AtomicU64,
}
impl Service for Server {
    const NAME: &'static str = "server";
}

impl Server {
    pub fn address(&self) -> SocketAddr {
        self.address
    }
    pub fn route_count(&self) -> usize {
        self.routes.lock().len()
    }
    pub(crate) fn next_id(&self) -> u64 {
        self.sequence.fetch_add(1, Ordering::Relaxed)
    }
}

pub struct ServerPlugin;
impl Plugin for ServerPlugin {
    const NAME: &'static str = "server";
    type Config = ServerConfig;
    fn provide() -> Vec<kardia_core::Key> {
        vec![Server::NAME.into()]
    }
    async fn apply(ctx: Context, config: Arc<ServerConfig>) -> Result<()> {
        let listener = bind(&config).await?;
        let server = Arc::new(Server {
            ctx: ctx.clone(),
            config: (*config).clone(),
            address: listener.local_addr()?,
            routes: Mutex::new(Vec::new()),
            middleware: Mutex::new(Vec::new()),
            hooks: Mutex::new(Vec::new()),
            checks: Mutex::new(Vec::new()),
            ws_routes: Mutex::new(Vec::new()),
            upgrades: Mutex::new(Vec::new()),
            sequence: AtomicU64::new(0),
        });
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(crate::transport::listen(server.clone(), listener, stopped));
        let shutdown = server.clone();
        ctx.defer(
            "server listener",
            Disposer::future(move || async move {
                let _ = stop.send(());
                let _ = task.await;
                shutdown.close_websockets().await;
            }),
        )?;
        ctx.provide_arc(server)?;
        Ok(())
    }
}

async fn bind(config: &ServerConfig) -> std::io::Result<TcpListener> {
    let last = config.max_port.unwrap_or(config.port);
    let mut port = config.port;
    loop {
        match TcpListener::bind((config.host.as_str(), port)).await {
            Ok(listener) => return Ok(listener),
            Err(error)
                if port < last
                    && matches!(error.kind(), std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied) =>
            {
                port += 1
            }
            Err(error) => return Err(error),
        }
    }
}

pub trait ServerExt {
    fn public_url(&self) -> String;
    fn route(&self, method: &str, path: impl Into<Path>, handler: Handler, options: Value) -> Result<Route>;
    fn get<F, Fut>(&self, path: impl Into<Path>, callback: F) -> Result<Route>
    where
        F: Fn(Request, Response, Next) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Option<Response>>> + Send + 'static;
    fn post<F, Fut>(&self, path: impl Into<Path>, callback: F) -> Result<Route>
    where
        F: Fn(Request, Response, Next) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Option<Response>>> + Send + 'static;
    fn all<F, Fut>(&self, path: impl Into<Path>, callback: F) -> Result<Route>
    where
        F: Fn(Request, Response, Next) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Option<Response>>> + Send + 'static;
    fn use_middleware(&self, handler: Handler) -> Result<EffectHandle>;
    fn on_route_request(&self, hook: RouteHook) -> Result<EffectHandle>;
    fn on_route_check(&self, check: RouteCheck) -> Result<EffectHandle>;
}

impl ServerExt for Svc<Server> {
    fn public_url(&self) -> String {
        let meta = self.meta();
        let prefix = meta["path"].as_str().unwrap_or_default();
        if let Some(url) = &self.config.public_url {
            return format!("{}{prefix}", url.trim_end_matches('/'));
        }
        let host = match self.config.host.as_str() {
            "0.0.0.0" | "::" => "127.0.0.1",
            host => host,
        };
        // An IPv6 address is bracketed in the authority of a URL, or its colons
        // would read as the separator before the port.
        let authority = if host.contains(':') { format!("[{host}]") } else { host.to_owned() };
        match self.address.port() {
            80 => format!("http://{authority}{prefix}"),
            443 => format!("https://{authority}{prefix}"),
            port => format!("http://{authority}:{port}{prefix}"),
        }
    }

    fn route(&self, method: &str, path: impl Into<Path>, callback: Handler, options: Value) -> Result<Route> {
        let path = path.into();
        let meta = self.meta();
        let mut options = options.as_object().cloned().unwrap_or_default();
        if let Path::String(path) = &path {
            if let Some(overrides) = meta["routes"][format!("{} {path}", method.to_uppercase())].as_object() {
                options.extend(overrides.clone());
            }
        }
        let info = Arc::new(RouteInfo {
            method: method.to_lowercase(),
            pattern: Pattern::new(&path)?,
            path,
            options: Value::Object(options),
            prefix: meta["path"].as_str().unwrap_or_default().into(),
            fiber: self.caller().fiber(),
        });
        let server = self.arc().clone();
        let id = self.next_id();
        let calls = crate::ownership::Calls::new();
        let callback = calls.handler(callback);
        let effect = self.caller().effect_sync("server route", |scope| {
            server.routes.lock().push(Arc::new(HttpRoute { id, info: info.clone(), handler: callback }));
            scope.defer(Disposer::future(move || {
                server.routes.lock().retain(|route| route.id != id);
                calls.close()
            }));
            Ok(())
        })?;
        Ok(Route::new(info, effect))
    }

    fn get<F, Fut>(&self, path: impl Into<Path>, callback: F) -> Result<Route>
    where
        F: Fn(Request, Response, Next) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Option<Response>>> + Send + 'static,
    {
        self.route("get", path, crate::route::handler(callback), json!({}))
    }
    fn post<F, Fut>(&self, path: impl Into<Path>, callback: F) -> Result<Route>
    where
        F: Fn(Request, Response, Next) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Option<Response>>> + Send + 'static,
    {
        self.route("post", path, crate::route::handler(callback), json!({}))
    }
    fn all<F, Fut>(&self, path: impl Into<Path>, callback: F) -> Result<Route>
    where
        F: Fn(Request, Response, Next) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Option<Response>>> + Send + 'static,
    {
        self.route("all", path, crate::route::handler(callback), json!({}))
    }

    fn use_middleware(&self, handler: Handler) -> Result<EffectHandle> {
        let server = self.arc().clone();
        let id = self.next_id();
        let calls = crate::ownership::Calls::new();
        let handler = calls.handler(handler);
        self.caller().effect_sync("server request hook", |scope| {
            server.middleware.lock().insert(0, (id, handler));
            scope.defer(Disposer::future(move || {
                server.middleware.lock().retain(|(key, _)| *key != id);
                calls.close()
            }));
            Ok(())
        })
    }
    fn on_route_request(&self, hook: RouteHook) -> Result<EffectHandle> {
        let server = self.arc().clone();
        let id = self.next_id();
        let calls = crate::ownership::Calls::new();
        let owner = calls.clone();
        let hook: RouteHook = Arc::new(move |req, res, info, next| {
            let calls = owner.clone();
            let hook = hook.clone();
            async move {
                let owner = calls.clone();
                calls
                    .run(async move {
                        let result = crate::dispatch::contain(async { hook(req, res.clone(), info, next).await }).await;
                        owner.response(result.as_ref().ok().and_then(Option::as_ref).unwrap_or(&res))?;
                        result
                    })
                    .await
            }
            .boxed()
        });
        self.caller().effect_sync("server route request hook", |scope| {
            server.hooks.lock().push((id, hook));
            scope.defer(Disposer::future(move || {
                server.hooks.lock().retain(|(key, _)| *key != id);
                calls.close()
            }));
            Ok(())
        })
    }
    fn on_route_check(&self, check: RouteCheck) -> Result<EffectHandle> {
        let server = self.arc().clone();
        let id = self.next_id();
        self.caller().effect_sync("server route check", |scope| {
            server.checks.lock().push((id, check));
            scope.defer_fn(move || {
                server.checks.lock().retain(|(key, _)| *key != id);
            });
            Ok(())
        })
    }
}

pub(crate) fn chain(
    handlers: impl DoubleEndedIterator<Item = Handler>,
    req: Request,
    res: Response,
    mut next: Next,
) -> Next {
    for handler in handlers.rev() {
        let req = req.clone();
        let res = res.clone();
        next = async move { handler(req, res, next).await }.boxed();
    }
    next
}
