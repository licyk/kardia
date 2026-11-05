//! Rate-limited callbacks, the common ground of throttle and debounce.

use std::sync::Arc;

use kardia_core::{Context, EffectHandle, Result};
use parking_lot::Mutex;
use tokio::task::JoinHandle;

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

    pub fn dispose(&self) -> kardia_core::Disposal {
        self.handle.dispose()
    }
}

pub(crate) fn schedule<A>(
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
