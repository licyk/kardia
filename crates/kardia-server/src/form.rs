use std::sync::atomic::{AtomicU64, Ordering};

use futures::StreamExt;
use kardia_core::{Error, Result};

use crate::{Body, Bytes};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blob {
    pub bytes: Bytes,
    pub content_type: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormValue {
    Text(String),
    File { filename: String, blob: Blob },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormEntry {
    pub name: String,
    pub value: FormValue,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FormData(pub Vec<FormEntry>);

impl From<Blob> for Body {
    fn from(blob: Blob) -> Self {
        let mut body: Self = blob.bytes.into();
        body.1 = blob_type(&blob.content_type);
        body
    }
}

impl FormData {
    pub(crate) async fn read(body: Body, content_type: &str) -> Result<Self> {
        if content_type
            .split(';')
            .next()
            .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("application/x-www-form-urlencoded"))
        {
            let bytes = body.bytes().await?;
            return Ok(Self(
                url::form_urlencoded::parse(&bytes)
                    .into_owned()
                    .map(|(name, value)| FormEntry { name, value: FormValue::Text(value) })
                    .collect(),
            ));
        }
        let boundary = multer::parse_boundary(content_type)?;
        let mut multipart = multer::Multipart::new(
            body.into_stream().map(|item| item.map_err(|e| std::io::Error::other(e.to_string()))),
            boundary,
        );
        let mut entries = Vec::new();
        while let Some(field) = multipart.next_field().await? {
            let name = field.name().ok_or_else(|| Error::msg("form field has no name"))?.to_owned();
            let filename = field.file_name().map(str::to_owned);
            let content_type = field.content_type().map(ToString::to_string).unwrap_or_else(|| "text/plain".into());
            let bytes = field.bytes().await?;
            let value = match filename {
                Some(filename) => FormValue::File { filename, blob: Blob { bytes, content_type } },
                None => FormValue::Text(String::from_utf8_lossy(&bytes).into()),
            };
            entries.push(FormEntry { name, value });
        }
        Ok(Self(entries))
    }
}

impl From<FormData> for Body {
    fn from(form: FormData) -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let boundary = format!("----kardia-form-{}", SEQUENCE.fetch_add(1, Ordering::Relaxed));
        let mut chunks = Vec::new();
        for entry in form.0 {
            let name = quoted(&entry.name);
            let prefix = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"");
            match entry.value {
                FormValue::Text(value) => {
                    chunks.push(Bytes::from(format!("{prefix}\r\n\r\n{}\r\n", normalize_lines(&value))));
                }
                FormValue::File { filename, blob } => {
                    let content_type =
                        blob_type(&blob.content_type).unwrap_or_else(|| "application/octet-stream".into());
                    chunks.push(Bytes::from(format!(
                        "{prefix}; filename=\"{}\"\r\nContent-Type: {content_type}\r\n\r\n",
                        quoted(&filename)
                    )));
                    chunks.push(blob.bytes);
                    chunks.push(Bytes::from_static(b"\r\n"));
                }
            }
        }
        chunks.push(Bytes::from(format!("--{boundary}--\r\n")));
        let mut body = Self::stream(futures::stream::iter(chunks.into_iter().map(Ok)));
        body.1 = Some(format!("multipart/form-data; boundary={boundary}"));
        body
    }
}

fn normalize_lines(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n").replace('\n', "\r\n")
}
fn blob_type(value: &str) -> Option<String> {
    (!value.is_empty() && value.bytes().all(|b| (0x20..=0x7e).contains(&b))).then(|| value.to_ascii_lowercase())
}
fn quoted(value: &str) -> String {
    normalize_lines(value).replace('\r', "%0D").replace('\n', "%0A").replace('"', "%22")
}

pub(crate) fn text(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned()
}
