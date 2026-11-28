use url::Url;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FsCode {
    NoProvider,
    NotSupported,
    NotFound,
    AlreadyExists,
    NotADirectory,
    IsADirectory,
    DirectoryNotEmpty,
    PermissionDenied,
    Busy,
    CrossesDevices,
    InvalidPath,
    VersionMismatch,
    NotALink,
    Unknown,
}

#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct FsError {
    pub code: FsCode,
    pub url: Option<Url>,
    pub message: String,
}
impl FsError {
    pub fn new(code: FsCode, url: Option<Url>, message: impl Into<String>) -> Self {
        Self { code, url, message: message.into() }
    }
    pub(crate) fn io(error: std::io::Error, url: &Url) -> Self {
        use std::io::ErrorKind as Io;
        let code = match error.kind() {
            Io::NotFound => FsCode::NotFound,
            Io::PermissionDenied | Io::ReadOnlyFilesystem => FsCode::PermissionDenied,
            Io::NotADirectory => FsCode::NotADirectory,
            Io::IsADirectory => FsCode::IsADirectory,
            Io::Unsupported => FsCode::NotSupported,
            Io::InvalidFilename => FsCode::InvalidPath,
            Io::ResourceBusy => FsCode::Busy,
            Io::AlreadyExists => FsCode::AlreadyExists,
            Io::DirectoryNotEmpty => FsCode::DirectoryNotEmpty,
            Io::CrossesDevices => FsCode::CrossesDevices,
            _ => FsCode::Unknown,
        };
        Self::new(code, Some(url.clone()), error.to_string())
    }
}
