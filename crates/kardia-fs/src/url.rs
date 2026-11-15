use kardia_core::{Error, Result};
use url::Url;

pub(crate) fn resolve(base: Option<&Url>, path: &str) -> Result<Url> {
    #[cfg(windows)]
    if path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && path.as_bytes().get(1) == Some(&b':')
        && matches!(path.as_bytes().get(2), Some(b'/' | b'\\'))
    {
        return Url::from_file_path(path).map_err(|_| Error::msg("cannot represent path as a file URL"));
    }
    Ok(match base {
        Some(base) => base.join(path)?,
        None => Url::parse(path)?,
    })
}

pub(crate) fn directory(mut url: Url) -> Result<Url> {
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
        .map(|path| resolve(None, &path).map_err(serde::de::Error::custom))
        .transpose()
}
