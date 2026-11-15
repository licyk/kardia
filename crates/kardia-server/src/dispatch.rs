use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures::FutureExt;
use http::{HeaderValue, Method, StatusCode, header};
use kardia_core::{Panicked, Result};

use crate::route::{HttpRoute, RouteCheck, RouteHook};
use crate::{Next, Request, Response, Server, ValidationError};

pub(crate) async fn contain<T>(future: impl Future<Output = Result<T>>) -> Result<T> {
    AssertUnwindSafe(future).catch_unwind().await.unwrap_or_else(|panic| {
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap_or_else(|| "plugin callback panicked".into());
        Err(Panicked(message).into())
    })
}

impl Server {
    pub(crate) async fn dispatch(self: Arc<Self>, req: Request) -> Response {
        let res = Response::new();
        let middleware = self.middleware.lock().iter().map(|(_, h)| h.clone()).collect::<Vec<_>>();
        let server = self.clone();
        let request = req.clone();
        let response = res.clone();
        let next = async move { server.dispatch_routes(request, response).await }.boxed();
        let result = contain(crate::server::chain(middleware.into_iter(), req, res.clone(), next)).await;
        if let Err(error) = result {
            self.ctx.logger().log_error(&error);
            if !res.claimed() {
                if let Some(validation) = error.downcast_ref::<ValidationError>() {
                    res.set_status(StatusCode::UNPROCESSABLE_ENTITY);
                    let _ = res.set_json(&serde_json::json!({ "issues": validation.issues }));
                } else {
                    res.set_status(StatusCode::INTERNAL_SERVER_ERROR);
                }
            }
        }
        res
    }

    async fn dispatch_routes(self: Arc<Self>, req: Request, res: Response) -> Result<Option<Response>> {
        let routes = self.routes.lock().clone();
        let checks = self.checks.lock().iter().map(|(_, c)| c.clone()).collect::<Vec<_>>();
        let hooks = self.hooks.lock().iter().map(|(_, h)| h.clone()).collect::<Vec<_>>();
        let dispatch = Arc::new(Dispatch { routes, checks, hooks });
        if let Some(response) = dispatch.run(0, req.clone(), res.clone()).await? {
            res.adopt(&response);
        }
        if res.claimed() {
            return Ok(None);
        }
        let mut methods = Vec::new();
        for route in self.routes.lock().clone() {
            if route.info.matches(&req, &dispatch.checks)?.is_none() {
                continue;
            }
            if route.info.method == "all" {
                methods = vec!["*".into()];
                break;
            }
            if !methods.contains(&route.info.method) {
                methods.push(route.info.method.clone());
            }
        }
        if methods.is_empty() {
            res.set_status(StatusCode::NOT_FOUND);
        } else {
            res.set_status(if req.method == Method::OPTIONS {
                StatusCode::NO_CONTENT
            } else {
                StatusCode::METHOD_NOT_ALLOWED
            });
            res.set_header(header::ALLOW, HeaderValue::from_str(&methods.join(", "))?);
        }
        Ok(None)
    }
}

struct Dispatch {
    routes: Vec<Arc<HttpRoute>>,
    checks: Vec<RouteCheck>,
    hooks: Vec<RouteHook>,
}
impl Dispatch {
    fn run(self: &Arc<Self>, index: usize, req: Request, res: Response) -> Next {
        let dispatch = self.clone();
        async move {
            for (index, route) in dispatch.routes.iter().enumerate().skip(index) {
                if route.info.method != "all" && !route.info.method.eq_ignore_ascii_case(req.method.as_str()) {
                    continue;
                }
                let Some(matched) = route.info.matches(&req, &dispatch.checks)? else { continue };
                let next = dispatch.run(index + 1, req.clone(), res.clone());
                let handler = route.handler.clone();
                let response = res.clone();
                let request = matched.clone();
                let mut next: Next = async move { handler(request, response, next).await }.boxed();
                for hook in dispatch.hooks.iter().rev() {
                    let hook = hook.clone();
                    let request = matched.clone();
                    let response = res.clone();
                    let info = route.info.clone();
                    next = async move { hook(request, response, info, next).await }.boxed();
                }
                return next.await;
            }
            Ok(None)
        }
        .boxed()
    }
}
