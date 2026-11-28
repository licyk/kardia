use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use bytes::{Bytes, BytesMut};
use futures::{
    FutureExt, StreamExt, TryStreamExt,
    stream::{self, BoxStream},
};
use kardia_core::{Disposer, EffectHandle, Error, Result, Svc};
use parking_lot::Mutex;
use tokio::sync::mpsc;
use url::Url;

use futures::future::BoxFuture;

use crate::{Fs, FsCode, FsError};

pub type FileStream = BoxStream<'static, Result<Bytes>>;

/// The part of `bytes` that `start..end` selects, with the semantics of
/// `Blob.slice()`: a negative bound counts from the end, and both are clamped to
/// the size.
pub fn slice(bytes: &Bytes, start: i64, end: Option<i64>, size: i64) -> Bytes {
    let resolve = |value: i64| -> i64 {
        let value = if value < 0 { size + value } else { value };
        value.clamp(0, size)
    };
    let start = resolve(start);
    let end = resolve(end.unwrap_or(size)).max(start);
    bytes.slice(start as usize..end as usize)
}

/// Providers resolve their own dependencies at their definition site.
pub trait FsProvider: Send + Sync + 'static {
    /// What this provider can do beyond reading. An operation it does not
    /// declare fails with `NotSupported` before the provider is asked.
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    fn read(&self, url: Url) -> FileStream {
        stream::once(async move {
            Err(FsError::new(FsCode::NotSupported, Some(url), "provider does not support reading").into())
        })
        .boxed()
    }

    fn write(&self, url: Url, _data: Vec<u8>) -> BoxFuture<'static, Result<()>> {
        let _ = url;
        unsupported("writing")
    }

    fn rename(&self, from: Url, _to: Url) -> BoxFuture<'static, Result<()>> {
        let _ = from;
        unsupported("renaming")
    }

    fn remove(&self, url: Url) -> BoxFuture<'static, Result<()>> {
        let _ = url;
        unsupported("removing")
    }

    /// Removes an entry, with the entries below a directory when `recursive`.
    fn remove_with(&self, url: Url, _options: RemoveOptions) -> BoxFuture<'static, Result<()>> {
        self.remove(url)
    }

    fn stat(&self, url: Url) -> BoxFuture<'static, Result<Stats>> {
        let _ = url;
        async move { Err(FsError::new(FsCode::NotSupported, None, "provider does not support stat").into()) }.boxed()
    }

    /// Reads a range of the content, or all of it, and checks `if_match` first.
    /// A provider with `read_range` implements this; the default reads the whole
    /// file and slices it, which is correct but not efficient.
    fn read_range(&self, url: Url, options: ReadOptions) -> FileStream {
        let source = self.read(url.clone());
        if options.start == 0 && options.end.is_none() && options.if_match.is_none() {
            return source;
        }
        let (start, end) = (options.start, options.end);
        stream::once(async move {
            let bytes = source
                .try_fold(BytesMut::new(), |mut all, chunk| async move {
                    all.extend_from_slice(&chunk);
                    Ok(all)
                })
                .await?;
            let size = bytes.len() as i64;
            let slice = crate::file::slice(&bytes.freeze(), start, end, size);
            Ok::<_, Error>(stream::iter([Ok(slice)]))
        })
        .try_flatten()
        .boxed()
    }

    /// Writes `data`. A provider that honours `exclusive`, `mode`, `atomic` or
    /// `if_match` overrides this and declares the capability; the default writes
    /// and reports no version.
    fn write_with(
        &self,
        url: Url,
        data: Vec<u8>,
        _options: WriteOptions,
    ) -> BoxFuture<'static, Result<Option<String>>> {
        let future = self.write(url, data);
        async move {
            future.await?;
            Ok(None)
        }
        .boxed()
    }
    fn mkdir(&self, url: Url, _options: MkdirOptions) -> BoxFuture<'static, Result<()>> {
        let _ = url;
        unsupported("creating a directory")
    }

    fn copy(&self, from: Url, to: Url, _options: CopyOptions) -> BoxFuture<'static, Result<()>> {
        let _ = (from, to);
        unsupported("copying")
    }

    /// The location with every symbolic link resolved.
    fn realpath(&self, url: Url) -> BoxFuture<'static, Result<Url>> {
        async move { Err(FsError::new(FsCode::NotSupported, Some(url), "provider does not support realpath").into()) }
            .boxed()
    }

    fn symlink(&self, _target: String, url: Url) -> BoxFuture<'static, Result<()>> {
        let _ = url;
        unsupported("creating a symbolic link")
    }

    /// The target of a symbolic link, as stored.
    fn readlink(&self, url: Url) -> BoxFuture<'static, Result<String>> {
        async move { Err(FsError::new(FsCode::NotSupported, Some(url), "provider does not support readlink").into()) }
            .boxed()
    }

    /// The entries of a directory, in an unspecified order.
    fn readdir(&self, url: Url) -> BoxFuture<'static, Result<Vec<DirEntry>>> {
        async move { Err(FsError::new(FsCode::NotSupported, Some(url), "provider does not support readdir").into()) }
            .boxed()
    }
}

