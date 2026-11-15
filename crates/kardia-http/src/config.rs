use crate::{Body, Http};
use http::{HeaderMap, Method};
use kardia_core::{Result, Svc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpConfig {
    pub base_url: Option<String>,
    pub headers: Option<BTreeMap<String, String>>,
    pub timeout: Option<u64>,
    pub keep_alive: Option<bool>,
    pub proxy_agent: Option<String>,
    #[serde(default)]
    pub root_certificates: Vec<String>,
}
#[derive(Clone, Copy, Debug, Default)]
pub enum Redirect {
    #[default]
    Follow,
    Manual,
    Error,
}
#[derive(Default)]
pub struct RequestConfig {
    pub method: Option<Method>,
    pub headers: Option<HeaderMap>,
    pub body: Option<Body>,
    pub params: Vec<(String, String)>,
    pub base_url: Option<String>,
    pub timeout: Option<u64>,
    pub keep_alive: Option<bool>,
    pub proxy_agent: Option<String>,
    pub redirect: Redirect,
}
pub(crate) struct Resolved {
    pub url: url::Url,
    pub method: Method,
    pub headers: HeaderMap,
    pub body: Option<Body>,
    pub timeout: Option<u64>,
    pub keep_alive: bool,
    pub proxy_agent: Option<String>,
    pub redirect: Redirect,
}
impl RequestConfig {
    pub(crate) fn resolve(self, http: &Svc<Http>, location: &str) -> Result<Resolved> {
        let mut config = serde_json::to_value(&http.config)?;
        if let Some(meta) = http.meta().as_object() {
            for (k, v) in meta {
                config[k] = v.clone();
            }
        }
        let config: kardia_core::Result<HttpConfig> = kardia_core::decode(&config).map_err(Into::into);
        let config = config?;
        let base = self.base_url.or(config.base_url).map(|v| url::Url::parse(&v)).transpose()?;
        let mut url = match base {
            Some(base) => base.join(location)?,
            None => url::Url::parse(location)?,
        };
        if !self.params.is_empty() {
            url.query_pairs_mut().extend_pairs(self.params);
        }
        let headers = match self.headers {
            Some(headers) => headers,
            None => {
                let mut headers = HeaderMap::new();
                for (k, v) in config.headers.unwrap_or_default() {
                    headers.insert(http::HeaderName::from_bytes(k.as_bytes())?, http::HeaderValue::from_str(&v)?);
                }
                headers
            }
        };
        let mut method = self.method.unwrap_or(Method::GET);
        let upper = method.as_str().to_ascii_uppercase();
        if matches!(upper.as_str(), "DELETE" | "GET" | "HEAD" | "OPTIONS" | "POST" | "PUT") {
            method = upper.parse()?;
        }
        Ok(Resolved {
            url,
            headers,
            method,
            body: self.body,
            timeout: self.timeout.or(config.timeout),
            keep_alive: self.keep_alive.or(config.keep_alive).unwrap_or(false),
            proxy_agent: self.proxy_agent.or(config.proxy_agent),
            redirect: self.redirect,
        })
    }
}
