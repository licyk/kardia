use std::future::Future;
use std::sync::Arc;

use futures::future::BoxFuture;
use kardia_core::{Disposal, EffectHandle, Fiber, Result};
use serde_json::Value;

use crate::pattern::Pattern;
use crate::{Path, Request, Response};

pub type Next = BoxFuture<'static, Result<Option<Response>>>;
pub type Handler = Arc<dyn Fn(Request, Response, Next) -> Next + Send + Sync>;
pub type RouteHook = Arc<dyn Fn(Request, Response, Arc<RouteInfo>, Next) -> Next + Send + Sync>;
pub type RouteCheck = Arc<dyn Fn(&Request, &RouteInfo) -> bool + Send + Sync>;

pub struct RouteInfo {
    pub method: String,
    pub path: Path,
    pub options: Value,
    pub fiber: Fiber,
    pub(crate) prefix: String,
    pub(crate) pattern: Pattern,
}

impl RouteInfo {
    pub(crate) fn matches(&self, req: &Request, checks: &[RouteCheck]) -> Result<Option<Request>> {
        let Some(path) = req.path.strip_prefix(&self.prefix) else { return Ok(None) };
        let Some(params) = self.pattern.check(path)? else { return Ok(None) };
        let mut matched = req.clone();
        matched.params = params;
        if checks.iter().any(|check| check(&matched, self)) {
            return Ok(None);
        }
        Ok(Some(matched))
    }
}

pub struct Route {
    pub info: Arc<RouteInfo>,
    effect: EffectHandle,
}
impl Route {
    pub(crate) fn new(info: Arc<RouteInfo>, effect: EffectHandle) -> Self {
        Self { info, effect }
    }
    pub fn dispose(&self) -> Disposal {
        self.effect.dispose()
    }
}

pub(crate) struct HttpRoute {
    pub id: u64,
    pub info: Arc<RouteInfo>,
    pub handler: Handler,
}

pub fn handler<F, Fut>(callback: F) -> Handler
where
    F: Fn(Request, Response, Next) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Option<Response>>> + Send + 'static,
{
    Arc::new(move |req, res, next| Box::pin(callback(req, res, next)))
}
