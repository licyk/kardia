//! Timer service whose timeouts and intervals are revertible effects.
//!
//! Every scheduling call is an effect of the *caller*'s fiber (through
//! [`Svc::caller`]), so timers die with the plugin that started them.

use std::sync::Arc;
use std::time::Duration;

use cordis_core::{Context, EffectHandle, Plugin, Result, Service, Svc};
use futures::FutureExt;
use futures::future::BoxFuture;
use parking_lot::Mutex;
use tokio::sync::{Notify, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep};

/// The timer service (`"timer"`).
pub struct Timer;

impl Service for Timer {
    const NAME: &'static str = "timer";
}

/// Provides [`Timer`].
pub struct TimerPlugin;

impl Plugin for TimerPlugin {
    const NAME: &'static str = "timer";
    type Config = ();

    fn provide() -> Vec<cordis_core::Key> {
        vec![Timer::NAME.into()]
    }

    async fn apply(ctx: Context, _: Arc<()>) -> Result<()> {
        ctx.provide(Timer)?;
        Ok(())
    }
}

/// The context that scheduled a timer was disposed before it fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Context has been disposed")]
pub struct Disposed;

pub trait TimerExt {
    /// Calls `callback` once after `delay`, unless the effect is disposed first.
    fn timeout(&self, delay: Duration, callback: impl FnOnce() + Send + 'static) -> Result<EffectHandle>;

    /// Resolves after `delay`; fails with [`Disposed`] if the caller is disposed first.
    fn sleep(&self, delay: Duration) -> BoxFuture<'static, Result<()>>;

    /// Calls `callback` every `delay` until the effect is disposed.
    fn interval(&self, delay: Duration, callback: impl Fn() + Send + Sync + 'static) -> Result<EffectHandle>;

    /// A tick source: [`Ticks::tick`] resolves on the next tick after it is called.
    fn ticks(&self, delay: Duration) -> Result<Ticks>;

    /// Calls `callback` at most once per `delay`; a call inside the window
    /// is postponed to its end (trailing) unless `trailing` is false.
    fn throttle<A: Send + 'static>(
        &self,
        delay: Duration,
        trailing: bool,
        callback: impl Fn(A) + Send + Sync + 'static,
    ) -> Result<Scheduled<A>>;

    /// Calls `callback` once `delay` has passed without another call.
    fn debounce<A: Send + 'static>(
        &self,
        delay: Duration,
        callback: impl Fn(A) + Send + Sync + 'static,
    ) -> Result<Scheduled<A>>;
}

impl TimerExt for Svc<Timer> {
    fn timeout(&self, delay: Duration, callback: impl FnOnce() + Send + 'static) -> Result<EffectHandle> {
        let slot: Arc<Mutex<Option<EffectHandle>>> = Default::default();
        let own = slot.clone();
        let handle = self.caller().effect_sync("ctx.timeout()", |scope| {
            let task = tokio::spawn(async move {
                sleep(delay).await;
                if let Some(handle) = own.lock().take() {
                    // revert the effect first, as the timer is spent
                    drop(handle.dispose());
                }
                callback();
            });
            scope.defer_fn(move || task.abort());
            Ok(())
        })?;
        *slot.lock() = Some(handle.clone());
        Ok(handle)
    }

