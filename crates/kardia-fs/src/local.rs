use std::sync::Arc;
use std::time::SystemTime;

use bytes::Bytes;
use futures::{FutureExt, StreamExt, TryStreamExt, stream};
use kardia_core::{Context, Inject, Plugin, Result};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use url::Url;

use crate::{FileStream, Fs, FsCode, FsError, FsExt, FsProvider};

pub struct LocalFs;
impl FsProvider for LocalFs {
    fn capabilities(&self) -> crate::Capabilities {
        crate::Capabilities {
            write: true,
            rename: true,
            remove: true,
            stat: true,
            mkdir: true,
            copy: true,
            copy_recursive: true,
            realpath: true,
            symlink: true,
            readdir: true,
            links: true,
            read_range: true,
            atomic_write: true,
        }
    }

    fn write(&self, url: Url, data: Vec<u8>) -> futures::future::BoxFuture<'static, Result<()>> {
        async move {
            let path = local_path(&url)?;
            tokio::fs::write(&path, &data).await.map_err(|error| FsError::io(error, &url))?;
            Ok(())
        }
        .boxed()
    }

    fn rename(&self, from: Url, to: Url) -> futures::future::BoxFuture<'static, Result<()>> {
        async move {
            let (from_path, to_path) = (local_path(&from)?, local_path(&to)?);
            tokio::fs::rename(&from_path, &to_path).await.map_err(|error| FsError::io(error, &from))?;
            Ok(())
        }
        .boxed()
    }

    fn remove_with(&self, url: Url, options: crate::RemoveOptions) -> futures::future::BoxFuture<'static, Result<()>> {
        async move {
            let path = local_path(&url)?;
            let metadata = match tokio::fs::symlink_metadata(&path).await {
                Ok(metadata) => metadata,
                Err(error) if options.force && error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(FsError::io(error, &url).into()),
            };
            // A link is removed itself, so its target is never touched.
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                tokio::fs::remove_file(&path).await.map_err(|error| FsError::io(error, &url))?;
            } else if options.recursive {
                tokio::fs::remove_dir_all(&path).await.map_err(|error| FsError::io(error, &url))?;
            } else {
                tokio::fs::remove_dir(&path).await.map_err(|error| FsError::io(error, &url))?;
            }
            Ok(())
        }
        .boxed()
    }

    fn remove(&self, url: Url) -> futures::future::BoxFuture<'static, Result<()>> {
        async move {
            let path = local_path(&url)?;
            let metadata = tokio::fs::symlink_metadata(&path).await.map_err(|error| FsError::io(error, &url))?;
            if metadata.is_dir() {
                tokio::fs::remove_dir(&path).await.map_err(|error| FsError::io(error, &url))?;
            } else {
                tokio::fs::remove_file(&path).await.map_err(|error| FsError::io(error, &url))?;
            }
            Ok(())
        }
        .boxed()
    }

    fn stat(&self, url: Url) -> futures::future::BoxFuture<'static, Result<crate::Stats>> {
        async move {
            let path = local_path(&url)?;
            let metadata = tokio::fs::metadata(&path).await.map_err(|error| FsError::io(error, &url))?;
            let kind = if metadata.is_dir() {
                crate::EntryType::Directory
            } else if metadata.is_file() {
                crate::EntryType::File
            } else {
                crate::EntryType::Other
            };
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                Some(metadata.permissions().mode() & 0o7777)
            };
            #[cfg(not(unix))]
            let mode = None;
            // The modification time and the size together identify the content
            // closely enough for a conditional write on a local disk.
            let version = metadata.modified().ok().map(|time| {
                let nanos =
                    time.duration_since(std::time::UNIX_EPOCH).map(|since| since.as_nanos()).unwrap_or_default();
                format!("{nanos}-{}", metadata.len())
            });
            Ok(crate::Stats { kind, size: metadata.len(), mtime: metadata.modified().ok(), mode, version })
        }
        .boxed()
    }

    fn read(&self, url: Url) -> FileStream {
        stream::once(async move {
            let path = local_path(&url)?;
            if url.path().ends_with('/') || tokio::fs::metadata(&path).await.map_err(|e| FsError::io(e, &url))?.is_dir()
            {
                return Err(FsError::new(FsCode::IsADirectory, Some(url), "expected a file").into());
            }
            let file = tokio::fs::File::open(path).await.map_err(|e| FsError::io(e, &url))?;
            Ok::<FileStream, kardia_core::Error>(
                stream::try_unfold((file, url), |(mut file, url)| async move {
                    let mut chunk = vec![0; 64 * 1024];
                    let count = file.read(&mut chunk).await.map_err(|e| FsError::io(e, &url))?;
                    if count == 0 {
                        return Ok(None);
                    }
                    chunk.truncate(count);
                    Ok(Some((Bytes::from(chunk), (file, url))))
                })
                .boxed(),
            )
        })
        .try_flatten()
        .boxed()
    }
    fn write_with(
        &self,
        url: Url,
        data: Vec<u8>,
        options: crate::WriteOptions,
    ) -> futures::future::BoxFuture<'static, Result<Option<String>>> {
        async move {
            let path = local_path(&url)?;
            if let Some(expected) = &options.if_match {
                let metadata = tokio::fs::metadata(&path).await.map_err(|error| FsError::io(error, &url))?;
                let version = version_of(&metadata);
                if &version != expected {
                    return Err(FsError::new(
                        FsCode::VersionMismatch,
                        Some(url.clone()),
                        "the file has another version",
                    )
                    .into());
                }
            }
            if options.atomic {
                // A temporary file beside the destination is renamed over it, so a
                // reader sees the previous or the new content.
                let temporary = path.with_file_name(format!(
                    ".{}.{}.tmp",
                    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default(),
                    unique()
                ));
                let write = async {
                    tokio::fs::write(&temporary, &data).await.map_err(|error| FsError::io(error, &url))?;
                    if options.exclusive {
                        // Creating the destination exclusively is what `exclusive`
                        // means; a staged rename would replace it silently.
                        return Err(FsError::new(
                            FsCode::NotSupported,
                            Some(url.clone()),
                            "an atomic write cannot be exclusive",
                        )
                        .into());
                    }
                    if let Some(mode) = options.mode {
                        set_mode(&temporary, mode, &url).await?;
                    }
                    tokio::fs::rename(&temporary, &path).await.map_err(|error| FsError::io(error, &url))?;
                    Ok::<_, kardia_core::Error>(())
                };
                if let Err(error) = write.await {
                    let _ = tokio::fs::remove_file(&temporary).await;
                    return Err(error);
                }
            } else {
                if options.exclusive {
                    let file = tokio::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)
                        .await
                        .map_err(|error| FsError::io(error, &url))?;
                    drop(file);
                }
                tokio::fs::write(&path, &data).await.map_err(|error| FsError::io(error, &url))?;
                if let Some(mode) = options.mode {
                    set_mode(&path, mode, &url).await?;
                }
            }
            // The stamp is what a conditional write compares, so it must change
            // even when two writes land in the same clock tick.
            touch(&path, &url)?;
            let metadata = tokio::fs::metadata(&path).await.map_err(|error| FsError::io(error, &url))?;
            Ok(Some(version_of(&metadata)))
        }
        .boxed()
    }

    fn read_range(&self, url: Url, options: crate::ReadOptions) -> FileStream {
        stream::once(async move {
            let path = local_path(&url)?;
            let metadata = tokio::fs::metadata(&path).await.map_err(|error| FsError::io(error, &url))?;
            if metadata.is_dir() {
                return Err(FsError::new(FsCode::IsADirectory, Some(url), "expected a file").into());
            }
            if let Some(expected) = &options.if_match
                && &version_of(&metadata) != expected
            {
                return Err(
                    FsError::new(FsCode::VersionMismatch, Some(url.clone()), "the file has another version").into()
                );
            }
            let size = metadata.len() as i64;
            let (start, end) = range(options.start, options.end, size);
            let mut file = tokio::fs::File::open(&path).await.map_err(|error| FsError::io(error, &url))?;
            if start > 0 {
                use tokio::io::AsyncSeekExt;
                file.seek(std::io::SeekFrom::Start(start as u64)).await.map_err(|error| FsError::io(error, &url))?;
            }
            let remaining = end.saturating_sub(start);
            Ok::<FileStream, kardia_core::Error>(
                stream::try_unfold((file, url, remaining), |(mut file, url, remaining)| async move {
                    if remaining == 0 {
                        return Ok(None);
                    }
                    let mut chunk = vec![0; 64 * 1024];
                    let count = file.read(&mut chunk).await.map_err(|error| FsError::io(error, &url))?;
                    if count == 0 {
                        return Ok(None);
                    }
                    chunk.truncate(count.min(remaining as usize));
                    let left = remaining - chunk.len() as i64;
                    Ok(Some((Bytes::from(chunk), (file, url, left))))
                })
                .boxed(),
            )
        })
        .try_flatten()
        .boxed()
    }

    fn mkdir(&self, url: Url, options: crate::MkdirOptions) -> futures::future::BoxFuture<'static, Result<()>> {
        async move {
            let path = local_path(&url)?;
            let result = if options.recursive {
                tokio::fs::create_dir_all(&path).await
            } else {
                tokio::fs::create_dir(&path).await
            };
            result.map_err(|error| FsError::io(error, &url))?;
            Ok(())
        }
        .boxed()
    }

    fn copy(&self, from: Url, to: Url, options: crate::CopyOptions) -> futures::future::BoxFuture<'static, Result<()>> {
        async move {
            let (from_path, to_path) = (local_path(&from)?, local_path(&to)?);
            // `dereference` copies what a link points to; the default copies the link
            // itself, which `std::fs::copy` already does not do for a directory.
            if options.dereference {
                Box::pin(copy_dereferenced(&from_path, &to_path, &from, &to)).await?;
            } else if options.recursive {
                copy_tree(&from_path, &to_path, &from, &to).await?;
            } else if options.exclusive {
                let mut target = tokio::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&to_path)
                    .await
                    .map_err(|error| FsError::io(error, &to))?;
                let mut source = tokio::fs::File::open(&from_path).await.map_err(|error| FsError::io(error, &from))?;
                tokio::io::copy(&mut source, &mut target).await.map_err(|error| FsError::io(error, &to))?;
            } else {
                tokio::fs::copy(&from_path, &to_path).await.map_err(|error| FsError::io(error, &from))?;
            }
            Ok(())
        }
        .boxed()
    }

    fn realpath(&self, url: Url) -> futures::future::BoxFuture<'static, Result<Url>> {
        async move {
            let path = local_path(&url)?;
            let resolved = tokio::fs::canonicalize(&path).await.map_err(|error| FsError::io(error, &url))?;
            Url::from_file_path(&resolved)
                .map_err(|_| FsError::new(FsCode::InvalidPath, Some(url), "cannot represent the real path").into())
        }
        .boxed()
    }

    fn symlink(&self, target: String, url: Url) -> futures::future::BoxFuture<'static, Result<()>> {
        async move {
            let path = local_path(&url)?;
            #[cfg(unix)]
            let created = tokio::fs::symlink(&target, &path).await;
            #[cfg(windows)]
            let created = async {
                // A link to a directory needs its own call on Windows.
                let points_to_directory = tokio::fs::metadata(&target).await.map(|m| m.is_dir()).unwrap_or(false);
                if points_to_directory {
                    tokio::fs::symlink_dir(&target, &path).await
                } else {
                    tokio::fs::symlink_file(&target, &path).await
                }
            }
            .await;
            created.map_err(|error| FsError::io(error, &url))?;
            Ok(())
        }
        .boxed()
    }

    fn readlink(&self, url: Url) -> futures::future::BoxFuture<'static, Result<String>> {
        async move {
            let path = local_path(&url)?;
            let target = tokio::fs::read_link(&path).await.map_err(|error| FsError::io(error, &url))?;
            Ok(target.to_string_lossy().into_owned())
        }
        .boxed()
    }

    fn readdir(&self, url: Url) -> futures::future::BoxFuture<'static, Result<Vec<crate::DirEntry>>> {
        async move {
            let path = local_path(&url)?;
            let mut entries = Vec::new();
            let mut reader = tokio::fs::read_dir(&path).await.map_err(|error| FsError::io(error, &url))?;
            while let Some(entry) = reader.next_entry().await.map_err(|error| FsError::io(error, &url))? {
                // The type is the entry's own, so a link is reported as one.
                let kind = entry.file_type().await.map_err(|error| FsError::io(error, &url))?;
                let kind = if kind.is_dir() {
                    crate::EntryType::Directory
                } else if kind.is_file() {
                    crate::EntryType::File
                } else if kind.is_symlink() {
                    crate::EntryType::Symlink
                } else {
                    crate::EntryType::Other
                };
                entries.push(crate::DirEntry { name: entry.file_name().to_string_lossy().into_owned(), kind });
            }
            Ok(entries)
        }
        .boxed()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalFsConfig {
    #[serde(default = "mount")]
    pub mount: String,
}
impl Default for LocalFsConfig {
    fn default() -> Self {
        Self { mount: mount() }
    }
}
fn mount() -> String {
    "file:".into()
}

