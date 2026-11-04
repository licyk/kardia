use std::any::Any;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::Notify;

use crate::error::Error;
use crate::events::Emit;
use crate::key::FiberId;
use crate::logger::{Level, LoggerState};
use crate::state::State;

/// Shared state of one root context.
///
/// All mutable runtime state lives behind one lock. Every synchronous segment
/// of the reference implementation is one critical section: nothing awaits
/// while holding the lock and no user code runs under it. Critical sections
/// queue their follow-up work as [`Action`]s, which run after unlocking.
pub(crate) struct RootInner {
    state: Mutex<State>,
    changed: Notify,
    pub(crate) logger: Mutex<LoggerState>,
}

pub(crate) enum Action {
    Reload(FiberId),
    Unload(FiberId),
    Emit(Emit),
    Log {
        name: String,
        fiber: FiberId,
        level: Level,
        error: Error,
    },
    /// Values released under the lock are dropped after it, since their
    /// destructors are user code.
    Drop(#[allow(dead_code)] Box<dyn Any + Send>),
}

impl RootInner {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new_cyclic(|root| Self {
            state: Mutex::new(State::new(root.clone())),
            changed: Notify::new(),
            logger: Mutex::new(LoggerState::default()),
        })
    }

    /// Runs one critical section, then wakes waiters and executes its actions.
    pub(crate) fn run<R>(self: &Arc<Self>, f: impl FnOnce(&mut State) -> R) -> R {
        let (result, actions) = {
            let mut state = self.state.lock();
            let result = f(&mut state);
            (result, std::mem::take(&mut state.actions))
        };
        self.changed.notify_waiters();
        self.execute(actions);
        result
    }

    /// A read-only critical section.
    pub(crate) fn with<R>(&self, f: impl FnOnce(&State) -> R) -> R {
        f(&self.state.lock())
    }

    /// Waits until `pred` holds. Every critical section wakes the waiters,
    /// and the wake-up is registered before the check, so none is lost.
    pub(crate) async fn wait_until(&self, mut pred: impl FnMut(&State) -> bool) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.with(&mut pred) {
                return;
            }
            notified.await;
        }
    }

    fn execute(self: &Arc<Self>, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Reload(id) => {
                    tokio::spawn(crate::lifecycle::reload(self.clone(), id));
                }
                Action::Unload(id) => {
                    tokio::spawn(crate::lifecycle::unload(self.clone(), id));
                }
                Action::Emit(emit) => emit.run(self),
                Action::Log { name, fiber, level, error } => {
                    crate::logger::Logger::internal(self, name, fiber).log_error_at(level, &error);
                }
                Action::Drop(value) => drop(value),
            }
        }
    }
}
