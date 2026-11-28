use crate::{Disposition, StaticConfig, detect::detect, generated::extension_mime, probe::Probe};
use futures::{StreamExt, stream};
use kardia_core::{Context, Error, Result, Svc};
use kardia_fs::{Fs, FsCode, FsError, FsExt, Url};
use kardia_server::{Body, HeaderName, HeaderValue, Next, Request, Response, StatusCode};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use picomatch_rs::Matcher;
use std::sync::Arc;

const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');
pub(super) struct Files {
    fs: Svc<Fs>,
    root: Url,
    config: Arc<StaticConfig>,
    exclude: Vec<Matcher>,
}
impl Files {
    pub fn new(ctx: &Context, config: Arc<StaticConfig>) -> Result<Self> {
        let fs = ctx.intercept("std:fs", serde_json::json!({"baseUrl": config.root})).get::<Fs>()?;
        let root = fs.base_url().map_err(|error| kardia_core::ValidationError::message(format!("root: {error}")))?;
        let exclude =
            config.exclude.iter().map(|p| crate::exclude::compile(p).map_err(Error::from)).collect::<Result<_>>()?;
        Ok(Self { fs, root, config, exclude })
    }
    pub async fn serve(&self, req: Request, next: Next) -> Result<Option<Response>> {
        let path = req.params.get("path").and_then(|p| p.as_str()).unwrap_or_default().trim_start_matches('/');
        let url = self.locate(path, &self.root)?;
        let status = if let Some(url) = url {
            if url.path().ends_with('/') {
                for index in &self.config.index {
                    if let Some(location) = self.locate(index, &url)? {
                        if let Outcome::Response(response) = self.try_file(location).await? {
                            return Ok(Some(response));
                        }
                    }
                }
                StatusCode::NOT_FOUND
            } else {
                let error = match self.try_file(url).await? {
                    Outcome::Response(r) => return Ok(Some(r)),
                    Outcome::Missing(e) => e,
                };
                if error == Some(FsCode::IsADirectory) && self.config.redirect && !req.path.ends_with('/') {
                    let response = response(StatusCode::MOVED_PERMANENTLY);
                    response.set_header(
                        HeaderName::from_static("location"),
                        HeaderValue::from_str(&format!("{}/", req.path))?,
                    );
                    return Ok(Some(response));
                }
                status(error)
            }
        } else {
            StatusCode::FORBIDDEN
        };
        if status == StatusCode::NOT_FOUND && self.config.fallthrough {
            return next.await;
        }
        if let Some(path) = self.config.error_pages.get(status.as_str()).filter(|p| !p.is_empty()) {
            let url = self.root.join(&encode_path(path))?;
            if let Outcome::Response(response) = self.read(url, None).await? {
                response.set_status(status);
                return Ok(Some(response));
            }
        }
        Ok(Some(response(status)))
    }
    fn locate(&self, path: &str, base: &Url) -> Result<Option<Url>> {
        let url = base.join(&encode_path(path))?;
        Ok(url.as_str().starts_with(self.root.as_str()).then_some(url))
    }
    fn excluded(&self, url: &Url) -> Result<bool> {
        let path = url.path().get(self.root.path().len()..).unwrap_or_default();
        let path = percent_decode_str(path).decode_utf8()?;
        for matcher in &self.exclude {
            if matcher.is_match(&path).map_err(|e| Error::msg(format!("exclusion match: {e:?}")))? {
                return Ok(true);
            }
        }
        Ok(false)
    }
    async fn try_file(&self, url: Url) -> Result<Outcome> {
        let result = if self.excluded(&url)? {
            Outcome::Missing(None)
        } else {
            self.read(url.clone(), self.config.disposition).await?
        };
        if matches!(result, Outcome::Response(_)) {
            return Ok(result);
        }
        // The extensions name alternatives for a missing file. A directory is not
        // missing: its redirect is decided outside, before any extension is tried.
        if matches!(result, Outcome::Missing(Some(FsCode::IsADirectory))) {
            return Ok(result);
        }
        for ext in &self.config.extensions {
            let candidate = Url::parse(&format!("{}{}", url, utf8_percent_encode(ext, COMPONENT)))?;
            if self.excluded(&candidate)? {
                continue;
            }
            if let Outcome::Response(response) = self.read(candidate, self.config.disposition).await? {
                return Ok(Outcome::Response(response));
            }
        }
        Ok(result)
    }
    async fn read(&self, url: Url, disposition: Option<Disposition>) -> Result<Outcome> {
        let first: Result<_> = async {
            let mut reader = self.fs.file(url.as_str())?.stream()?;
            let first = reader.next().await.transpose()?;
            Ok((reader, first))
        }
        .await;
        let (reader, first) = match first {
            Ok(value) => value,
            Err(error) => {
                return match error.downcast_ref::<FsError>() {
                    Some(error) => {
                        if status(Some(error.code)) == StatusCode::INTERNAL_SERVER_ERROR {
                            self.fs.caller().logger().warn(error);
                        }
                        Ok(Outcome::Missing(Some(error.code)))
                    }
                    None => Err(error),
                };
            }
        };
        let filename = percent_decode_str(url.path().rsplit('/').next().unwrap_or_default()).decode_utf8()?;
        let mime = if first.is_some() {
            match self.fs.file(url.as_str())?.stream() {
                Ok(source) => detect(&mut Probe::new(source)).await.ok().flatten(),
                Err(_) => None,
            }
        } else {
            None
        };
        let extension = filename.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
        let mime = mime.or_else(|| extension_mime(&extension)).unwrap_or("application/octet-stream");
        let response = response(StatusCode::OK);
        response.set_header(HeaderName::from_static("content-type"), HeaderValue::from_static(mime));
        match disposition {
            Some(Disposition::Attachment) => response.set_header(
                HeaderName::from_static("content-disposition"),
                HeaderValue::from_str(&format!(
                    "attachment; filename*=UTF-8''{}",
                    utf8_percent_encode(&filename, COMPONENT)
                ))?,
            ),
            Some(Disposition::Inline) => {
                response.set_header(HeaderName::from_static("content-disposition"), HeaderValue::from_static("inline"))
            }
            None => {}
        }
        if let Some(first) = first {
            response.set_body(Body::stream(stream::once(async move { Ok(first) }).chain(reader)));
        }
        Ok(Outcome::Response(response))
    }
}
enum Outcome {
    Response(Response),
    Missing(Option<FsCode>),
}
fn response(status: StatusCode) -> Response {
    let response = Response::new();
    response.set_status(status);
    response
}
fn status(code: Option<FsCode>) -> StatusCode {
    match code {
        None | Some(FsCode::NotFound | FsCode::NotADirectory | FsCode::IsADirectory | FsCode::InvalidPath) => {
            StatusCode::NOT_FOUND
        }
        Some(FsCode::PermissionDenied) => StatusCode::FORBIDDEN,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
fn encode_path(path: &str) -> String {
    path.split('/').map(|p| utf8_percent_encode(p, COMPONENT).to_string()).collect::<Vec<_>>().join("/")
}
