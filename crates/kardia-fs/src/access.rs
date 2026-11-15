use std::path::Path;
use std::sync::Arc;

use kardia_core::{Disposer, EffectHandle, Result, Svc};
use tokio::sync::watch;
use url::Url;

use crate::service::Mount;
use crate::{FileRef, Fs, FsCode, FsError, FsProvider, WatchOptions, Watcher};

pub trait FsExt {
    fn watch(&self, path: impl AsRef<Path>, options: WatchOptions) -> Result<Watcher>;
    fn base_url(&self) -> Result<Url>;
    fn resolve(&self, path: &str) -> Result<Url>;
    fn file(&self, path: &str) -> Result<FileRef>;
    fn mount(&self, prefix: &str, provider: Arc<dyn FsProvider>) -> Result<EffectHandle>;
}
impl FsExt for Svc<Fs> {
    fn watch(&self, path: impl AsRef<Path>, options: WatchOptions) -> Result<Watcher> {
        Watcher::start(self.caller(), path.as_ref(), options)
    }
    fn base_url(&self) -> Result<Url> {
        let mut base = self.base.clone();
        for meta in self.intercepts() {
            if let Some(value) = meta.get("baseUrl").and_then(|v| v.as_str()) {
                base = base.join(value).map_err(|e| FsError::new(FsCode::InvalidPath, None, e.to_string()))?;
            }
        }
        Ok(base)
    }
    fn resolve(&self, path: &str) -> Result<Url> {
        self.base_url()?.join(path).map_err(|e| FsError::new(FsCode::InvalidPath, None, e.to_string()).into())
    }
    fn file(&self, path: &str) -> Result<FileRef> {
        Ok(FileRef { url: self.resolve(path)?, fs: self.clone() })
    }
    fn mount(&self, prefix: &str, provider: Arc<dyn FsProvider>) -> Result<EffectHandle> {
        let prefix = normalize_prefix(prefix)?;
        let fs = self.arc().clone();
        let (stop, _) = watch::channel(false);
        let (readers, _) = watch::channel(0);
        let mount = Arc::new(Mount { prefix, provider, stop, readers });
        self.caller().effect_sync("fs.mount()", |scope| {
            let mut mounts = fs.mounts.lock();
            if mounts.iter().any(|old| old.prefix == mount.prefix) {
                return Err(kardia_core::Error::msg(format!("provider already mounted at {}", mount.prefix)));
            }
            mounts.push(mount.clone());
            drop(mounts);
            scope.defer(Disposer::future(move || {
                fs.mounts.lock().retain(|old| !Arc::ptr_eq(old, &mount));
                mount.stop.send_replace(true);
                let mut readers = mount.readers.subscribe();
                async move {
                    let _ = readers.wait_for(|count| *count == 0).await;
                }
            }));
            Ok(())
        })
    }
}
fn normalize_prefix(prefix: &str) -> Result<String> {
    if let Some(scheme) = prefix.strip_suffix(':') {
        if scheme.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
            && scheme.bytes().all(|b| b.is_ascii_alphanumeric() || b"+.-".contains(&b))
        {
            return Ok(prefix.to_ascii_lowercase());
        }
    }
    let url = Url::parse(prefix)?;
    let mut prefix = url.to_string();
    if !prefix.ends_with('/') {
        prefix.push('/');
    }
    Ok(prefix)
}
