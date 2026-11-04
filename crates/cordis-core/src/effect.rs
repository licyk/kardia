use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::task::{Context as TaskContext, Poll};

use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use parking_lot::Mutex;
use tokio::task::JoinHandle;

use crate::context::Context;
use crate::error::{Error, Interrupted, Result};
use crate::key::FiberId;
use crate::root::RootInner;

pub(crate) enum DisposeResult {
    Done(Result<()>),
    Pending(BoxFuture<'static, Result<()>>),
}

/// The inverse of an effect. Runs at most once (it is consumed).
///
/// The closure passed to the constructors runs synchronously when disposal
/// starts; an asynchronous disposer may return a future that finishes the
/// work. Panics are caught and reported as errors.
pub struct Disposer(Box<dyn FnOnce() -> DisposeResult + Send>);

impl Disposer {
    pub fn new(f: impl FnOnce() + Send + 'static) -> Self {
        Self(Box::new(move || {
            f();
            DisposeResult::Done(Ok(()))
        }))
    }

    pub fn fallible(f: impl FnOnce() -> Result<()> + Send + 'static) -> Self {
        Self(Box::new(move || DisposeResult::Done(f())))
    }

    pub fn future<F, Fut>(f: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        Self(Box::new(move || DisposeResult::Pending(f().map(Ok).boxed())))
    }

    pub fn try_future<F, Fut>(f: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        Self(Box::new(move || DisposeResult::Pending(f().boxed())))
    }

    pub(crate) fn from_handle(handle: EffectHandle) -> Self {
        Self(Box::new(move || handle.dispose().into_result()))
    }

    pub(crate) fn call(self) -> DisposeResult {
        match catch_unwind(AssertUnwindSafe(self.0)) {
            Ok(DisposeResult::Pending(future)) => DisposeResult::Pending(
                AssertUnwindSafe(future)
                    .catch_unwind()
                    .map(|result| result.unwrap_or_else(|panic| Err(Error::from_panic(panic))))
                    .boxed(),
            ),
            Ok(done) => done,
            Err(panic) => DisposeResult::Done(Err(Error::from_panic(panic))),
        }
    }
}

/// The completion of a disposal. Disposal makes progress without being
/// polled: the synchronous part has already run, and any asynchronous rest
/// runs as a spawned task, so dropping a `Disposal` does not cancel it.
#[must_use = "a disposal completes on its own; await it to observe the result"]
pub struct Disposal(DisposalInner);

enum DisposalInner {
    Ready(Option<Result<()>>),
    Task(JoinHandle<Result<()>>),
}

impl Disposal {
    pub(crate) fn ready(result: Result<()>) -> Self {
        Self(DisposalInner::Ready(Some(result)))
    }

    pub(crate) fn spawn(future: impl Future<Output = Result<()>> + Send + 'static) -> Self {
        Self(DisposalInner::Task(tokio::spawn(future)))
    }

    pub(crate) fn into_result(self) -> DisposeResult {
        match self.0 {
            DisposalInner::Ready(result) => DisposeResult::Done(result.unwrap_or(Ok(()))),
            DisposalInner::Task(task) => DisposeResult::Pending(join(task).boxed()),
        }
    }

    /// Whether everything finished synchronously.
    pub fn is_ready(&self) -> bool {
        matches!(self.0, DisposalInner::Ready(_))
    }
}

impl Future for Disposal {
    type Output = Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Result<()>> {
        match &mut self.0 {
            DisposalInner::Ready(result) => Poll::Ready(result.take().unwrap_or(Ok(()))),
            DisposalInner::Task(task) => task.poll_unpin(cx).map(join_result),
        }
    }
}

async fn join(task: JoinHandle<Result<()>>) -> Result<()> {
    join_result(task.await)
}

fn join_result(result: std::result::Result<Result<()>, tokio::task::JoinError>) -> Result<()> {
    match result {
        Ok(result) => result,
        Err(error) if error.is_panic() => Err(Error::from_panic(error.into_panic())),
        Err(error) => Err(Error::new(error)),
    }
}

/// Runs disposers newest first, one after another. Synchronous disposers run
/// immediately; from the first asynchronous one on, the rest is spawned.
/// Every disposer runs even if an earlier one fails; the first error wins.
pub(crate) fn run_sequence(disposers: Vec<Disposer>) -> Disposal {
    let mut rest = disposers;
    let mut first_error = None;
    while let Some(disposer) = rest.pop() {
        match disposer.call() {
            DisposeResult::Done(result) => record(&mut first_error, result),
            DisposeResult::Pending(future) => {
                return Disposal::spawn(async move {
                    record(&mut first_error, future.await);
                    while let Some(disposer) = rest.pop() {
                        let result = match disposer.call() {
                            DisposeResult::Done(result) => result,
                            DisposeResult::Pending(future) => future.await,
                        };
                        record(&mut first_error, result);
                    }
                    first_error.map_or(Ok(()), Err)
                });
            }
        }
    }
    Disposal::ready(first_error.map_or(Ok(()), Err))
}

fn record(first_error: &mut Option<Error>, result: Result<()>) {
    if let Err(error) = result {
        first_error.get_or_insert(error);
    }
}

type SetupFuture = Shared<BoxFuture<'static, Result<()>>>;

/// One effect: its collected inverses, nested effects and (for asynchronous
/// effects) the in-flight setup.
pub(crate) struct EffectCell {
    pub(crate) label: String,
    pub(crate) fiber: FiberId,
    inner: Mutex<CellInner>,
}

struct CellInner {
    active: bool,
    /// Disposers were taken for the final run; late ones run immediately.
    closed: bool,
    /// Slot in the owning fiber's accumulator, while registered there.
    entry: Option<u64>,
    disposers: Vec<Disposer>,
    children: Vec<Arc<EffectCell>>,
    setup: Option<SetupFuture>,
}

impl EffectCell {
    pub(crate) fn new(label: impl Into<String>, fiber: FiberId) -> Arc<Self> {
        Arc::new(Self {
            label: label.into(),
            fiber,
            inner: Mutex::new(CellInner {
                active: true,
                closed: false,
                entry: None,
                disposers: Vec::new(),
                children: Vec::new(),
                setup: None,
            }),
        })
    }

