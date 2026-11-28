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
    /// The hosts that bypass the proxy: `*`, a host with its subdomains, or a
    /// host and port.
    #[serde(default)]
    pub no_proxy: Vec<String>,
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
    pub no_proxy: Option<Vec<String>>,
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
    pub no_proxy: Vec<String>,
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
            no_proxy: self.no_proxy.or(Some(config.no_proxy)).unwrap_or_default(),
            redirect: self.redirect,
        })
    }
}

/// Whether `url` bypasses the proxy, as `noProxy` of Cordis decides it: `*`
/// matches every host, an entry matches its subdomains, and an entry with a
/// port matches only that port.
pub(crate) fn bypass_proxy(url: &url::Url, no_proxy: &[String]) -> bool {
    // `host_str()` reports an IPv6 literal without its brackets.
    let hostname =
        url.host_str().unwrap_or_default().trim_end_matches('.').trim_start_matches('[').trim_end_matches(']');
    let port = url.port_or_known_default();
    no_proxy.iter().any(|entry| {
        if entry == "*" {
            return true;
        }
        let (host, entry_port) = split_host_port(entry);
        if let Some(entry_port) = entry_port
            && Some(entry_port) != port
        {
            return false;
        }
        let domain = host.trim_start_matches("*.").trim_start_matches('.').trim_end_matches('.');
        if domain.is_empty() {
            return false;
        }
        hostname == domain || hostname.ends_with(&format!(".{domain}"))
    })
}

/// Splits `host[:port]`, keeping an IPv6 literal in brackets together.
fn split_host_port(entry: &str) -> (&str, Option<u16>) {
    if let Some(rest) = entry.strip_prefix('[') {
        return match rest.split_once(']') {
            Some((host, tail)) => (host, tail.strip_prefix(':').and_then(|port| port.parse().ok())),
            None => (entry, None),
        };
    }
    match entry.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => (host, port.parse().ok()),
        // An unbracketed IPv6 literal carries colons of its own.
        _ => (entry, None),
    }
}

#[cfg(test)]
mod tests {
    use super::bypass_proxy;
    use url::Url;

    fn bypass(url: &str, entries: &[&str]) -> bool {
        let entries: Vec<String> = entries.iter().map(|entry| (*entry).to_owned()).collect();
        bypass_proxy(&Url::parse(url).unwrap(), &entries)
    }

    #[test]
    fn a_star_bypasses_every_host() {
        assert!(bypass("https://example.com/", &["*"]));
        assert!(bypass("http://127.0.0.1:8000/", &["*"]));
    }

    #[test]
    fn a_host_matches_itself_and_its_subdomains() {
        assert!(bypass("https://example.com/", &["example.com"]));
        assert!(bypass("https://api.example.com/", &["example.com"]));
        assert!(bypass("https://deep.api.example.com/", &[".example.com"]));
        assert!(!bypass("https://badexample.com/", &["example.com"]));
        assert!(!bypass("https://example.com.evil.test/", &["example.com"]));
    }

    #[test]
    fn a_port_limits_the_entry_to_that_port() {
        assert!(bypass("http://127.0.0.1:8080/", &["127.0.0.1:8080"]));
        assert!(!bypass("http://127.0.0.1:9090/", &["127.0.0.1:8080"]));
        // The default port of the scheme still matches an entry without one.
        assert!(bypass("https://example.com/", &["example.com"]));
    }

    #[test]
    fn an_ipv6_literal_keeps_its_brackets_apart_from_the_port() {
        assert!(bypass("http://[::1]:8080/", &["[::1]:8080"]));
        assert!(bypass("http://[::1]/", &["[::1]"]));
        assert!(!bypass("http://[::1]:9090/", &["[::1]:8080"]));
    }

    #[test]
    fn an_empty_list_bypasses_nothing() {
        assert!(!bypass("https://example.com/", &[]));
    }
}
