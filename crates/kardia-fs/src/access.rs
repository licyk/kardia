use std::path::Path;
use std::sync::Arc;

use futures::FutureExt;
use kardia_core::{Disposer, EffectHandle, Result, Svc};
use tokio::sync::watch;
use url::Url;

use crate::service::Mount;
use crate::{
    Capabilities, CopyOptions, DirEntry, DirRef, FileRef, Fs, FsCode, FsError, FsProvider, MkdirOptions, RemoveOptions,
    Stats, WatchOptions, Watcher,
};

pub trait FsExt {
    fn watch(&self, path: impl AsRef<Path>, options: WatchOptions) -> Result<Watcher>;
    fn base_url(&self) -> Result<Url>;
    fn resolve(&self, path: &str) -> Result<Url>;
    fn file(&self, path: &str) -> Result<FileRef>;
    fn mount(&self, prefix: &str, provider: Arc<dyn FsProvider>) -> Result<EffectHandle>;
    /// Writes `data` to the location of `path`, replacing its content.
    fn write(&self, path: &str, data: impl Into<Vec<u8>>) -> impl Future<Output = Result<()>> + Send;
    /// Moves the entry at `from` to `to`, keeping its content.
    fn rename(&self, from: &str, to: &str) -> impl Future<Output = Result<()>> + Send;
    /// Removes the entry at `path`, which must be a file or an empty directory.
    fn remove(&self, path: &str, options: RemoveOptions) -> impl Future<Output = Result<()>> + Send;
    /// The metadata of the entry at `path`.
    fn stat(&self, path: &str) -> impl Future<Output = Result<Stats>> + Send;
    /// Whether an entry exists at `path`.
    fn exists(&self, path: &str) -> impl Future<Output = Result<bool>> + Send;
    /// Creates a directory, optionally with its missing parents.
    fn mkdir(&self, path: &str, options: MkdirOptions) -> impl Future<Output = Result<()>> + Send;
    /// Copies an entry, optionally with the entries below a directory.
    fn copy(&self, from: &str, to: &str, options: CopyOptions) -> impl Future<Output = Result<()>> + Send;
    /// The location with every symbolic link resolved.
    fn realpath(&self, path: &str) -> impl Future<Output = Result<Url>> + Send;
    /// Creates a symbolic link at `path` that stores `target` as given.
    fn symlink(&self, target: &str, path: &str) -> impl Future<Output = Result<()>> + Send;
    /// The target of a symbolic link, as stored.
    fn readlink(&self, path: &str) -> impl Future<Output = Result<String>> + Send;
    /// The entries of a directory, in an unspecified order.
    fn readdir(&self, path: &str) -> impl Future<Output = Result<Vec<DirEntry>>> + Send;
    /// A reference to a directory, which resolves `path` against its base URL.
    fn dir(&self, path: &str) -> Result<DirRef>;
    /// Resolves `path` against `base`, which names a directory.
    fn resolve_at(&self, base: &Url, path: &str) -> Result<Url>;
}
impl FsExt for Svc<Fs> {
    fn watch(&self, path: impl AsRef<Path>, options: WatchOptions) -> Result<Watcher> {
        Watcher::start(self.caller(), path.as_ref(), options)
    }
    fn base_url(&self) -> Result<Url> {
        let mut base = self.base.clone();
        for meta in self.intercepts() {
            if let Some(value) = meta.get("baseUrl").and_then(|v| v.as_str()) {
                base = crate::url::resolve(Some(&base), value)
                    .and_then(crate::url::directory)
                    .map_err(|e| kardia_core::ValidationError::message(format!("baseUrl: {e}")))?;
            }
        }
        Ok(base)
    }
    fn resolve(&self, path: &str) -> Result<Url> {
        crate::url::resolve(Some(&self.base_url()?), path)
            .map_err(|e| FsError::new(FsCode::InvalidPath, None, e.to_string()).into())
    }
    fn file(&self, path: &str) -> Result<FileRef> {
        Ok(FileRef { url: self.resolve(path)?, fs: self.clone(), slices: Vec::new() })
    }
    fn write(&self, path: &str, data: impl Into<Vec<u8>>) -> impl Future<Output = Result<()>> + Send {
        // The conversion happens before the async block, so the caller's type does
        // not have to be `Send`.
        let data = data.into();
        let (url, mount) = match self.resolve(path).and_then(|url| {
            let mount = self.provider_capable(&url, |caps| caps.write, "writing")?;
            Ok((url, mount))
        }) {
            Ok(found) => found,
            Err(error) => return futures::future::ready(Err(error)).boxed(),
        };
        async move { mount.provider.write(url, data).await }.boxed()
    }
    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        let (from, to) = (self.resolve(from)?, self.resolve(to)?);
        let mount = self.provider_capable(&from, |caps| caps.rename, "renaming")?;
        mount.provider.rename(from, to).await
    }
    async fn remove(&self, path: &str, options: RemoveOptions) -> Result<()> {
        let url = self.resolve(path)?;
        let mount = self.provider_capable(&url, |caps| caps.remove, "removing")?;
        match mount.provider.remove_with(url, options).await {
            Err(error)
                if options.force && error.downcast_ref::<FsError>().is_some_and(|e| e.code == FsCode::NotFound) =>
            {
                Ok(())
            }
            result => result,
        }
    }
    async fn stat(&self, path: &str) -> Result<Stats> {
        let url = self.resolve(path)?;
        let mount = self.provider_capable(&url, |caps| caps.stat, "stat")?;
        mount.provider.stat(url).await
    }
    async fn exists(&self, path: &str) -> Result<bool> {
        match self.stat(path).await {
            Ok(_) => Ok(true),
            Err(error) if error.downcast_ref::<FsError>().is_some_and(|error| error.code == FsCode::NotFound) => {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }
    async fn mkdir(&self, path: &str, options: MkdirOptions) -> Result<()> {
        let url = self.resolve(path)?;
        let mount = self.provider_capable(&url, |caps| caps.mkdir, "creating a directory")?;
        mount.provider.mkdir(url, options).await
    }
    async fn copy(&self, from: &str, to: &str, options: CopyOptions) -> Result<()> {
        let (from, to) = (self.resolve(from)?, self.resolve(to)?);
        let capable = |caps: &Capabilities| caps.copy && (!options.recursive || caps.copy_recursive);
        let mount = self.provider_capable(&from, capable, "copying")?;
        mount.provider.copy(from, to, options).await
    }
    async fn realpath(&self, path: &str) -> Result<Url> {
        let url = self.resolve(path)?;
        let mount = self.provider_capable(&url, |caps| caps.realpath, "realpath")?;
        mount.provider.realpath(url).await
    }
    async fn symlink(&self, target: &str, path: &str) -> Result<()> {
        let url = self.resolve(path)?;
        let mount = self.provider_capable(&url, |caps| caps.symlink, "creating a symbolic link")?;
        mount.provider.symlink(target.to_owned(), url).await
    }
    async fn readlink(&self, path: &str) -> Result<String> {
        let url = self.resolve(path)?;
        let mount = self.provider_capable(&url, |caps| caps.links, "readlink")?;
        mount.provider.readlink(url).await
    }
    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        let url = self.resolve(path)?;
        let mount = self.provider_capable(&url, |caps| caps.readdir, "readdir")?;
        mount.provider.readdir(url).await
    }
    fn dir(&self, path: &str) -> Result<DirRef> {
        let url = self.resolve(path)?;
        DirRef::new(self.clone(), url)
    }
    fn resolve_at(&self, base: &Url, path: &str) -> Result<Url> {
        crate::url::resolve(Some(base), path)
            .map_err(|error| FsError::new(FsCode::InvalidPath, None, error.to_string()).into())
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
    // The empty prefix contains every location: it is the shortest, so a mount at
    // it serves only what no other mount claims.
    if prefix.is_empty() {
        return Ok(String::new());
    }
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