    pub(crate) fn with_disposer(label: impl Into<String>, fiber: FiberId, disposer: Disposer) -> Arc<Self> {
        let cell = Self::new(label, fiber);
        cell.inner.lock().disposers.push(disposer);
        cell
    }

    pub(crate) fn set_entry(&self, entry: Option<u64>) {
        self.inner.lock().entry = entry;
    }

    pub(crate) fn take_entry(&self) -> Option<u64> {
        self.inner.lock().entry.take()
    }

    pub(crate) fn is_active(&self) -> bool {
        self.inner.lock().active
    }

    fn push(&self, disposer: Disposer) {
        let mut inner = self.inner.lock();
        if inner.closed {
            drop(inner);
            let _ = run_sequence(vec![disposer]);
        } else {
            inner.disposers.push(disposer);
        }
    }

    fn take_disposers(&self, close: bool) -> Vec<Disposer> {
        let mut inner = self.inner.lock();
        inner.closed |= close;
        std::mem::take(&mut inner.disposers)
    }

    pub(crate) fn meta(&self) -> EffectMeta {
        let children = self.inner.lock().children.clone();
        EffectMeta {
            label: self.label.clone(),
            children: children.iter().map(|child| child.meta()).collect(),
        }
    }

    /// Starts disposal at most once; waits for an in-flight setup first.
    pub(crate) fn dispose(self: &Arc<Self>, root: Option<&Arc<RootInner>>) -> Disposal {
        let (entry, setup) = {
            let mut inner = self.inner.lock();
            if !inner.active {
                return Disposal::ready(Ok(()));
            }
            inner.active = false;
            (inner.entry.take(), inner.setup.clone())
        };
        if let (Some(entry), Some(root)) = (entry, root) {
            let fiber = self.fiber;
            let removed = root.run(|st| st.detach_effect(fiber, entry));
            drop(removed);
        }
        match setup {
            Some(setup) => {
                let cell = self.clone();
                Disposal::spawn(async move {
                    let _ = setup.await;
                    run_sequence(cell.take_disposers(true)).await
                })
            }
            None => run_sequence(self.take_disposers(true)),
        }
    }
}

/// Introspection of an effect: its label and the effects it adopted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectMeta {
    pub label: String,
    pub children: Vec<EffectMeta>,
}

impl EffectMeta {
    pub fn new(label: impl Into<String>, children: Vec<EffectMeta>) -> Self {
        Self {
            label: label.into(),
            children,
        }
    }
}

/// A non-owning handle to an effect. Dropping it does not dispose the effect:
/// its lifetime is governed by the fiber it was registered on.
#[derive(Clone)]
pub struct EffectHandle {
    root: Weak<RootInner>,
    cell: Arc<EffectCell>,
}

impl EffectHandle {
    pub(crate) fn new(root: &Arc<RootInner>, cell: Arc<EffectCell>) -> Self {
        Self {
            root: Arc::downgrade(root),
            cell,
        }
    }

    pub fn label(&self) -> &str {
        &self.cell.label
    }

    pub fn is_active(&self) -> bool {
        self.cell.is_active()
    }

    /// Reverts the effect (at most once). Inverses run newest first.
    pub fn dispose(&self) -> Disposal {
        self.cell.dispose(self.root.upgrade().as_ref())
    }

    /// Resolves when the setup has finished (immediately for synchronous
    /// effects), with the setup's failure if it failed.
    pub fn ready(&self) -> impl Future<Output = Result<()>> + Send + 'static {
        let setup = self.cell.inner.lock().setup.clone();
        async move {
            match setup {
                Some(setup) => setup.await,
                None => Ok(()),
            }
        }
    }

    pub fn meta(&self) -> EffectMeta {
        self.cell.meta()
    }
}

