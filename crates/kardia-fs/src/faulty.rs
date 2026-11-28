use std::sync::Arc;

use futures::FutureExt;
use futures::future::BoxFuture;
use kardia_core::Result;
use url::Url;

use crate::{Capabilities, FileStream, FsProvider, Stats};

/// How a [`Faulty`] provider injects failures.
pub trait Fault: Send + Sync + 'static {
    /// Called before a rename; `Some` fails it with that error instead.
    fn before_rename(&self) -> Option<kardia_core::Error>;
}

/// A provider that delegates to another and lets a test fail its renames, so
/// the retry of a write-back is exercised against the real provider.
pub struct Faulty<P> {
    inner: Arc<P>,
    fault: Arc<dyn Fault>,
}

impl<P> Faulty<P> {
    pub fn new(inner: Arc<P>, fault: impl Fault) -> Self {
        Self { inner, fault: Arc::new(fault) }
    }
}

impl<P: FsProvider> FsProvider for Faulty<P> {
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    fn read(&self, url: Url) -> FileStream {
        self.inner.read(url)
    }

    fn write(&self, url: Url, data: Vec<u8>) -> BoxFuture<'static, Result<()>> {
        self.inner.write(url, data)
    }

    fn remove(&self, url: Url) -> BoxFuture<'static, Result<()>> {
        self.inner.remove(url)
    }

    fn stat(&self, url: Url) -> BoxFuture<'static, Result<Stats>> {
        self.inner.stat(url)
    }

    fn rename(&self, from: Url, to: Url) -> BoxFuture<'static, Result<()>> {
        match self.fault.before_rename() {
            Some(error) => futures::future::ready(Err(error)).boxed(),
            None => self.inner.rename(from, to),
        }
    }
}
