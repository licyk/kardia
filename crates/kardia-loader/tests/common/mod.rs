#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use kardia_core::{Context, EffectHandle, Fiber, PluginDef};
use kardia_loader::{Catalog, EntryChange, Loader, Resolver, TreeBackend, builtins};
use parking_lot::Mutex;
use serde_json::Value;

/// The analogue of `MockLoader`: a loader whose tree records its changes
/// and whose plugins come from a mutable catalog.
pub struct Mock {
    pub root: Context,
    pub loader: Loader,
    pub catalog: Catalog,
    pub changes: Arc<Mutex<Vec<EntryChange>>>,
    /// An artificial delay for every resolution, in milliseconds.
    pub delay: Arc<AtomicUsize>,
    /// Holds the resolution of one name until released.
    pub hold: Arc<Mutex<Option<Hold>>>,
}

/// Holds the resolution of `name`: `entered` opens when it starts,
/// it continues once `release` opens, and then fails if `fail` is set.
/// The first `skip` resolutions of `name` pass through.
#[derive(Clone)]
pub struct Hold {
    pub name: String,
    pub entered: Gate,
    pub release: Gate,
    pub fail: bool,
    pub skip: Arc<Mutex<usize>>,
}

struct Recorder(Arc<Mutex<Vec<EntryChange>>>);

impl TreeBackend for Recorder {
    fn commit(&self, change: &EntryChange) {
        self.0.lock().push(change.clone());
    }
}

struct Delayed {
    catalog: Catalog,
    delay: Arc<AtomicUsize>,
    hold: Arc<Mutex<Option<Hold>>>,
}

impl Resolver for Delayed {
    fn resolve(&self, name: &str) -> BoxFuture<'static, kardia_core::Result<PluginDef>> {
        let delay = self.delay.load(Ordering::SeqCst) as u64;
        let hold = self.hold.lock().clone().filter(|hold| {
            let mut skip = hold.skip.lock();
            let held = hold.name == name && *skip == 0;
            if hold.name == name && *skip > 0 {
                *skip -= 1;
            }
            held
        });
        let resolved = self.catalog.resolve(name);
        let name = name.to_string();
        async move {
            if let Some(hold) = hold {
                hold.entered.open();
                hold.release.wait().await;
                if hold.fail {
                    return Err(kardia_core::Error::msg(format!("cannot find plugin {name}")));
                }
            }
            if delay > 0 {
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            resolved.await
        }
        .boxed()
    }
}

impl Mock {
    pub async fn boot() -> Self {
        let root = Context::new();
        let catalog = builtins();
        let changes = Arc::new(Mutex::new(Vec::new()));
        let delay = Arc::new(AtomicUsize::new(0));
        let hold = Arc::new(Mutex::new(None));
        let resolver = Arc::new(Delayed { catalog: catalog.clone(), delay: delay.clone(), hold: hold.clone() });
        let def = Loader::plugin_with(resolver, Arc::new(Recorder(changes.clone())));
        root.plugin_def(&def, Value::Null).unwrap().settled().await.unwrap();
        let loader = (*root.get::<Loader>().unwrap()).clone();
        Self { root, loader, catalog, changes, delay, hold }
    }

    /// Holds the next resolutions of `name`; returns the `entered` and `release` gates.
    pub fn hold(&self, name: &str, fail: bool) -> (Gate, Gate) {
        self.hold_after(name, 0, fail)
    }

    /// Like [`Mock::hold`], letting the first `skip` resolutions through.
    pub fn hold_after(&self, name: &str, skip: usize, fail: bool) -> (Gate, Gate) {
        let (entered, release) = (Gate::new(), Gate::new());
        let skip = Arc::new(Mutex::new(skip));
        *self.hold.lock() =
            Some(Hold { name: name.into(), entered: entered.clone(), release: release.clone(), fail, skip });
        (entered, release)
    }

    /// Holds the context patch (`loader/patch-context`) of the entries whose
    /// options `select` picks, before or `after` the rest of the patch;
    /// returns the `entered` and `release` gates and the hook.
    pub fn hold_patch(
        &self,
        after: bool,
        select: impl Fn(&Value) -> bool + Send + Sync + 'static,
    ) -> (Gate, Gate, EffectHandle) {
        let (entered, release) = (Gate::new(), Gate::new());
        let (e, r) = (entered.clone(), release.clone());
        let hook = self
            .loader
            .on_patch_context(&self.root, move |entry, next| {
                let (entered, release, held) = (e.clone(), r.clone(), select(&entry.options()));
                async move {
                    if held && !after {
                        entered.open();
                        release.wait().await;
                    }
                    next.await?;
                    if held && after {
                        entered.open();
                        release.wait().await;
                    }
                    Ok(())
                }
            })
            .unwrap();
        (entered, release, hook)
    }

    pub fn mock(&self, name: &str, def: PluginDef) -> PluginDef {
        let def = def.name(name);
        self.catalog.register_def(name, def.clone());
        def
    }

    pub fn data(&self) -> Vec<Value> {
        self.loader.root().data()
    }

    pub fn take_changes(&self) -> Vec<EntryChange> {
        std::mem::take(&mut self.changes.lock())
    }

    pub async fn read(&self, data: Value) {
        self.loader.root().update(data.as_array().cloned().unwrap_or_default()).await;
        self.loader.settled().await;
    }

    pub fn fiber(&self, id: &str) -> Fiber {
        self.loader.resolve(id).unwrap().inner_fiber().expect("entry has an inner fiber")
    }

    pub fn enabled(&self, def: &PluginDef) -> bool {
        self.root.registry().has(def)
    }
}

/// A plugin counting its loads and the disposals of its loads.
#[derive(Clone, Default)]
pub struct Counter {
    pub calls: Arc<AtomicUsize>,
    pub disposed: Arc<AtomicUsize>,
    pub configs: Arc<Mutex<Vec<Value>>>,
}

impl Counter {
    pub fn def(&self) -> PluginDef {
        let this = self.clone();
        PluginDef::new(move |ctx, config| {
            let this = this.clone();
            this.calls.fetch_add(1, Ordering::SeqCst);
            this.configs.lock().push(config);
            async move {
                let disposed = this.disposed.clone();
                ctx.on_dispose(move || {
                    disposed.fetch_add(1, Ordering::SeqCst);
                })?;
                Ok(())
            }
        })
    }

    /// Loads and disposals since the last call.
    pub fn take(&self) -> (usize, usize) {
        (self.calls.swap(0, Ordering::SeqCst), self.disposed.swap(0, Ordering::SeqCst))
    }
}

pub async fn settle() {
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
}

/// A one-shot gate.
#[derive(Clone)]
pub struct Gate(tokio::sync::watch::Sender<bool>);

impl Gate {
    pub fn new() -> Self {
        Self(tokio::sync::watch::channel(false).0)
    }

    pub fn open(&self) {
        self.0.send_replace(true);
    }

    pub async fn wait(&self) {
        let mut rx = self.0.subscribe();
        let _ = rx.wait_for(|open| *open).await;
    }
}
