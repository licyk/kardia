use crate::{
    generated::{archive_mime, confident, imprecise},
    probe::Probe,
};
use kardia_core::{Error, Result};
use std::io::Read;

pub(super) async fn detect(p: &mut Probe) -> Result<Option<&'static str>> {
    let position = p.position;
    let result = Box::pin(confident(p)).await?;
    if result.is_some() || p.position != position {
        return Ok(result);
    }
    imprecise(p).await
}
pub(super) fn check(bytes: &[u8], signature: &[u8], offset: usize, mask: &[u8]) -> bool {
    signature
        .iter()
        .enumerate()
        .all(|(i, &v)| bytes.get(offset + i).is_some_and(|&b| if mask.is_empty() { b == v } else { b & mask[i] == v }))
}
pub(super) fn u16n(b: &[u8], big: bool) -> u16 {
    let a = [b[0], b[1]];
    if big { u16::from_be_bytes(a) } else { u16::from_le_bytes(a) }
}
pub(super) fn u32n(b: &[u8], big: bool) -> u32 {
    let a = [b[0], b[1], b[2], b[3]];
    if big { u32::from_be_bytes(a) } else { u32::from_le_bytes(a) }
}
pub(super) fn latin(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}
pub(super) fn truthy(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(v) => *v,
        serde_json::Value::Number(v) => v.as_f64() != Some(0.0),
        serde_json::Value::String(v) => !v.is_empty(),
        _ => true,
    }
}
pub(super) fn tar(b: &[u8]) -> bool {
    let field = String::from_utf8_lossy(&b[148..154]);
    let digits = field
        .split('\0')
        .next()
        .unwrap_or_default()
        .trim()
        .chars()
        .take_while(|c| matches!(c, '0'..='7'))
        .collect::<String>();
    let Ok(sum) = usize::from_str_radix(&digits, 8) else { return false };
    sum == 256 + b[..148].iter().chain(&b[156..512]).map(|&b| usize::from(b)).sum::<usize>()
}
pub(super) fn brand(b: &[u8]) -> &'static str {
    let name = latin(b).replacen('\0', " ", 1);
    match name.trim() {
        "avif" | "avis" => "image/avif",
        "mif1" => "image/heif",
        "msf1" => "image/heif-sequence",
        "heic" | "heix" => "image/heic",
        "hevc" | "hevx" => "image/heic-sequence",
        "qt" => "video/quicktime",
        "M4V" | "M4VH" | "M4VP" => "video/x-m4v",
        "M4B" | "F4A" | "F4B" => "audio/mp4",
        "M4A" => "audio/x-m4a",
        "crx" => "image/x-canon-cr3",
        v if v.starts_with("3g2") => "video/3gpp2",
        v if v.starts_with("3g") => "video/3gpp",
        _ => "video/mp4",
    }
}
pub(super) async fn ogg(p: &mut Probe) -> Result<Option<&'static str>> {
    p.skip(28).await?;
    let b = p.read(8).await?;
    Ok(Some(if b.starts_with(b"OpusHead") {
        "audio/ogg; codecs=opus"
    } else if b.starts_with(b"\x80theora") || b.starts_with(b"\x01video\0") {
        "video/ogg"
    } else if b.starts_with(b"\x7fFLAC") || b.starts_with(b"Speex  ") || b.starts_with(b"\x01vorbis") {
        "audio/ogg"
    } else {
        "application/ogg"
    }))
}
pub(super) async fn pdf(p: &mut Probe) -> Result<Option<&'static str>> {
    let skipped = p.skip(1350).await;
    if let Err(error) = &skipped {
        if !error.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::UnexpectedEof) {
            return Err(error.clone());
        }
    }
    if skipped.is_ok() {
        let mut b = vec![0; 10 * 1024 * 1024];
        let size = p.peek(&mut b).await?;
        if b[..size].windows(13).any(|v| v == b"AIPrivateData") {
            return Ok(Some("application/postscript"));
        }
    }
    Ok(Some("application/pdf"))
}
pub(super) async fn png(p: &mut Probe) -> Result<Option<&'static str>> {
    p.skip(8).await?;
    loop {
        let header = p.read(8).await?;
        let length = u32n(&header, true);
        if length > i32::MAX as u32 {
            return Ok(None);
        }
        match &header[4..8] {
            b"IDAT" => return Ok(Some("image/png")),
            b"acTL" => return Ok(Some("image/apng")),
            _ => p.skip(length as usize + 4).await?,
        }
    }
}
pub(super) async fn tiff(p: &mut Probe, b: &[u8], big: bool) -> Result<Option<&'static str>> {
    let version = u16n(&b[2..4], big);
    let offset = u32n(&b[4..8], big) as usize;
    if version == 43 {
        return Ok(Some("image/tiff"));
    }
    if version != 42 {
        return Ok(None);
    }
    if offset >= 6 {
        if &b[8..10] == b"CR" {
            return Ok(Some("image/x-canon-cr2"));
        }
        if offset >= 8 {
            let pair = (u16n(&b[8..10], big), u16n(&b[10..12], big));
            if matches!(pair, (0x1c, 0xfe) | (0x1f, 0x0b)) {
                return Ok(Some("image/x-nikon-nef"));
            }
        }
    }
    p.skip(offset).await?;
    let count = u16n(&p.read(2).await?, big);
    for _ in 0..count {
        let tag = u16n(&p.read(2).await?, big);
        p.skip(10).await?;
        match tag {
            50341 => return Ok(Some("image/x-sony-arw")),
            50706 => return Ok(Some("image/x-adobe-dng")),
            _ => {}
        }
    }
    Ok(Some("image/tiff"))
}
async fn ebml_field(p: &mut Probe, length: bool) -> Result<u64> {
    let mut first = [0];
    if p.peek(&mut first).await? == 0 {
        return Err(Error::msg("end of EBML header"));
    }
    let count = first[0].leading_zeros() as usize + 1;
    let mut bytes = p.read(count).await?;
    if length {
        bytes[0] ^= 0x80u8.checked_shr((count - 1) as u32).unwrap_or(0);
    }
    let bytes = if length { &bytes[count.saturating_sub(6)..] } else { &bytes[..] };
    Ok(bytes.iter().fold(0, |v, &b| (v << 8) | u64::from(b)))
}
async fn ebml_element(p: &mut Probe) -> Result<(u64, u64)> {
    Ok((ebml_field(p, false).await?, ebml_field(p, true).await?))
}
pub(super) async fn ebml(p: &mut Probe) -> Result<Option<&'static str>> {
    let (_, count) = ebml_element(p).await?;
    for _ in 0..count {
        let (id, length) = ebml_element(p).await?;
        if id == 0x4282 {
            let b = p.read(length as usize).await?;
            let name = String::from_utf8_lossy(&b);
            return Ok(match name.split('\0').next().unwrap_or_default() {
                "webm" => Some("video/webm"),
                "matroska" => Some("video/x-matroska"),
                _ => None,
            });
        }
        p.skip(length as usize).await?;
    }
    Ok(None)
}
pub(super) async fn asf(p: &mut Probe) -> Result<Option<&'static str>> {
    p.skip(30).await?;
    loop {
        let header = p.read(24).await?;
        let size = u64::from_le_bytes(header[16..24].try_into().unwrap());
        let payload = size.checked_sub(24).ok_or_else(|| Error::msg("invalid ASF size"))?;
        if header[..16] == [0x91, 7, 0xdc, 0xb7, 0xb7, 0xa9, 0xcf, 0x11, 0x8e, 0xe6, 0, 0xc0, 0xc, 0x20, 0x53, 0x65] {
            let id = p.read(16).await?;
            if id == [0x40, 0x9e, 0x69, 0xf8, 0x4d, 0x5b, 0xcf, 0x11, 0xa8, 0xfd, 0, 0x80, 0x5f, 0x5c, 0x44, 0x2b] {
                return Ok(Some("audio/x-ms-asf"));
            }
            if id == [0xc0, 0xef, 0x19, 0xbc, 0x4d, 0x5b, 0xcf, 0x11, 0xa8, 0xfd, 0, 0x80, 0x5f, 0x5c, 0x44, 0x2b] {
                return Ok(Some("video/x-ms-asf"));
            }
            return Ok(Some("application/vnd.ms-asf"));
        }
        p.skip(payload as usize).await?;
    }
}
pub(super) async fn zip(p: &mut Probe) -> Result<Option<&'static str>> {
    loop {
        let mut signature = [0; 4];
        if p.peek(&mut signature).await? != 4 {
            return Err(Error::msg("end of ZIP header"));
        }
        if signature == [0x50, 0x4b, 1, 2] {
            return Ok(Some("application/zip"));
        }
        if signature != [0x50, 0x4b, 3, 4] {
            return Err(Error::msg("unexpected ZIP signature"));
        }
        let h = p.read(30).await?;
        let descriptor = u16n(&h[6..8], false) & 8 != 0;
        let size = u32n(&h[18..22], false) as usize;
        let name = p.read(u16n(&h[26..28], false) as usize).await?;
        let name = String::from_utf8_lossy(&name);
        let direct = match name.as_ref() {
            "META-INF/mozilla.rsa" => Some("application/x-xpinstall"),
            "META-INF/MANIFEST.MF" => Some("application/java-archive"),
            v if apk_name(v) => Some("application/vnd.android.package-archive"),
            _ => None,
        };
        let data_handler = matches!(name.as_ref(), "mimetype" | "[Content_Types].xml");
        p.skip(u16n(&h[28..30], false) as usize).await?;
        let mut data = Vec::new();
        if descriptor && size == 0 {
            loop {
                let mut buffer = vec![0; 256 * 1024];
                let size = p.peek(&mut buffer).await?;
                let index = buffer[..size].windows(4).position(|v| v == [0x50, 0x4b, 7, 8]);
                let length = index.unwrap_or(size);
                if data_handler {
                    data.extend(p.read(length).await?);
                } else {
                    p.skip(length).await?;
                }
                if index.is_some() || size < buffer.len() {
                    break;
                }
            }
        } else if data_handler {
            data = p.read(size).await?;
        } else {
            p.skip(size).await?;
        }
        if descriptor && p.read(16).await?[..4] != [0x50, 0x4b, 7, 8] {
            return Err(Error::msg("invalid ZIP descriptor"));
        }
        if let Some(mime) = direct {
            return Ok(Some(mime));
        }
        if data_handler {
            if u16n(&h[8..10], false) != 0 {
                let mut decoded = Vec::new();
                flate2::read::DeflateDecoder::new(data.as_slice()).read_to_end(&mut decoded)?;
                data = decoded;
            }
            let text = String::from_utf8_lossy(&data);
            let mime = if name == "mimetype" {
                archive_mime(text.trim())
            } else if let Some(end) = text.find(".main+xml\"") {
                let start = text[..end].rfind('"').map_or(0, |i| i + 1);
                archive_mime(&text[start..end])
            } else if text.contains("ContentType=\"application/vnd.ms-package.3dmanufacturing-3dmodel+xml\"") {
                Some("model/3mf")
            } else {
                None
            };
            return Ok(Some(mime.unwrap_or("application/zip")));
        }
    }
}
fn apk_name(name: &str) -> bool {
    name.match_indices("classes")
        .any(|(i, _)| name[i + 7..].trim_start_matches(|c: char| c.is_ascii_digit()).starts_with(".dex"))
}
