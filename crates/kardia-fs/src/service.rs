use std::sync::Arc;

use kardia_core::{Context, Plugin, Result, Service};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use url::Url;

use crate::{Filter, FsProvider};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FsConfig {
    #[serde(default, deserialize_with = "crate::url::deserialize_base")]
    pub base_url: Option<Url>,
}

/// File readers are routed through mounts; native watches retain their existing API.
pub struct Fs {
    pub(crate) base: Url,
    pub(crate) mounts: Mutex<Vec<Arc<Mount>>>,
}
impl Service for Fs {
    const NAME: &'static str = "std:fs";
}

impl Fs {
    /// The provider that serves `url`, as a handle that keeps it alive. A plugin
    /// that must read or write while it unloads holds this instead of looking the
    /// provider up again, when a mount that another plugin owns is already gone.
    pub fn provider_of(&self, url: &Url) -> Result<crate::Provider> {
        Ok(crate::Provider(self.provider_for(url)?))
    }

    /// The provider whose mount contains `url`: the longest prefix that does, so
    /// a provider mounted at the empty string serves only the locations that no
    /// other mount claims.
    pub(crate) fn provider_for(&self, url: &Url) -> Result<Arc<Mount>> {
        self.mounts
            .lock()
            .iter()
            .filter(|mount| {
                if let Some(scheme) = mount.prefix.strip_suffix(':') {
                    url.scheme() == scheme
                } else {
                    url.as_str().starts_with(&mount.prefix) || format!("{url}/") == mount.prefix
                }
            })
            .max_by_key(|mount| mount.prefix.len())
            .cloned()
            .ok_or_else(|| {
                crate::FsError::new(crate::FsCode::NoProvider, Some(url.clone()), "no provider mounted for file").into()
            })
    }

    /// The provider for a location that must support `capability`, or a
    /// `NotSupported` that names the operation.
    pub(crate) fn provider_capable(
        &self,
        url: &Url,
        capability: impl Fn(&crate::Capabilities) -> bool,
        operation: &str,
    ) -> Result<Arc<Mount>> {
        let mount = self.provider_for(url)?;
        if capability(&mount.provider.capabilities()) {
            return Ok(mount);
        }
        Err(crate::FsError::new(
            crate::FsCode::NotSupported,
            Some(url.clone()),
            format!("the provider of {url} does not support {operation}"),
        )
        .into())
    }
}

pub(crate) struct Mount {
    pub prefix: String,
    pub provider: Arc<dyn FsProvider>,
    pub stop: watch::Sender<bool>,
    pub readers: watch::Sender<usize>,
}

/// A provider handle that outlives the mount list, for a caller that must keep
/// working while its owner unloads.
#[derive(Clone)]
pub struct Provider(pub(crate) Arc<Mount>);

impl Provider {
    pub fn capabilities(&self) -> crate::Capabilities {
        self.0.provider.capabilities()
    }

    pub fn read(&self, url: Url) -> crate::FileStream {
        self.0.provider.read(url)
    }

    pub async fn write(&self, url: Url, data: Vec<u8>) -> Result<()> {
        self.0.provider.write(url, data).await
    }

    pub async fn rename(&self, from: Url, to: Url) -> Result<()> {
        self.0.provider.rename(from, to).await
    }

    pub async fn remove(&self, url: Url) -> Result<()> {
        self.0.provider.remove(url).await
    }

    pub async fn stat(&self, url: Url) -> Result<crate::Stats> {
        self.0.provider.stat(url).await
    }

    /// Reads a range of the content, or all of it.
    pub fn read_range(&self, url: Url, options: crate::ReadOptions) -> crate::FileStream {
        self.0.provider.read_range(url, options)
    }

    pub async fn mkdir(&self, url: Url, options: crate::MkdirOptions) -> Result<()> {
        self.0.provider.mkdir(url, options).await
    }

    pub async fn copy(&self, from: Url, to: Url, options: crate::CopyOptions) -> Result<()> {
        self.0.provider.copy(from, to, options).await
    }

    pub async fn realpath(&self, url: Url) -> Result<Url> {
        self.0.provider.realpath(url).await
    }

    pub async fn symlink(&self, target: String, url: Url) -> Result<()> {
        self.0.provider.symlink(target, url).await
    }

    pub async fn readlink(&self, url: Url) -> Result<String> {
        self.0.provider.readlink(url).await
    }

    pub async fn readdir(&self, url: Url) -> Result<Vec<crate::DirEntry>> {
        self.0.provider.readdir(url).await
    }
}

pub struct FsPlugin;
impl Plugin for FsPlugin {
    const NAME: &'static str = "fs";
    type Config = FsConfig;
    fn provide() -> Vec<kardia_core::Key> {
        vec![Fs::NAME.into()]
    }
    async fn apply(ctx: Context, config: Arc<FsConfig>) -> Result<()> {
        let base = match &config.base_url {
            Some(url) => url.clone(),
            None => Url::from_directory_path(std::env::current_dir()?)
                .map_err(|_| kardia_core::Error::msg("cannot represent working directory as a file URL"))?,
        };
        let base = crate::url::directory(base)
            .map_err(|error| kardia_core::ValidationError::message(format!("baseUrl: {error}")))?;
        ctx.provide(Fs { base, mounts: Mutex::new(Vec::new()) })?;
        Ok(())
    }
}

#[derive(Default, Clone)]
pub struct WatchOptions {
    pub filter: Option<Filter>,
}
