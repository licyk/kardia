//! `data:` URLs, which carry their content in the URL itself.

use kardia_core::{Error, Result};

use crate::Body;

/// The content of a `data:` URL.
pub(crate) struct DataUrl {
    pub(crate) content_type: String,
    pub(crate) body: Vec<u8>,
}

/// Parses `data:[<media-type>][;base64],<data>`, as the WHATWG HTML Standard
/// defines it: the media type defaults to `text/plain;charset=US-ASCII`, and a
/// `;base64` suffix decodes the rest as base64. Everything after the first comma
/// is data, so a comma inside it stays part of the content.
pub(crate) fn parse(url: &url::Url) -> Result<DataUrl> {
    let rest = url.as_str().strip_prefix("data:").ok_or_else(|| Error::msg("not a data URL"))?;
    let Some((meta, data)) = rest.split_once(',') else {
        return Err(Error::msg("a data URL needs a comma between the media type and the data"));
    };
    let (media_type, base64) = match meta.strip_suffix(";base64") {
        Some(media_type) => (media_type, true),
        None => (meta, false),
    };
    let content_type =
        if media_type.is_empty() { "text/plain;charset=US-ASCII".to_owned() } else { percent_decode(media_type) };
    let body = if base64 {
        decode_base64(data)?
    } else {
        // The data of a URL that is not base64 is percent-encoded UTF-8.
        percent_decode(data).into_bytes()
    };
    Ok(DataUrl { content_type, body })
}

fn percent_decode(value: &str) -> String {
    // The URL parser has already accepted the escapes, so a sequence that does not
    // decode is kept as written rather than rejected.
    let mut bytes = Vec::with_capacity(value.len());
    let mut rest = value.as_bytes();
    while let Some((index, byte)) =
        rest.iter().enumerate().find(|(_, byte)| **byte == b'%').map(|(index, byte)| (index, *byte))
    {
        bytes.extend_from_slice(&rest[..index]);
        let hex = rest.get(index + 1..index + 3).and_then(|pair| std::str::from_utf8(pair).ok());
        match hex.and_then(|pair| u8::from_str_radix(pair, 16).ok()) {
            Some(decoded) => {
                bytes.push(decoded);
                rest = &rest[index + 3..];
            }
            None => {
                bytes.push(byte);
                rest = &rest[index + 1..];
            }
        }
    }
    bytes.extend_from_slice(rest);
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Decodes standard base64, rejecting anything that is not one.
fn decode_base64(data: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(data.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in data.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            // Padding and the whitespace a line-wrapped value may carry.
            b'=' | b'\r' | b'\n' | b' ' | b'\t' => continue,
            _ => return Err(Error::msg("a data URL with base64 carries a character outside the alphabet")),
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Ok(bytes)
}

/// The response of a `data:` URL: `200` with the declared type, or `405` for a
/// method other than GET.
pub(crate) fn response(url: &url::Url, method: &http::Method) -> Result<crate::HttpResponse> {
    if method != http::Method::GET {
        return Ok(crate::HttpResponse {
            url: url.clone(),
            status: http::StatusCode::METHOD_NOT_ALLOWED,
            status_text: "Method Not Allowed".into(),
            headers: http::HeaderMap::new(),
            body: Body::stream(futures::stream::empty()),
        });
    }
    let parsed = parse(url)?;
    let mut headers = http::HeaderMap::new();
    if !parsed.content_type.is_empty() {
        headers.insert(http::header::CONTENT_TYPE, http::HeaderValue::from_str(&parsed.content_type)?);
    }
    headers.insert(http::header::CONTENT_LENGTH, http::HeaderValue::from(parsed.body.len()));
    Ok(crate::HttpResponse {
        url: url.clone(),
        status: http::StatusCode::OK,
        status_text: "OK".into(),
        headers,
        body: Body::from(parsed.body),
    })
}

#[cfg(test)]
mod tests {
    use super::{decode_base64, parse};

    fn at(value: &str) -> url::Url {
        url::Url::parse(value).expect("a URL")
    }

    #[test]
    fn an_empty_media_type_defaults_to_plain_text() {
        let parsed = parse(&at("data:,hello")).unwrap();
        assert_eq!(parsed.content_type, "text/plain;charset=US-ASCII");
        assert_eq!(parsed.body, b"hello");
    }

    #[test]
    fn a_media_type_is_kept_as_declared() {
        let parsed = parse(&at("data:application/json,%7B%7D")).unwrap();
        assert_eq!(parsed.content_type, "application/json");
        assert_eq!(parsed.body, b"{}");
    }

    #[test]
    fn base64_carries_binary_content() {
        let parsed = parse(&at("data:application/octet-stream;base64,AAEC")).unwrap();
        assert_eq!(parsed.body, vec![0, 1, 2]);
    }

    #[test]
    fn a_comma_inside_the_data_stays_in_the_content() {
        let parsed = parse(&at("data:text/plain,a,b")).unwrap();
        assert_eq!(parsed.body, b"a,b");
    }

    #[test]
    fn padding_and_line_breaks_are_ignored() {
        assert_eq!(decode_base64("AAEC\n").unwrap(), vec![0, 1, 2]);
        assert_eq!(decode_base64("YQ==").unwrap(), b"a");
    }

    #[test]
    fn a_character_outside_the_alphabet_is_rejected() {
        assert!(decode_base64("a$b").is_err());
    }

    #[test]
    fn a_url_without_a_comma_is_rejected() {
        assert!(parse(&at("data:text/plain")).is_err());
    }
}
