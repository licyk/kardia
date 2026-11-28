use kardia_core::{Error, Result};
use url::Url;

/// The characters that a URL path segment cannot carry literally: `%`, `?` and
/// `#` would end or escape the segment, and `\` reads as a separator. The parser
/// also drops controls and trailing spaces, and tabs and newlines anywhere, so
/// those are encoded as well.
fn encode_name(name: &str) -> String {
    let mut encoded = String::with_capacity(name.len());
    for ch in name.chars() {
        let literal = ch.is_ascii() && (ch > ' ' && !matches!(ch, '%' | '?' | '#' | '\\' | '\x7f'));
        if literal || !ch.is_ascii() {
            encoded.push(ch);
        } else {
            encoded.push_str(&format!("%{:02X}", ch as u32));
        }
    }
    encoded
}

/// A string whose prefix is a scheme of at least two characters is an absolute
/// URL; a single-letter scheme would read as a Windows drive.
fn absolute(path: &str) -> bool {
    let Some((scheme, _)) = path.split_once(':') else { return false };
    scheme.len() >= 2
        && scheme.chars().next().is_some_and(|ch| ch.is_ascii_alphabetic())
        && scheme.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
}

/// The working directory as a directory URL, which a relative path resolves
/// against.
pub(crate) fn working_directory() -> Result<Url> {
    let directory = std::env::current_dir()?;
    Url::from_directory_path(&directory)
        .map_err(|_| Error::msg(format!("cannot represent {} as a file URL", directory.display())))
}

/// Whether a path starts with a drive letter and a separator, such as `C:\app`.
fn drive(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_alphabetic)
        && bytes.get(1) == Some(&b':')
        && matches!(bytes.get(2), Some(b'/' | b'\\'))
}

/// Resolves a location against a base, as `@cordisjs/fs` describes a `Path`: a
/// string with a scheme of at least two characters is an absolute URL, and every
/// other string is a path in which `/` and `\` separate names, `.` and `..` name
/// a directory, a leading separator starts at the root of the base, two leading
/// separators followed by a host start a network path, a drive letter starts an
/// absolute path, and everything else is literal — `%`, `?`, `#` and spaces
/// included.
pub(crate) fn resolve(base: Option<&Url>, path: &str) -> Result<Url> {
    if absolute(path) {
        return Url::parse(path).map_err(|error| Error::msg(format!("invalid path {path:?}: {error}")));
    }
    let base = match base {
        Some(base) => directory(base.clone())?,
        None => working_directory()?,
    };
    let (prefix, rest) = network(path).map_or_else(
        || {
            if drive(path) {
                // A drive is local, also below a base URL with a host.
                (if base.scheme() == "file" { "///".to_owned() } else { "/".to_owned() }, path.to_owned())
            } else if path.starts_with('/') || path.starts_with('\\') {
                ("/".to_owned(), path.to_owned())
            } else {
                ("./".to_owned(), path.to_owned())
            }
        },
        |(host, rest)| (format!("//{host}/"), rest),
    );
    let names: Vec<&str> = rest.split(['/', '\\']).collect();
    let mut segments = Vec::with_capacity(names.len());
    for name in &names {
        match *name {
            "" | "." => {}
            // The URL parser resolves `..` itself, which keeps a base under a
            // network prefix and a drive consistent.
            ".." => segments.push("..".to_owned()),
            name => segments.push(encode_name(name)),
        }
    }
    // A trailing separator, `.` or `..` names a directory.
    let last = names.last().copied().unwrap_or_default();
    let tail = if segments.is_empty() {
        String::new()
    } else if matches!(last, "" | "." | "..") {
        format!("{}/", segments.join("/"))
    } else {
        segments.join("/")
    };
    let reference = format!("{prefix}{tail}");
    base.join(&reference).map_err(|error| Error::msg(format!("invalid path {path:?}: {error}")))
}

/// A network path, `\\host\share`: its host and what follows it.
fn network(path: &str) -> Option<(String, String)> {
    let mut chars = path.char_indices();
    let (_, first) = chars.next()?;
    let (_, second) = chars.next()?;
    if !matches!(first, '/' | '\\') || !matches!(second, '/' | '\\') {
        return None;
    }
    let rest = &path[2..];
    let start = rest.find(['/', '\\']).unwrap_or(rest.len());
    let host = rest[..start].to_owned();
    if host.is_empty() {
        return None;
    }
    Some((host, rest[start..].to_owned()))
}

/// A base URL names a directory: its path ends with `/`, and it has no query or
/// fragment.
pub(crate) fn directory(mut url: Url) -> Result<Url> {
    // A scheme the URL parser does not know is still a base as long as its path is
    // not opaque, such as `mem:bucket/`. A relative reference cannot resolve
    // against an opaque path.
    if url.cannot_be_a_base() {
        return Err(Error::msg("baseUrl must be a hierarchical directory URL"));
    }
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

pub(crate) fn deserialize_base<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Url>, D::Error> {
    use serde::Deserialize;
    Option::<String>::deserialize(deserializer)?
        .map(|path| resolve(None, &path).and_then(directory).map_err(serde::de::Error::custom))
        .transpose()
}
