use kardia_core::{Result, Svc};
use url::Url;

use crate::{EntryType, FileRef, Fs, FsCode, FsError, FsExt};

/// A reference to a directory. Creating it performs no I/O. Its URL ends with
/// `/` and is the base of the paths given to `dir()` and `file()`.
#[derive(Clone)]
pub struct DirRef {
    pub url: Url,
    pub(crate) fs: Svc<Fs>,
}

impl DirRef {
    pub(crate) fn new(fs: Svc<Fs>, mut url: Url) -> Result<Self> {
        if url.cannot_be_a_base() {
            return Err(FsError::new(FsCode::InvalidPath, Some(url), "expected a hierarchical path").into());
        }
        if !url.path().ends_with('/') {
            url.set_path(&format!("{}/", url.path()));
        }
        Ok(Self { url, fs })
    }

    /// A reference to a directory, resolved against this one.
    pub fn dir(&self, path: &str) -> Result<Self> {
        let url = self.fs.resolve_at(&self.url, path)?;
        Self::new(self.fs.clone(), url)
    }

    /// A reference to a file, resolved against this one.
    pub fn file(&self, path: &str) -> Result<FileRef> {
        Ok(FileRef { url: self.fs.resolve_at(&self.url, path)?, fs: self.fs.clone(), slices: Vec::new() })
    }

    /// The entries of the directory, in an unspecified order.
    pub async fn list(&self) -> Result<Vec<DirEntryRef>> {
        let entries = self.fs.readdir(self.url.as_str()).await?;
        Ok(entries
            .into_iter()
            .map(|entry| {
                let url = self.fs.resolve_at(&self.url, &entry.name).unwrap_or_else(|_| self.url.clone());
                DirEntryRef { url, kind: entry.kind, fs: self.fs.clone(), name: entry.name }
            })
            .collect())
    }
}

/// An entry of a directory as listed: a reference to it with the type it had at
/// the time of the listing.
#[derive(Clone)]
pub struct DirEntryRef {
    pub url: Url,
    pub kind: EntryType,
    pub name: String,
    pub(crate) fs: Svc<Fs>,
}

impl DirEntryRef {
    /// A file reference to the entry, whether or not it is a file.
    pub fn file(&self) -> FileRef {
        FileRef { url: self.url.clone(), fs: self.fs.clone(), slices: Vec::new() }
    }

    /// A directory reference to the entry, which requires a hierarchical URL.
    pub fn dir(&self) -> Result<DirRef> {
        DirRef::new(self.fs.clone(), self.url.clone())
    }
}
