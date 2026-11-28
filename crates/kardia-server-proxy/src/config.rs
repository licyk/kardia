//! The proxy configuration, its validation and the prefix it forwards under.

use std::collections::BTreeMap;

use kardia_core::Result;
use kardia_http::Url;
use serde::{Deserialize, Serialize};

/// A hop-by-hop header, which describes one connection and is never forwarded.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// The headers the upstream client supplies itself, or handles together with
/// the request body, so a mapping may not set them.
pub(crate) const RESERVED_REQUEST: &[&str] = &[
    HOP_BY_HOP[0],
    HOP_BY_HOP[1],
    HOP_BY_HOP[2],
    HOP_BY_HOP[3],
    HOP_BY_HOP[4],
    HOP_BY_HOP[5],
    HOP_BY_HOP[6],
    HOP_BY_HOP[7],
    HOP_BY_HOP[8],
    "host",
    "expect",
];

/// Undici decodes the body, so its upstream encoding and length no longer
/// describe the bytes that are forwarded.
pub(crate) const RESERVED_RESPONSE: &[&str] = &[
    HOP_BY_HOP[0],
    HOP_BY_HOP[1],
    HOP_BY_HOP[2],
    HOP_BY_HOP[3],
    HOP_BY_HOP[4],
    HOP_BY_HOP[5],
    HOP_BY_HOP[6],
    HOP_BY_HOP[7],
    HOP_BY_HOP[8],
    "content-encoding",
    "content-length",
];

/// The handshake headers a WebSocket client supplies itself.
pub(crate) const RESERVED_WEBSOCKET: &[&str] = &[
    "sec-websocket-key",
    "sec-websocket-version",
    "sec-websocket-extensions",
    "sec-websocket-accept",
    "content-length",
];

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyConfig {
    /// The prefix of the forwarded URLs: a URL ending with `/`, a scheme such as
    /// `https:`, or the empty string for every HTTP URL.
    pub target: String,
    /// Filter the incoming `Cookie` header, drop `Set-Cookie` and
    /// `Clear-Site-Data` from upstream responses, and sandbox upstream documents.
    #[serde(default)]
    pub isolate_origin: bool,
    /// Maps each upstream request header to its sole incoming source.
    #[serde(default)]
    pub request_headers: BTreeMap<String, String>,
}

#[derive(Clone)]
/// How a target prefix decides which URLs lie under it.
pub(crate) enum Target {
    Every,
    Scheme(String),
    Url { raw: Url, decoded: Url },
}

impl Target {
    pub(crate) fn parse(target: &str) -> Result<Self> {
        if target.is_empty() {
            return Ok(Self::Every);
        }
        if let Some(scheme) = target.strip_suffix(':')
            && !scheme.is_empty()
            && scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        {
            return Ok(Self::Scheme(format!("{}:", scheme.to_ascii_lowercase())));
        }
        let raw = Url::parse(target).map_err(|error| {
            kardia_core::Error::from(kardia_core::ValidationError::message(format!("target: {error}")))
        })?;
        if !matches!(raw.scheme(), "http" | "https") {
            return Err(kardia_core::ValidationError::message(
                "target: only an http or https URL is forwarded, or the empty string",
            )
            .into());
        }
        if !raw.path().ends_with('/') || raw.query().is_some() || raw.fragment().is_some() {
            return Err(kardia_core::ValidationError::message(
                "target: a URL prefix ends with `/` and has no query or fragment",
            )
            .into());
        }
        // The forwarded URL and the target are also compared with their encoded
        // separators decoded, as an upstream that decodes them before it
        // normalizes the path would take `..%2F` out of the target.
        let decoded = decode_separators(&raw);
        Ok(Self::Url { raw, decoded })
    }

    /// The URL a request path and query are forwarded to, or `None` when the
    /// result leaves the prefix.
    pub(crate) fn locate(&self, target: &str, path: &str, query: &str) -> Option<Url> {
        let mut url = Url::parse(&format!("{target}{path}{query}")).ok()?;
        let contained = match self {
            Self::Every => true,
            Self::Scheme(scheme) => url.scheme() == scheme.trim_end_matches(':'),
            Self::Url { raw, decoded } => {
                url.as_str().starts_with(raw.as_str()) && decode_separators(&url).as_str().starts_with(decoded.as_str())
            }
        };
        if !contained {
            return None;
        }
        url.set_fragment(None);
        Some(url)
    }
}

/// The URL with `%2F` and `%5C` read as separators, which the URL parser then
/// normalizes like any other separator.
fn decode_separators(url: &Url) -> Url {
    let path = url.path().replace("%2F", "/").replace("%2f", "/").replace("%5C", "/").replace("%5c", "/");
    Url::parse(&format!(
        "{}{}{}",
        url.origin().ascii_serialization(),
        path,
        url.query().map(|q| format!("?{q}")).unwrap_or_default()
    ))
    .unwrap_or_else(|_| url.clone())
}

/// The lowercase field names an incoming `Connection` header marks as
/// hop-by-hop for this hop.
pub(crate) fn connection_fields(connection: Option<&str>) -> Vec<String> {
    connection
        .unwrap_or_default()
        .split(',')
        .map(|field| field.trim().to_ascii_lowercase())
        .filter(|field| !field.is_empty())
        .collect()
}
