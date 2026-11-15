use std::sync::Arc;

use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use kardia_core::Result;
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::Value;

use crate::{Blob, Body, FormData};

#[derive(Clone, Default)]
pub struct Response(Arc<Mutex<ResponseData>>);

pub(crate) struct ResponseData {
    pub status: StatusCode,
    pub status_text: Option<hyper::ext::ReasonPhrase>,
    pub headers: HeaderMap,
    pub body: Body,
    claimed: bool,
}
impl Default for ResponseData {
    fn default() -> Self {
        Self {
            status: StatusCode::OK,
            status_text: None,
            headers: HeaderMap::new(),
            body: Body::default(),
            claimed: false,
        }
    }
}

impl Response {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn status(&self) -> StatusCode {
        self.0.lock().status
    }
    pub fn set_status(&self, status: StatusCode) {
        let mut data = self.0.lock();
        data.status = status;
        data.claimed = true;
    }
    pub fn status_text(&self) -> Option<String> {
        self.0.lock().status_text.as_ref().map(|text| String::from_utf8_lossy(text.as_bytes()).into_owned())
    }
    pub fn set_status_text(&self, text: String) -> Result<()> {
        self.0.lock().status_text = Some(hyper::ext::ReasonPhrase::try_from(text)?);
        Ok(())
    }
    pub fn ok(&self) -> bool {
        self.status().is_success()
    }
    pub fn redirected(&self) -> bool {
        self.status().is_redirection()
    }
    pub fn claimed(&self) -> bool {
        self.0.lock().claimed
    }
    pub fn headers(&self) -> HeaderMap {
        self.0.lock().headers.clone()
    }
    pub fn set_header(&self, name: HeaderName, value: HeaderValue) {
        self.0.lock().headers.insert(name, value);
    }
    pub fn append_header(&self, name: HeaderName, value: HeaderValue) {
        self.0.lock().headers.append(name, value);
    }
    pub fn remove_header(&self, name: &str) {
        self.0.lock().headers.remove(name);
    }
    pub fn body(&self) -> Body {
        self.0.lock().body.fork()
    }
    pub fn set_body(&self, body: impl Into<Body>) {
        let mut data = self.0.lock();
        data.body = body.into();
        data.claimed = true;
    }
    pub fn set_text(&self, text: impl Into<String>) {
        self.content_type("text/plain;charset=UTF-8");
        self.set_body(text.into());
    }
    pub fn set_json(&self, value: &impl Serialize) -> Result<()> {
        self.content_type("application/json");
        self.set_body(crate::json::stringify(value)?);
        Ok(())
    }
    pub async fn bytes(&self) -> Result<Bytes> {
        self.body().bytes().await
    }
    pub async fn text(&self) -> Result<String> {
        Ok(crate::form::text(&self.bytes().await?))
    }
    pub fn set_blob(&self, blob: Blob) {
        self.write_typed(blob.into());
    }
    pub fn set_form_data(&self, form: FormData) {
        self.write_typed(form.into());
    }
    pub async fn blob(&self) -> Result<Blob> {
        let body = self.body();
        let content_type = body.1.clone().unwrap_or_default().to_ascii_lowercase();
        Ok(Blob { bytes: body.bytes().await?, content_type })
    }
    pub async fn form_data(&self) -> Result<FormData> {
        let body = self.body();
        let content_type = body.1.clone().unwrap_or_default();
        FormData::read(body, &content_type).await
    }
    pub async fn json(&self) -> Result<Value> {
        Ok(serde_json::from_str(&self.text().await?)?)
    }
    pub(crate) fn take_body(&self) -> Body {
        std::mem::take(&mut self.0.lock().body)
    }
    pub(crate) fn take(&self) -> ResponseData {
        std::mem::take(&mut *self.0.lock())
    }
    pub(crate) fn adopt(&self, response: &Self) {
        if Arc::ptr_eq(&self.0, &response.0) {
            return;
        }
        let incoming = response.take();
        let mut data = self.0.lock();
        data.body = incoming.body;
        data.status = incoming.status;
        data.headers.extend(incoming.headers);
        data.claimed = true;
    }
    fn content_type(&self, value: &'static str) {
        self.0.lock().headers.entry(http::header::CONTENT_TYPE).or_insert(HeaderValue::from_static(value));
    }
    fn write_typed(&self, body: Body) {
        if let Some(content_type) = &body.1 {
            self.0
                .lock()
                .headers
                .entry(http::header::CONTENT_TYPE)
                .or_insert(HeaderValue::from_str(content_type).expect("body content type"));
        }
        self.set_body(body);
    }
}
