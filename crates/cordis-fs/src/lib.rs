//! File-system watch service (`"fs"`). A watch is an effect of the caller's
//! fiber and ends with it.

mod filter;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cordis_core::{Context, Disposal, EffectHandle, Error, IdTable, Plugin, Result, Service, Svc};
use notify::{EventKind, RecursiveMode, Watcher as _};
use parking_lot::Mutex;

pub use filter::{Filter, Predicate, admits, compile};

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

    fn provide() -> Vec<cordis_core::Key> {
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
    /// resolved against the working directory.
    fn watch(&self, path: impl AsRef<Path>, options: WatchOptions) -> Result<Watcher>;
}

impl FsExt for Svc<Fs> {
    fn watch(&self, path: impl AsRef<Path>, options: WatchOptions) -> Result<Watcher> {
        Watcher::start(self.caller(), path.as_ref(), options)
    }
}

type Listener<A> = Arc<dyn Fn(&A) + Send + Sync>;

#[derive(Default)]
struct Hooks {
    change: IdTable<Listener<PathBuf>>,
    ready: IdTable<Listener<()>>,
    error: IdTable<Listener<Error>>,
    ready_seen: bool,
}

/// A running watch. Events are dispatched on the runtime, never on the
/// watcher's own thread.
#[derive(Clone)]
pub struct Watcher {
    hooks: Arc<Mutex<Hooks>>,
    handle: EffectHandle,
    ctx: Context,
}

/// A listener registration; [`Subscription::remove`] removes it.
pub struct Subscription {
    remove: Box<dyn FnOnce() + Send>,
}

impl Subscription {
    pub fn remove(self) {
        (self.remove)()
    }
}

enum Signal {
    Change(PathBuf),
    Error(Error),
}

impl Watcher {
    fn start(ctx: &Context, path: &Path, options: WatchOptions) -> Result<Self> {
        let path = std::path::absolute(path)?;
        // A file replaced through a rename (an atomic write) is lost by a
        // watch on the file itself, so its directory is watched instead.
        let is_file = !path.is_dir();
        let root =
            if is_file { path.parent().map(Path::to_path_buf).unwrap_or_else(|| path.clone()) } else { path.clone() };
        let keep = options.filter.map(|filter| filter.predicate()).transpose()?;
        let hooks: Arc<Mutex<Hooks>> = Default::default();
        let dispatch = hooks.clone();
        let logger = ctx.clone();
        let handle = ctx.effect_sync("ctx.fs.watch()", |scope| {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let target = path.clone();
            let base = root.clone();
            let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| match event {
                Err(error) => drop(tx.send(Signal::Error(Error::new(error)))),
                Ok(event) if is_change(&event.kind) => {
                    for changed in event.paths {
                        let kept = if is_file {
                            changed == target
                        } else {
                            keep.as_ref().is_none_or(|keep| {
                                let relative = changed.strip_prefix(&base).unwrap_or(&changed);
                                let relative = relative.to_string_lossy().replace('\\', "/");
                                admits(keep, &relative, changed.is_dir())
                            })
                        };
                        if kept && !changed.is_dir() {
                            let _ = tx.send(Signal::Change(changed));
                        }
                    }
                }
                Ok(_) => {}
            })?;
            let mode = if is_file { RecursiveMode::NonRecursive } else { RecursiveMode::Recursive };
            watcher.watch(&root, mode)?;
            let task = tokio::spawn(async move {
                while let Some(signal) = rx.recv().await {
                    match signal {
                        Signal::Change(path) => {
                            let listeners: Vec<_> = dispatch.lock().change.values().cloned().collect();
                            for listener in listeners {
                                listener(&path);
                            }
                        }
                        Signal::Error(error) => {
                            let listeners: Vec<_> = dispatch.lock().error.values().cloned().collect();
                            if listeners.is_empty() {
                                logger.logger().warn(&error);
                            }
                            for listener in listeners {
                                listener(&error);
                            }
                        }
                    }
                }
            });
            scope.defer_fn(move || {
                drop(watcher);
                task.abort();
            });
            Ok(())
        })?;
        let watcher = Self { hooks, handle, ctx: ctx.clone() };
        watcher.mark_ready();
        Ok(watcher)
    }

    fn mark_ready(&self) {
        let listeners: Vec<_> = {
            let mut hooks = self.hooks.lock();
            hooks.ready_seen = true;
            hooks.ready.values().cloned().collect()
        };
        for listener in listeners {
            listener(&());
        }
    }

    /// Called with the path of every changed file below the watched path.
    pub fn on_change(&self, listener: impl Fn(&Path) + Send + Sync + 'static) -> Subscription {
        let listener: Listener<PathBuf> = Arc::new(move |path: &PathBuf| listener(path));
        let id = self.hooks.lock().change.insert(listener);
        self.subscription(move |hooks| drop(hooks.change.remove(id)))
    }

    /// Called once the watch is set up (at once if it already is).
    pub fn on_ready(&self, listener: impl Fn() + Send + Sync + 'static) -> Subscription {
        let listener: Listener<()> = Arc::new(move |_: &()| listener());
        let (id, ready) = {
            let mut hooks = self.hooks.lock();
            (hooks.ready.insert(listener.clone()), hooks.ready_seen)
        };
        if ready {
            listener(&());
        }
        self.subscription(move |hooks| drop(hooks.ready.remove(id)))
    }

    /// Without an error listener, errors are logged as warnings.
    pub fn on_error(&self, listener: impl Fn(&Error) + Send + Sync + 'static) -> Subscription {
        let id = self.hooks.lock().error.insert(Arc::new(listener));
        self.subscription(move |hooks| drop(hooks.error.remove(id)))
    }

    pub fn context(&self) -> &Context {
        &self.ctx
    }

    /// Ends the watch before its fiber does.
    pub fn dispose(&self) -> Disposal {
        self.handle.dispose()
    }

    fn subscription(&self, remove: impl FnOnce(&mut Hooks) + Send + 'static) -> Subscription {
        let hooks = Arc::downgrade(&self.hooks);
        Subscription {
            remove: Box::new(move || {
                if let Some(hooks) = hooks.upgrade() {
                    remove(&mut hooks.lock());
                }
            }),
        }
    }
}

fn is_change(kind: &EventKind) -> bool {
    matches!(kind, EventKind::Create(_) | EventKind::Modify(_))
}