pub struct LocalFsPlugin;
impl Plugin for LocalFsPlugin {
    const NAME: &'static str = "fs:local";
    type Config = LocalFsConfig;
    fn inject() -> Inject {
        Inject::new().with("std:fs")
    }
    async fn apply(ctx: Context, config: Arc<LocalFsConfig>) -> Result<()> {
        ctx.get::<Fs>()?.mount(&config.mount, Arc::new(LocalFs))?;
        Ok(())
    }
}

/// The version a provider reports for a file: its modification time and size.
fn version_of(metadata: &std::fs::Metadata) -> String {
    let nanos = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since| since.as_nanos())
        .unwrap_or_default();
    format!("{nanos}-{}", metadata.len())
}

/// Marks a file as just written, so its version differs from the one before even
/// when both writes land in the same clock tick.
fn touch(path: &std::path::Path, url: &Url) -> Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST: AtomicU64 = AtomicU64::new(0);
    let observed = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_nanos() as u64)
        .unwrap_or_default();
    // The stamp never goes backwards, so a write always produces a new version.
    let stamp = LAST.fetch_max(observed, Ordering::SeqCst).max(observed) + 1;
    LAST.store(stamp, Ordering::SeqCst);
    let time = std::time::UNIX_EPOCH + std::time::Duration::from_nanos(stamp);
    let file = std::fs::OpenOptions::new().write(true).open(path).map_err(|error| FsError::io(error, url))?;
    file.set_modified(time).map_err(|error| FsError::io(error, url))?;
    Ok(())
}

