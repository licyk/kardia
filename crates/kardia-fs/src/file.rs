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

use crate::{Fs, FsCode, FsError};

pub type FileStream = BoxStream<'static, Result<Bytes>>;

/// Providers resolve their own dependencies at their definition site.
pub trait FsProvider: Send + Sync + 'static {
    fn read(&self, url: Url) -> FileStream {
        stream::once(async move {
            Err(FsError::new(FsCode::NotSupported, Some(url), "provider does not support reading").into())
        })
        .boxed()
    }
}

#[derive(Clone)]
pub struct FileRef {
    pub url: Url,
    pub(crate) fs: Svc<Fs>,
}
impl FileRef {
    pub fn stream(&self) -> Result<FileStream> {
        let mount = self
            .fs
            .mounts
            .lock()
            .iter()
            .filter(|mount| {
                if let Some(scheme) = mount.prefix.strip_suffix(':') {
                    self.url.scheme() == scheme
                } else {
                    self.url.as_str().starts_with(&mount.prefix) || format!("{}/", self.url) == mount.prefix
                }
            })
            .max_by_key(|mount| mount.prefix.len())
            .cloned()
            .ok_or_else(|| FsError::new(FsCode::NoProvider, Some(self.url.clone()), "no provider mounted for file"))?;
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
