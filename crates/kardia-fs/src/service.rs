//! The `"fs"` service, its plugin, and the extension trait that starts a watch.

use std::path::Path;
use std::sync::Arc;

use kardia_core::{Context, Plugin, Result, Service, Svc};

use crate::filter::Filter;
use crate::watcher::Watcher;

/// The file-system service.
pub struct Fs;

impl Service for Fs {
    const NAME: &'static str = "fs";
}

/// Provides [`Fs`].
pub struct FsPlugin;

impl Plugin for FsPlugin {
    const NAME: &'static str = "fs";
    type Config = ();

    fn provide() -> Vec<kardia_core::Key> {
        vec![Fs::NAME.into()]
    }

    async fn apply(ctx: Context, _: Arc<()>) -> Result<()> {
        ctx.provide(Fs)?;
        Ok(())
    }
}

#[derive(Default, Clone)]
pub struct WatchOptions {
    /// Decides which entries below a watched directory are kept.
    pub filter: Option<Filter>,
}

pub trait FsExt {
    /// Watches a file or a directory (recursively). A relative path is
    /// resolved against the working directory, and the result is normalized.
    fn watch(&self, path: impl AsRef<Path>, options: WatchOptions) -> Result<Watcher>;
}

impl FsExt for Svc<Fs> {
    fn watch(&self, path: impl AsRef<Path>, options: WatchOptions) -> Result<Watcher> {
        Watcher::start(self.caller(), path.as_ref(), options)
    }
}