/// A number that distinguishes the temporary files of one process.
fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[cfg(unix)]
async fn set_mode(path: &std::path::Path, mode: u32, url: &Url) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let permissions = std::fs::Permissions::from_mode(mode);
    tokio::fs::set_permissions(path, permissions)
        .await
        .map_err(|error| <FsError as Into<kardia_core::Error>>::into(FsError::io(error, url)))
}

#[cfg(not(unix))]
async fn set_mode(_path: &std::path::Path, _mode: u32, _url: &Url) -> Result<()> {
    // A platform without POSIX permission bits keeps its own.
    Ok(())
}

/// The first and the end of a range, with the semantics of `Blob.slice()`.
fn range(start: i64, end: Option<i64>, size: i64) -> (i64, i64) {
    let resolve = |value: i64| -> i64 {
        let value = if value < 0 { size + value } else { value };
        value.clamp(0, size)
    };
    let start = resolve(start);
    let end = resolve(end.unwrap_or(size)).max(start);
    (start, end)
}

/// Copies with every symbolic link followed, so a link is copied as what it
/// points to.
async fn copy_dereferenced(from: &std::path::Path, to: &std::path::Path, from_url: &Url, to_url: &Url) -> Result<()> {
    // A link is copied as a link, with the same target; `symlink_metadata` is what
    // tells one apart from what it points to.
    let metadata = tokio::fs::symlink_metadata(from).await.map_err(|error| FsError::io(error, from_url))?;
    if metadata.file_type().is_symlink() {
        let target = tokio::fs::read_link(from).await.map_err(|error| FsError::io(error, from_url))?;
        #[cfg(unix)]
        tokio::fs::symlink(&target, to).await.map_err(|error| FsError::io(error, to_url))?;
        #[cfg(windows)]
        {
            let points_to_directory = metadata.is_dir();
            if points_to_directory {
                tokio::fs::symlink_dir(&target, to).await.map_err(|error| FsError::io(error, to_url))?;
            } else {
                tokio::fs::symlink_file(&target, to).await.map_err(|error| FsError::io(error, to_url))?;
            }
        }
        return Ok(());
    }
    if metadata.is_dir() {
        tokio::fs::create_dir_all(to).await.map_err(|error| FsError::io(error, to_url))?;
        let mut reader = tokio::fs::read_dir(from).await.map_err(|error| FsError::io(error, from_url))?;
        while let Some(entry) = reader.next_entry().await.map_err(|error| FsError::io(error, from_url))? {
            let name = entry.file_name();
            Box::pin(copy_dereferenced(&from.join(&name), &to.join(&name), from_url, to_url)).await?;
        }
        Ok(())
    } else {
        tokio::fs::copy(from, to).await.map_err(|error| FsError::io(error, from_url))?;
        Ok(())
    }
}

