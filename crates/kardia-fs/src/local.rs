use std::sync::Arc;

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
        crate::Capabilities { write: true, rename: true, remove: true, stat: true }
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
            Ok(crate::Stats { kind, size: metadata.len(), mtime: metadata.modified().ok(), mode })
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

fn local_path(url: &Url) -> Result<std::path::PathBuf> {
    let path = url.path().to_ascii_lowercase();
    if path.contains("%2f") || (cfg!(windows) && path.contains("%5c")) {
        return Err(FsError::new(FsCode::InvalidPath, Some(url.clone()), "encoded path separator in file URL").into());
    }
    url.to_file_path()
        .map_err(|_| FsError::new(FsCode::InvalidPath, Some(url.clone()), "invalid local file URL").into())
}
