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
    pub base_url: Option<Url>,
}

/// File readers are routed through mounts; native watches retain their existing API.
pub struct Fs {
    pub(crate) base: Url,
    pub(crate) mounts: Mutex<Vec<Arc<Mount>>>,
}
impl Service for Fs {
    const NAME: &'static str = "fs";
}

pub(crate) struct Mount {
    pub prefix: String,
    pub provider: Arc<dyn FsProvider>,
    pub stop: watch::Sender<bool>,
    pub readers: watch::Sender<usize>,
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
        ctx.provide(Fs { base, mounts: Mutex::new(Vec::new()) })?;
        Ok(())
    }
}

#[derive(Default, Clone)]
pub struct WatchOptions {
    pub filter: Option<Filter>,
}
