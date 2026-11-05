//! A plugin counting its loads and disposals.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use kardia_core::PluginDef;
use parking_lot::Mutex;
use serde_json::Value;

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
