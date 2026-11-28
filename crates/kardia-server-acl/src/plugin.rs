use std::sync::Arc;

use kardia_core::{Context, Inject, Plugin, Result};
use kardia_server::{Server, ServerExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AllowedHosts {
    Any(bool),
    List(Vec<String>),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AclConfig {
    pub allowed_hosts: Option<AllowedHosts>,
}

pub struct AclPlugin;
impl Plugin for AclPlugin {
    const NAME: &'static str = "server:acl";
    type Config = AclConfig;
    fn inject() -> Inject {
        Inject::new().with("std:server")
    }
    async fn apply(ctx: Context, config: Arc<AclConfig>) -> Result<()> {
        let defaults = serde_json::to_value(&config.allowed_hosts)?;
        ctx.get::<Server>()?.on_route_check(Arc::new(move |req, route| {
            let policy = route.options.get("allowedHosts").filter(|v| !v.is_null()).unwrap_or(&defaults);
            let host = req.headers.get("host").and_then(|h| h.to_str().ok()).unwrap_or_default();
            !allowed(policy, host)
        }))?;
        Ok(())
    }
}

fn allowed(policy: &Value, host: &str) -> bool {
    if policy.is_null() || policy == &Value::Bool(true) {
        return true;
    }
    let host = host.to_lowercase();
    let host = if host.starts_with('[') {
        host.find(']').map_or(host.as_str(), |end| &host[..=end])
    } else {
        host.split(':').next().unwrap_or_default()
    };
    if matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]") {
        return true;
    }
    if policy == &Value::Bool(false) {
        return false;
    }
    // Route options are extensible JSON, as upstream. Invalid known values
    // fail this operation through the server callback boundary.
    policy.as_array().expect("allowedHosts must be boolean or string array").iter().any(|pattern| {
        let pattern = pattern.as_str().expect("allowedHosts pattern must be a string").to_lowercase();
        if let Some(domain) = pattern.strip_prefix('.') {
            host == domain || host.ends_with(&pattern)
        } else {
            host == pattern
        }
    })
}