async fn copy_tree(from: &std::path::Path, to: &std::path::Path, from_url: &Url, to_url: &Url) -> Result<()> {
    let metadata = tokio::fs::metadata(from).await.map_err(|error| FsError::io(error, from_url))?;
    if metadata.is_dir() {
        tokio::fs::create_dir_all(to).await.map_err(|error| FsError::io(error, to_url))?;
        let mut reader = tokio::fs::read_dir(from).await.map_err(|error| FsError::io(error, from_url))?;
        while let Some(entry) = reader.next_entry().await.map_err(|error| FsError::io(error, from_url))? {
            let name = entry.file_name();
            let (child_from, child_to) = (from.join(&name), to.join(&name));
            Box::pin(copy_tree(&child_from, &child_to, from_url, to_url)).await?;
        }
        Ok(())
    } else {
        tokio::fs::copy(from, to).await.map_err(|error| FsError::io(error, from_url))?;
        Ok(())
    }
}

fn local_path(url: &Url) -> Result<std::path::PathBuf> {
    let path = url.path().to_ascii_lowercase();
    if path.contains("%2f") || (cfg!(windows) && path.contains("%5c")) {
        return Err(FsError::new(FsCode::InvalidPath, Some(url.clone()), "encoded path separator in file URL").into());
    }
    url.to_file_path()
        .map_err(|_| FsError::new(FsCode::InvalidPath, Some(url.clone()), "invalid local file URL").into())
}
