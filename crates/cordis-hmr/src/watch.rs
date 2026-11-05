//! Watch mode: reloads changed files as they change on disk.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use cordis_core::{Context, Fiber, Inject, Logger, Result, Svc};
use cordis_fs::{Filter, Fs, FsExt, WatchOptions};
use serde::Deserialize;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::hmr::{Hmr, HmrExt};

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct WatchConfig {
    /// Reload on file changes. Requires the `fs` service.
    pub enabled: bool,
    /// Files to watch: patterns relative to `base`, as the `fs` watch filter.
    pub files: Vec<String>,
    /// Changes within this many milliseconds are reloaded together.
    pub debounce: u64,
}

impl Default for WatchConfig {
    fn default() -> Self {
        let files = [".", "!**/.*", "!**/target"].map(String::from).to_vec();
        Self { enabled: false, files, debounce: 100 }
    }
}

/// Watches `base` once `fs` is available, as a fiber below `ctx`.
pub(crate) fn start(ctx: &Context, base: PathBuf, config: WatchConfig) -> Result<Fiber> {
    ctx.logger().info(format_args!("watching {:?} in {}", config.files, base.display()));
    ctx.inject(Inject::of::<(Fs,)>(), move |ctx| {
        let (base, config) = (base.clone(), config.clone());
        async move {
            let hmr = ctx.get::<Hmr>()?;
            let filter = Some(Filter::Patterns(config.files.clone()));
            let watcher = ctx.get::<Fs>()?.watch(&base, WatchOptions { filter })?;
            let (changes, received) = unbounded_channel();
            let logger = hmr.logger();
            let root = base.clone();
            watcher.on_change(move |path| {
                logger.debug(format_args!("change detected at {}", relative(&root, path).display()));
                drop(changes.send(path.to_path_buf()));
            });
            // ends when the watch is disposed and drops the listener
            tokio::spawn(batches(hmr, base, received, Duration::from_millis(config.debounce)));
            Ok(())
        }
    })
}

/// Reconciles the changes of each debounce window, one batch after another,
/// so that a batch takes over the changes retained by the one before.
async fn batches(hmr: Svc<Hmr>, base: PathBuf, mut changes: UnboundedReceiver<PathBuf>, debounce: Duration) {
    let mut retained = HashSet::new();
    while let Some(first) = changes.recv().await {
        let mut files = std::mem::take(&mut retained);
        files.insert(first);
        loop {
            match tokio::time::timeout(debounce, changes.recv()).await {
                Ok(Some(path)) => drop(files.insert(path)),
                Ok(None) => return,
                Err(_) => break,
            }
        }
        retained = flush(&hmr, &base, files).await;
    }
}

/// Returns the files to try again: those of failed imports, or all of them
/// if the transaction failed. A change to a host module is only reported,
/// since restarting is up to the host.
async fn flush(hmr: &Svc<Hmr>, base: &Path, files: HashSet<PathBuf>) -> HashSet<PathBuf> {
    let logger = hmr.logger();
    logger.debug(format_args!("reconcile {} changed files", files.len()));
    let roots: Vec<PathBuf> = files.iter().cloned().collect();
    match hmr.reconcile(move || async move { Ok(roots) }).await {
        Ok(result) => {
            if !result.skipped_host_modules.is_empty() {
                warn_skipped(&logger, base, &result.skipped_host_modules);
            }
            // an import failure has been logged by the transaction itself
            let failed = result.import_failures.iter().flat_map(|failure| &failure.modules);
            failed.map(PathBuf::from).filter(|path| files.contains(path)).collect()
        }
        Err(error) => {
            logger.warn(&error);
            files
        }
    }
}

fn warn_skipped(logger: &Logger, base: &Path, modules: &[String]) {
    let lines: Vec<String> =
        modules.iter().map(|id| format!("- {}", relative(base, Path::new(id)).display())).collect();
    logger.warn(format_args!("skipped host modules:\n{}", lines.join("\n")));
}

fn relative<'a>(base: &Path, path: &'a Path) -> &'a Path {
    path.strip_prefix(base).unwrap_or(path)
}