fn unsupported(operation: &'static str) -> BoxFuture<'static, Result<()>> {
    async move { Err(FsError::new(FsCode::NotSupported, None, format!("provider does not support {operation}")).into()) }
        .boxed()
}

/// The operations a provider implements beyond `read`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub write: bool,
    pub rename: bool,
    pub remove: bool,
    pub stat: bool,
    pub mkdir: bool,
    pub copy: bool,
    /// `copy` of a directory with every entry below it.
    pub copy_recursive: bool,
    pub realpath: bool,
    pub symlink: bool,
    pub readdir: bool,
    /// A symbolic link reads as an `EntryType::Symlink` in `stat` and a listing.
    pub links: bool,
    /// `read` resolves a range and checks `if_match`.
    pub read_range: bool,
    /// A write is atomic, so a reader sees the previous or the new content.
    pub atomic_write: bool,
}

/// The type of an entry. A symbolic link is resolved by `stat`, so only the
/// three names a caller acts on are reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryType {
    File,
    Directory,
    Symlink,
    Other,
}

/// The metadata of an entry.
#[derive(Clone, Debug)]
pub struct Stats {
    pub kind: EntryType,
    /// The length of the content in bytes for a file.
    pub size: u64,
    pub mtime: Option<std::time::SystemTime>,
    /// The permission bits, as in the lower 12 bits of a POSIX mode.
    pub mode: Option<u32>,
    /// The version of a file, which a provider with `conditional.stat` reports
    /// for every file: an opaque string compared for equality with other
    /// versions of the same location.
    pub version: Option<String>,
}

/// The options of a read, as a provider receives them.
#[derive(Clone, Debug, Default)]
pub struct ReadOptions {
    /// The first byte of the range; `0` by default. A negative value counts from
    /// the end, as `Blob.slice()` does.
    pub start: i64,
    /// The end of the range, exclusive; the size by default.
    pub end: Option<i64>,
    /// The version of the whole file: the read fails with `VersionMismatch`
    /// unless the file has it.
    pub if_match: Option<String>,
}

/// The options of a write.
#[derive(Clone, Debug, Default)]
pub struct WriteOptions {
    /// An existing file fails with `AlreadyExists`, checked when it is created.
    pub exclusive: bool,
    /// The permission bits of the file, where the provider has them.
    pub mode: Option<u32>,
    /// A reader observes the previous or the new content, and a failed write
    /// leaves the previous content.
    pub atomic: bool,
    /// The version of the file to be replaced: the write fails with
    /// `VersionMismatch` unless the destination is a file of this version.
    pub if_match: Option<String>,
}

/// The options of a copy.
#[derive(Clone, Debug, Default)]
pub struct CopyOptions {
    /// A directory is copied with every entry below it.
    pub recursive: bool,
    /// What a symbolic link points to is copied instead of the link.
    pub dereference: bool,
    pub exclusive: bool,
    pub atomic: bool,
}

/// The options of a directory creation.
#[derive(Clone, Copy, Debug, Default)]
pub struct MkdirOptions {
    /// Missing parents are created, and an existing directory is not an error.
    pub recursive: bool,
}

/// The options of a removal.
#[derive(Clone, Copy, Debug, Default)]
pub struct RemoveOptions {
    /// A directory is removed with every entry below it.
    pub recursive: bool,
    /// A missing location is not an error.
    pub force: bool,
}

/// An entry of a directory, as a provider lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    /// The type of the entry itself, not of the target of a link.
    pub kind: EntryType,
}

#[derive(Clone)]
pub struct FileRef {
    pub url: Url,
    pub(crate) fs: Svc<Fs>,
    /// The ranges this reference reads, relative to the file; absent for a whole
    /// file.
    pub(crate) slices: Vec<(i64, Option<i64>)>,
}
impl FileRef {
    /// A reference to a range of the content, with the semantics of
    /// `Blob.slice()`: a negative bound counts from the end, and the range is
    /// resolved at each read.
    pub fn slice(&self, start: i64, end: Option<i64>) -> Self {
        let mut slices = self.slices.clone();
        slices.push((start, end));
        Self { url: self.url.clone(), fs: self.fs.clone(), slices }
    }

    /// The `start` and `end` this reference selects, resolved against `size`, or
    /// `None` for the whole file.
    pub(crate) fn resolve_slices(&self, size: i64) -> (i64, Option<i64>) {
        let mut start = 0;
        let mut end: Option<i64> = None;
        for (next_start, next_end) in &self.slices {
            let length = end.map(|end| end - start).unwrap_or(size);
            let offset = if *next_start < 0 { length + next_start } else { *next_start };
            let offset = offset.clamp(0, length);
            let limit = match next_end {
                Some(value) => {
                    let value = if *value < 0 { length + value } else { *value };
                    value.clamp(offset, length)
                }
                None => length,
            };
            start += offset;
            end = Some(start + (limit - offset));
        }
        (start, end)
    }

