use std::sync::Arc;

use bytes::Bytes;
use http::{HeaderMap, Method};
use kardia_core::{Error, Result};
use parking_lot::Mutex;
use serde_json::Value;

use crate::Body;

#[derive(Debug, thiserror::Error)]
#[error("Validation failed")]
pub struct ValidationError {
    pub issues: Vec<Value>,
}

#[derive(Clone)]
pub struct Request {
    pub url: String,
    pub method: Method,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: HeaderMap,
    pub params: Value,
    body: Arc<Mutex<Option<Body>>>,
}

impl Request {
    pub fn new(method: Method, url: String, headers: HeaderMap, body: Body) -> Self {
        let mut parts = url.split('?');
        let path = parts.next().unwrap_or_default().to_owned();
        let query = url::form_urlencoded::parse(parts.next().unwrap_or_default().as_bytes()).into_owned().collect();
        Self { url, method, path, query, headers, params: Value::Null, body: Arc::new(Mutex::new(Some(body))) }
    }

    pub fn body_used(&self) -> bool {
        self.body.lock().is_none()
    }
    pub fn take_body(&self) -> Result<Body> {
        self.body.lock().take().ok_or_else(|| Error::msg("request body already consumed"))
    }
    pub async fn bytes(&self) -> Result<Bytes> {
        self.take_body()?.bytes().await
    }
    pub async fn text(&self) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.bytes().await?).trim_start_matches('\u{feff}').into())
    }
    pub async fn json(&self) -> Result<Value> {
        Ok(serde_json::from_str(&self.text().await?)?)
    }
    pub async fn validate_json<T>(
        &self,
        validate: impl AsyncFnOnce(Value) -> std::result::Result<T, ValidationError>,
    ) -> Result<T> {
        Ok(validate(self.json().await?).await?)
    }
}