/// The collector handed to an effect body.
///
/// Inverses collected here are disposed together, newest first. Iteration
/// boundaries ([`EffectScope::checkpoint`], [`EffectScope::yield_`]) report
/// [`Interrupted`] once the effect has been disposed; propagating it with `?`
/// ends the body without failure.
#[derive(Clone)]
pub struct EffectScope {
    root: Weak<RootInner>,
    cell: Arc<EffectCell>,
}

impl EffectScope {
    /// Collects an inverse without an iteration boundary.
    pub fn defer(&self, disposer: Disposer) {
        self.cell.push(disposer);
    }

    /// Shorthand for deferring a synchronous inverse.
    pub fn defer_fn(&self, f: impl FnOnce() + Send + 'static) {
        self.defer(Disposer::new(f));
    }

    /// Moves a nested effect under this one: it is no longer disposed by its
    /// fiber directly, but together with this effect.
    pub fn adopt(&self, handle: impl Into<EffectHandle>) {
        let handle = handle.into();
        if let (Some(root), Some(entry)) = (self.root.upgrade(), handle.cell.take_entry()) {
            let fiber = handle.cell.fiber;
            let removed = root.run(|st| st.detach_effect(fiber, entry));
            drop(removed);
        }
        self.cell.inner.lock().children.push(handle.cell.clone());
        self.cell.push(Disposer::from_handle(handle));
    }

    pub fn checkpoint(&self) -> Result<(), Interrupted> {
        if self.cell.is_active() {
            Ok(())
        } else {
            Err(Interrupted)
        }
    }

    /// Collects an inverse and marks an iteration boundary.
    pub fn yield_(&self, disposer: Disposer) -> Result<(), Interrupted> {
        self.defer(disposer);
        self.checkpoint()
    }
}

impl Context {
    /// Runs a synchronous effect body. If it fails, whatever it collected is
    /// disposed and the error is returned.
    pub fn effect_sync<F>(&self, label: impl Into<String>, body: F) -> Result<EffectHandle>
    where
        F: FnOnce(&EffectScope) -> Result<()>,
    {
        self.root.run(|st| st.assert_active(self.fiber))?;
        let cell = EffectCell::new(label, self.fiber);
        let scope = self.effect_scope(&cell);
        let result =
            catch_unwind(AssertUnwindSafe(|| body(&scope))).unwrap_or_else(|panic| Err(Error::from_panic(panic)));
        if let Err(error) = result {
            let _ = run_sequence(cell.take_disposers(true));
            return Err(error);
        }
        Ok(self.attach(cell))
    }

    /// Starts an asynchronous effect. The body is spawned and never dropped
    /// half-way; disposal waits for it to reach its end or an iteration
    /// boundary. A failure disposes what it collected, is logged, and is
    /// reported by [`EffectHandle::ready`].
    pub fn effect<F, Fut>(&self, label: impl Into<String>, body: F) -> Result<EffectHandle>
    where
        F: FnOnce(EffectScope) -> Fut,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        self.root.run(|st| st.assert_active(self.fiber))?;
        let cell = EffectCell::new(label, self.fiber);
        let future = body(self.effect_scope(&cell));
        let task_cell = cell.clone();
        let logger = self.clone();
        let task = tokio::spawn(async move {
            let result = AssertUnwindSafe(future)
                .catch_unwind()
                .await
                .unwrap_or_else(|panic| Err(Error::from_panic(panic)));
            match result {
                Err(error) if !error.is_interrupted() => {
                    if let Err(cleanup) = run_sequence(task_cell.take_disposers(false)).await {
                        logger.logger().log_error(&cleanup);
                    }
                    logger.logger().log_error(&error);
                    Err(error)
                }
                _ => Ok(()),
            }
        });
        cell.inner.lock().setup = Some(join(task).boxed().shared());
        Ok(self.attach(cell))
    }

    /// Registers a plain inverse on the current fiber.
    pub fn on_dispose(&self, f: impl FnOnce() + Send + 'static) -> Result<EffectHandle> {
        self.effect_sync("ctx.on_dispose()", |scope| {
            scope.defer_fn(f);
            Ok(())
        })
    }

    /// Registers an arbitrary inverse on the current fiber.
    pub fn defer(&self, label: impl Into<String>, disposer: Disposer) -> Result<EffectHandle> {
        self.effect_sync(label, |scope| {
            scope.defer(disposer);
            Ok(())
        })
    }

    fn effect_scope(&self, cell: &Arc<EffectCell>) -> EffectScope {
        EffectScope {
            root: Arc::downgrade(&self.root),
            cell: cell.clone(),
        }
    }

    fn attach(&self, cell: Arc<EffectCell>) -> EffectHandle {
        self.root.run(|st| st.attach_effect(self.fiber, &cell));
        EffectHandle::new(&self.root, cell)
    }
}