    fn sleep(&self, delay: Duration) -> BoxFuture<'static, Result<()>> {
        let (tx, rx) = oneshot::channel();
        let tx = Arc::new(Mutex::new(Some(tx)));
        let handle = self.caller().effect_sync("ctx.timeout()", |scope| {
            let resolve = tx.clone();
            let task = tokio::spawn(async move {
                sleep(delay).await;
                if let Some(tx) = resolve.lock().take() {
                    let _ = tx.send(Ok(()));
                }
            });
            scope.defer_fn(move || {
                task.abort();
                if let Some(tx) = tx.lock().take() {
                    let _ = tx.send(Err(Disposed));
                }
            });
            Ok(())
        });
        async move {
            let handle = handle?;
            let result = rx.await.unwrap_or(Err(Disposed));
            drop(handle.dispose());
            Ok(result?)
        }
        .boxed()
    }

    fn interval(&self, delay: Duration, callback: impl Fn() + Send + Sync + 'static) -> Result<EffectHandle> {
        self.caller().effect_sync("ctx.interval()", |scope| {
            let task = tokio::spawn(async move {
                let mut interval = tokio::time::interval_at(Instant::now() + delay, delay);
                loop {
                    interval.tick().await;
                    callback();
                }
            });
            scope.defer_fn(move || task.abort());
            Ok(())
        })
    }

    fn ticks(&self, delay: Duration) -> Result<Ticks> {
        let shared = Arc::new(TickState { notify: Notify::new(), closed: Mutex::new(false) });
        let state = shared.clone();
        let handle = self.caller().effect_sync("ctx.interval()", |scope| {
            let ticker = state.clone();
            let task = tokio::spawn(async move {
                let mut interval = tokio::time::interval_at(Instant::now() + delay, delay);
                loop {
                    interval.tick().await;
                    // only readers already waiting receive the tick
                    ticker.notify.notify_waiters();
                }
            });
            scope.defer_fn(move || {
                task.abort();
                *state.closed.lock() = true;
                state.notify.notify_waiters();
            });
            Ok(())
        })?;
        Ok(Ticks { state: shared, handle })
    }

    fn throttle<A: Send + 'static>(
        &self,
        delay: Duration,
        trailing: bool,
        callback: impl Fn(A) + Send + Sync + 'static,
    ) -> Result<Scheduled<A>> {
        let callback = Arc::new(callback);
        let last_call: Arc<Mutex<Option<Instant>>> = Default::default();
        schedule(self.caller(), "ctx.throttle()", move |args| {
            let execute = {
                let (callback, last_call) = (callback.clone(), last_call.clone());
                move |args| {
                    *last_call.lock() = Some(Instant::now());
                    callback(args);
                }
            };
            let elapsed = last_call.lock().map(|at| at.elapsed());
            match elapsed {
                Some(elapsed) if elapsed < delay => trailing.then(|| {
                    tokio::spawn(async move {
                        sleep(delay - elapsed).await;
                        execute(args);
                    })
                }),
                _ => {
                    execute(args);
                    None
                }
            }
        })
    }

    fn debounce<A: Send + 'static>(
        &self,
        delay: Duration,
        callback: impl Fn(A) + Send + Sync + 'static,
    ) -> Result<Scheduled<A>> {
        let callback = Arc::new(callback);
        schedule(self.caller(), "ctx.debounce()", move |args| {
            let callback = callback.clone();
            Some(tokio::spawn(async move {
                sleep(delay).await;
                callback(args);
            }))
        })
    }
}

struct TickState {
    notify: Notify,
    closed: Mutex<bool>,
}

/// Ticks of an interval. Dropping it disposes the interval.
pub struct Ticks {
    state: Arc<TickState>,
    handle: EffectHandle,
}

impl Ticks {
    /// Waits for the next tick; fails with [`Disposed`] once the interval
    /// (or the context that started it) has been disposed.
    pub async fn tick(&mut self) -> Result<(), Disposed> {
        let notified = self.state.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if *self.state.closed.lock() {
            return Err(Disposed);
        }
        notified.await;
        if *self.state.closed.lock() { Err(Disposed) } else { Ok(()) }
    }

    pub fn close(self) {}
}

impl Drop for Ticks {
    fn drop(&mut self) {
        drop(self.handle.dispose());
    }
}

type Trigger<A> = Box<dyn Fn(A) -> Option<JoinHandle<()>> + Send + Sync>;

struct ScheduleState {
    disposed: bool,
    timer: Option<JoinHandle<()>>,
}

/// A rate-limited callback. Calls after disposal are ignored.
pub struct Scheduled<A> {
    state: Arc<Mutex<ScheduleState>>,
    trigger: Arc<Trigger<A>>,
    handle: EffectHandle,
}

impl<A> Clone for Scheduled<A> {
    fn clone(&self) -> Self {
        Self { state: self.state.clone(), trigger: self.trigger.clone(), handle: self.handle.clone() }
    }
}

impl<A> Scheduled<A> {
    pub fn call(&self, args: A) {
        let mut state = self.state.lock();
        if state.disposed {
            return;
        }
        if let Some(timer) = state.timer.take() {
            timer.abort();
        }
        drop(state);
        let timer = (self.trigger)(args);
        let mut state = self.state.lock();
        if state.disposed {
            if let Some(timer) = timer {
                timer.abort();
            }
        } else {
            state.timer = timer;
        }
    }

    pub fn dispose(&self) -> cordis_core::Disposal {
        self.handle.dispose()
    }
}

fn schedule<A>(
    ctx: &Context,
    label: &str,
    trigger: impl Fn(A) -> Option<JoinHandle<()>> + Send + Sync + 'static,
) -> Result<Scheduled<A>> {
    let state = Arc::new(Mutex::new(ScheduleState { disposed: false, timer: None }));
    let own = state.clone();
    let handle = ctx.effect_sync(label, |scope| {
        scope.defer_fn(move || {
            let mut state = own.lock();
            state.disposed = true;
            if let Some(timer) = state.timer.take() {
                timer.abort();
            }
        });
        Ok(())
    })?;
    Ok(Scheduled { state, trigger: Arc::new(Box::new(trigger)), handle })
}

/// Convenience access to the timer service from a context that declares it.
pub trait TimerContext {
    fn timer(&self) -> std::result::Result<Svc<Timer>, cordis_core::AccessError>;
}

impl TimerContext for Context {
    fn timer(&self) -> std::result::Result<Svc<Timer>, cordis_core::AccessError> {
        self.get::<Timer>()
    }
}
