use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use bytes::{Bytes, BytesMut};
use futures::{
    FutureExt, StreamExt,
    stream::{self, BoxStream},
};
use kardia_core::{Disposer, EffectHandle, Error, Result, Svc};
use parking_lot::Mutex;
use tokio::sync::mpsc;
use url::Url;

use futures::future::BoxFuture;

use crate::{Fs, FsCode, FsError};

pub type FileStream = BoxStream<'static, Result<Bytes>>;

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

    fn stat(&self, url: Url) -> BoxFuture<'static, Result<Stats>> {
        let _ = url;
        async move { Err(FsError::new(FsCode::NotSupported, None, "provider does not support stat").into()) }.boxed()
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
#[derive(Clone, Copy, Debug)]
pub struct Stats {
    pub kind: EntryType,
    /// The length of the content in bytes for a file.
    pub size: u64,
    pub mtime: Option<std::time::SystemTime>,
    /// The permission bits, as in the lower 12 bits of a POSIX mode.
    pub mode: Option<u32>,
}

#[derive(Clone)]
pub struct FileRef {
    pub url: Url,
    pub(crate) fs: Svc<Fs>,
}
impl FileRef {
    pub fn stream(&self) -> Result<FileStream> {
        let mount = self.fs.provider_for(&self.url)?;
        let source =
            catch_unwind(AssertUnwindSafe(|| mount.provider.read(self.url.clone()))).map_err(Error::from_panic)?;
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
