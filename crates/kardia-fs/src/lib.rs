//! File-system watch service (`"fs"`). A watch is an effect of the caller's
//! fiber and ends with it.

mod filter;

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use kardia_core::{Context, Disposal, EffectHandle, Error, IdTable, Plugin, Result, Service, Svc};
use notify::{EventKind, RecursiveMode, Watcher as _};
use parking_lot::Mutex;
use tokio::sync::mpsc::UnboundedSender;

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

type Listener<A> = Arc<dyn Fn(&A) + Send + Sync>;

#[derive(Default)]
struct Hooks {
    change: IdTable<Listener<PathBuf>>,
    ready: IdTable<Listener<()>>,
    error: IdTable<Listener<Error>>,
}

/// A running watch. Events are dispatched in order on the runtime, never on
/// the watcher's own thread or in the call that adds a listener.
#[derive(Clone)]
pub struct Watcher {
    hooks: Arc<Mutex<Hooks>>,
    signals: UnboundedSender<Signal>,
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
    /// A symbolic link to a directory was added below a watched directory.
    Link(PathBuf),
    Ready(u64),
    Error(Error),
}

impl Watcher {
    fn start(ctx: &Context, path: &Path, options: WatchOptions) -> Result<Self> {
        let path = normalize(path)?;
        // A file replaced through a rename (an atomic write) is lost by a
        // watch on the file itself, so its directory is watched instead.
        let is_file = !path.is_dir();
        let root =
            if is_file { path.parent().map(Path::to_path_buf).unwrap_or_else(|| path.clone()) } else { path.clone() };
        let keep = options.filter.map(|filter| filter.predicate()).transpose()?;
        let hooks: Arc<Mutex<Hooks>> = Default::default();
        let dispatch = hooks.clone();
        let logger = ctx.clone();
        let (signals, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let tx = signals.clone();
        let handle = ctx.effect_sync("ctx.fs.watch()", |scope| {
            let target = path.clone();
            let base = root.clone();
            let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| match event {
                Err(error) => drop(tx.send(Signal::Error(Error::new(error)))),
                Ok(event) if is_change(&event.kind) => {
                    for changed in event.paths {
                        // the ancestors of a file are tested as directories by `admits`
                        if changed.is_dir() {
                            // notify descends only into new entries that are directories themselves
                            if !is_file && changed.is_symlink() {
                                let _ = tx.send(Signal::Link(changed));
                            }
                            continue;
                        }
                        let kept = if is_file {
                            changed == target
                        } else {
                            keep.as_ref().is_none_or(|keep| {
                                let relative = changed.strip_prefix(&base).unwrap_or(&changed);
                                let relative = relative.to_string_lossy().replace('\\', "/");
                                admits(keep, &relative, false)
                            })
                        };
                        if kept {
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
                        Signal::Change(path) => emit(&dispatch, |hooks| &hooks.change, &path),
                        // fails only if the link is gone already
                        Signal::Link(path) => drop(watcher.watch(&path, RecursiveMode::Recursive)),
                        Signal::Ready(id) => {
                            let listener = dispatch.lock().ready.get(id).cloned();
                            if let Some(listener) = listener {
                                listener(&());
                            }
                        }
                        Signal::Error(error) => {
                            if dispatch.lock().error.is_empty() {
                                logger.logger().warn(&error);
                            }
                            emit(&dispatch, |hooks| &hooks.error, &error);
                        }
                    }
                }
            });
            // the notify watcher ends with the task
            scope.defer_fn(move || task.abort());
            Ok(())
        })?;
        Ok(Self { hooks, signals, handle, ctx: ctx.clone() })
    }

    /// Called with the path of every changed file below the watched path.
    pub fn on_change(&self, listener: impl Fn(&Path) + Send + Sync + 'static) -> Subscription {
        let listener: Listener<PathBuf> = Arc::new(move |path: &PathBuf| listener(path));
        let id = self.hooks.lock().change.insert(listener);
        self.subscription(move |hooks| drop(hooks.change.remove(id)))
    }

    /// Called once, after the watch is set up, which it is once `watch()`
    /// returns.
    pub fn on_ready(&self, listener: impl Fn() + Send + Sync + 'static) -> Subscription {
        let listener: Listener<()> = Arc::new(move |_: &()| listener());
        let id = self.hooks.lock().ready.insert(listener);
        // fails only once the watch has ended
        let _ = self.signals.send(Signal::Ready(id));
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

/// Calls the listeners present when the dispatch starts, except those removed
/// by an earlier one meanwhile.
fn emit<A>(hooks: &Mutex<Hooks>, table: fn(&Hooks) -> &IdTable<Listener<A>>, arg: &A) {
    let ids: Vec<u64> = table(&hooks.lock()).iter().map(|(id, _)| id).collect();
    for id in ids {
        let listener = table(&hooks.lock()).get(id).cloned();
        if let Some(listener) = listener {
            listener(arg);
        }
    }
}

/// Like `path.resolve()`: absolute, without `.`, `..` or a trailing separator.
fn normalize(path: &Path) -> Result<PathBuf> {
    let mut normal = PathBuf::new();
    for component in std::path::absolute(path)?.components() {
        match component {
            Component::ParentDir => {
                normal.pop();
            }
            component => normal.push(component),
        }
    }
    Ok(normal)
}

fn is_change(kind: &EventKind) -> bool {
    matches!(kind, EventKind::Create(_) | EventKind::Modify(_))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use kardia_core::Level;

    use super::*;

    /// Sends the errors that the notify watcher reports, which a test cannot provoke.
    async fn watcher() -> (Context, Watcher) {
        let root = Context::new();
        root.plugin::<FsPlugin>(()).unwrap().settled().await.unwrap();
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let watcher = root.get::<Fs>().unwrap().watch(path, WatchOptions::default()).unwrap();
        (root, watcher)
    }

    fn warnings(ctx: &Context) -> Vec<String> {
        ctx.log_buffer()
            .into_iter()
            .filter(|message| message.level == Level::Warn)
            .map(|message| message.text)
            .collect()
    }

    async fn eventually(check: impl Fn() -> bool) {
        for _ in 0..250 {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("timed out");
    }

    #[tokio::test]
    async fn logs_an_error_of_the_watcher_without_an_error_listener() {
        let (ctx, watcher) = watcher().await;
        watcher.signals.send(Signal::Error(Error::msg("EPERM"))).unwrap();
        eventually(|| warnings(&ctx).iter().any(|text| text.contains("EPERM"))).await;
    }

    #[tokio::test]
    async fn passes_an_error_of_the_watcher_to_the_error_listeners_instead_of_logging_it() {
        let (ctx, watcher) = watcher().await;
        let errors = Arc::new(Mutex::new(Vec::new()));
        let e = errors.clone();
        watcher.on_error(move |error| e.lock().push(error.to_string()));

        watcher.signals.send(Signal::Error(Error::msg("EPERM"))).unwrap();
        eventually(|| *errors.lock() == ["EPERM"]).await;
        assert!(warnings(&ctx).iter().all(|text| !text.contains("EPERM")));
    }

    #[tokio::test]
    async fn does_not_call_an_error_listener_removed_by_another_one_in_the_same_dispatch() {
        let (_ctx, watcher) = watcher().await;
        let removal: Arc<Mutex<Option<Subscription>>> = Default::default();
        let r = removal.clone();
        watcher.on_error(move |_| {
            if let Some(subscription) = r.lock().take() {
                subscription.remove();
            }
        });
        let errors = Arc::new(Mutex::new(Vec::new()));
        let e = errors.clone();
        *removal.lock() = Some(watcher.on_error(move |error| e.lock().push(error.to_string())));
        let others = Arc::new(Mutex::new(0));
        let o = others.clone();
        watcher.on_error(move |_| *o.lock() += 1);

        watcher.signals.send(Signal::Error(Error::msg("EPERM"))).unwrap();
        eventually(|| *others.lock() == 1).await;
        assert!(errors.lock().is_empty());
    }
}