    pub fn stream(&self) -> Result<FileStream> {
        let mount = self.fs.provider_for(&self.url)?;
        let source = if self.slices.is_empty() {
            catch_unwind(AssertUnwindSafe(|| mount.provider.read(self.url.clone()))).map_err(Error::from_panic)?
        } else {
            // A slice resolves against the current size, which needs one metadata
            // query before the ranged read.
            let provider = mount.provider.clone();
            let (url, reference) = (self.url.clone(), self.clone());
            stream::once(async move {
                let stats = provider.stat(url.clone()).await?;
                let (start, end) = reference.resolve_slices(stats.size as i64);
                Ok::<FileStream, Error>(provider.read_range(url, ReadOptions { start, end, if_match: None }))
            })
            .try_flatten()
            .boxed()
        };
        let mut source = AssertUnwindSafe(source).catch_unwind();
        let (send, receive) = mpsc::channel(1);
        let error = Arc::new(Mutex::new(None));
        let failed = error.clone();
        let mut stopped = mount.stop.subscribe();
        let url = self.url.clone();
        mount.readers.send_modify(|count| *count += 1);
        let active = ActiveReader(mount.readers.clone());
        let pump = async move {
            let _active = active;
            let result:Result<()>=async {
                loop {
                    tokio::select! {
                        _=stopped.wait_for(|stopped| *stopped).map(|_| ())=>return Err(FsError::new(FsCode::NoProvider,Some(url.clone()),"file provider was unmounted").into()),
                        permit=send.reserve()=>{
                            let Ok(permit)=permit else {return Ok(())};
                            let next=tokio::select! {
                                _=stopped.wait_for(|stopped| *stopped).map(|_| ())=>return Err(FsError::new(FsCode::NoProvider,Some(url.clone()),"file provider was unmounted").into()),
                                chunk=source.next()=>chunk,
                            };
                            match next {Some(chunk)=>permit.send(chunk.map_err(Error::from_panic)??),None=>return Ok(())}
                        }
                    }
                }
            }.await;
            if let Err(error) = result {
                *failed.lock() = Some(error);
            }
            drop(source);
        };
        let effect = self.fs.caller().effect_sync("fs file stream", |scope| {
            let task = tokio::spawn(pump);
            scope.defer(Disposer::future(move || async move {
                task.abort();
                let _ = task.await;
            }));
            Ok(())
        })?;
        let reader = Reader { receive, error, _effect: StreamEffect(effect), ended: false };
        Ok(stream::unfold(reader, |mut reader| async move {
            if reader.ended {
                return None;
            }
            match reader.receive.recv().await {
                Some(bytes) => Some((Ok(bytes), reader)),
                None => {
                    reader.ended = true;
                    let error = reader.error.lock().take();
                    error.map(|error| (Err(error), reader))
                }
            }
        })
        .boxed())
    }
    /// Replaces the content, creating the file if it is missing.
    pub async fn write(&self, data: impl Into<Vec<u8>>, options: WriteOptions) -> Result<Option<String>> {
        if !self.slices.is_empty() {
            return Err(FsError::new(FsCode::InvalidPath, Some(self.url.clone()), "cannot write a slice").into());
        }
        let mount = self.fs.provider_for(&self.url)?;
        let provider = mount.provider.clone();
        if options.if_match.is_some() && !provider.capabilities().stat {
            return Err(FsError::new(
                FsCode::NotSupported,
                Some(self.url.clone()),
                "the provider cannot check a version",
            )
            .into());
        }
        if options.atomic && !provider.capabilities().atomic_write {
            return Err(FsError::new(
                FsCode::NotSupported,
                Some(self.url.clone()),
                "the provider cannot write atomically",
            )
            .into());
        }
        provider.write_with(self.url.clone(), data.into(), options).await
    }

    pub async fn bytes(&self) -> Result<Bytes> {
        let mut stream = self.stream()?;
        let mut bytes = BytesMut::new();
        while let Some(chunk) = stream.next().await {
            bytes.extend_from_slice(&chunk?);
        }
        Ok(bytes.freeze())
    }
    pub async fn text(&self) -> Result<String> {
        let bytes = self.bytes().await?;
        let text = String::from_utf8_lossy(&bytes);
        Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned())
    }
    pub async fn json(&self) -> Result<serde_json::Value> {
        Ok(serde_json::from_str(&self.text().await?)?)
    }
}
struct StreamEffect(EffectHandle);
impl Drop for StreamEffect {
    fn drop(&mut self) {
        drop(self.0.dispose());
    }
}
struct ActiveReader(tokio::sync::watch::Sender<usize>);
impl Drop for ActiveReader {
    fn drop(&mut self) {
        self.0.send_modify(|count| *count -= 1);
    }
}
struct Reader {
    receive: mpsc::Receiver<Bytes>,
    error: Arc<Mutex<Option<Error>>>,
    _effect: StreamEffect,
    ended: bool,
}
