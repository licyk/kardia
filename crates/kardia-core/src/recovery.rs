use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use tokio::task::AbortHandle;

use crate::{Error, Fiber, FiberState, Result, ValidationError};

/// The host's decision after observing a failed, fully cleaned-up instance.
#[derive(Clone, Copy, Debug, Default)]
pub enum RecoveryAction {
    #[default]
    KeepFailed,
    RetryAfter(Duration),
}

/// A failure notification delivered outside runtime locks.
#[derive(Clone, Debug)]
pub struct Failure {
    pub fiber: Fiber,
    pub error: Error,
    /// Where the failure happened, which a policy needs to tell a failed
    /// constructor from a fault of the running instance.
    pub phase: Phase,
    /// The automatic restarts this supervisor made before this failure.
    pub attempts: u32,
}

/// The phase a fiber was in when it failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// A load or reload failed: `apply` returned an error or panicked, so the
    /// constructor itself fails and retrying reaches the same error again.
    Start,
    /// A running instance faulted, through `Fiber::fail` or a caught panic.
    Running,
}

type Handler = Arc<dyn Fn(Failure) -> BoxFuture<'static, Result<RecoveryAction>> + Send + Sync>;

/// What a policy may read about a fiber's automatic recovery.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecoveryState {
    /// The automatic restarts made inside the current window.
    pub attempts: u32,
    /// When the scheduled restart runs, while one is pending.
    pub due_in: Option<Duration>,
    /// Which automatic restart that scheduled one is, while one is pending.
    pub next_attempt: Option<u32>,
}

/// The retry a supervisor is waiting out, shared with `Fiber::recovery_state`.
#[derive(Clone, Copy, Debug)]
struct Pending {
    attempt: u32,
    delay: Duration,
}

/// Failure isolation is independent of this policy. The default never retries.
#[derive(Clone, Default)]
pub enum RecoveryPolicy {
    #[default]
    Manual,
    Retry {
        delay: Duration,
        /// How many times the fiber is restarted before it stays failed; `None`
        /// retries without a limit.
        max_retries: Option<u32>,
        /// How the delay grows between attempts.
        backoff: Backoff,
    },
    /// Waits `delays[n]` before the n-th automatic restart; the list's end is
    /// the limit.
    Delays {
        delays: Vec<Duration>,
        /// How long an automatic restart is counted before it no longer uses up
        /// the next delay; `None` counts them for the life of the fiber.
        window: Option<Duration>,
    },
    Host(Handler),
}

/// How the delay of a retry changes with each attempt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Backoff {
    /// The configured delay every time.
    #[default]
    Fixed,
    /// The delay doubles with each attempt, capped at the configured delay
    /// multiplied by 2^5 = 32.
    Exponential,
}

impl RecoveryPolicy {
    /// Retries after `delay` every time, without a limit.
    pub fn retry(delay: Duration) -> Self {
        Self::Retry { delay, max_retries: None, backoff: Backoff::Fixed }
    }

    /// Retries with the given delay, limit and growth.
    pub fn retry_with(delay: Duration, max_retries: Option<u32>, backoff: Backoff) -> Self {
        Self::Retry { delay, max_retries, backoff }
    }

    /// Waits the n-th delay before the n-th automatic restart, within `window`.
    pub fn retry_delays(delays: impl Into<Vec<Duration>>, window: Option<Duration>) -> Self {
        Self::Delays { delays: delays.into(), window }
    }

    pub fn host<F, Fut>(handler: F) -> Self
    where
        F: Fn(Failure) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<RecoveryAction>> + Send + 'static,
    {
        Self::Host(Arc::new(move |failure| handler(failure).boxed()))
    }
}

pub(crate) struct RecoveryTask {
    task: AbortHandle,
    state: Arc<Mutex<Shared>>,
    /// Wakes the supervisor out of its delay when `Fiber::retry_now` asks.
    now: Arc<tokio::sync::Notify>,
}

/// What the supervisor and `Fiber::recovery_state` both see.
#[derive(Debug, Default)]
struct Shared {
    attempts: u32,
    pending: Option<Pending>,
}

impl Drop for RecoveryTask {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Fiber {
    /// Changes this instantiation's policy, cancelling its previous pending action.
    /// The policy survives instance failure/reload and ends when the fiber is retired.
    pub fn set_recovery(&self, policy: RecoveryPolicy) -> Result<()> {
        if self.is_root() {
            return Err(Error::msg("the root context cannot be automatically recovered"));
        }
        if let RecoveryPolicy::Retry { delay, .. } = &policy {
            validate_delay(*delay)?;
        }
        if let RecoveryPolicy::Delays { delays, .. } = &policy {
            if delays.is_empty() {
                return Err(Error::msg("a delay table needs at least one delay"));
            }
            for delay in delays {
                validate_delay(*delay)?;
            }
        }
        self.ctx.root.run(|st| {
            st.assert_active(self.id())?;
            let fiber = st.fiber_mut(self.id()).expect("active fiber");
            let old = fiber.recovery.take();
            st.drop_later(old);
            Ok::<_, Error>(())
        })?;
        if matches!(policy, RecoveryPolicy::Manual) {
            return Ok(());
        }
        let fiber = self.clone();
        let state = Arc::new(Mutex::new(Shared::default()));
        let shared = state.clone();
        let now = Arc::new(tokio::sync::Notify::new());
        let wake = now.clone();
        let task = tokio::spawn(async move { supervise(fiber, policy, shared, wake).await });
        let recovery = RecoveryTask { task: task.abort_handle(), state, now };
        self.ctx.root.run(|st| {
            if let Some(fiber) = st.fiber_mut(self.id()).filter(|fiber| !fiber.removed) {
                fiber.recovery = Some(recovery);
            } else {
                st.drop_later(recovery);
            }
        });
        Ok(())
    }

    /// The automatic restarts made and the one currently scheduled, if any.
    pub fn recovery_state(&self) -> RecoveryState {
        let Some(shared) =
            self.ctx.root.with(|st| st.fiber(self.id()).and_then(|f| f.recovery.as_ref().map(|r| r.state.clone())))
        else {
            return RecoveryState::default();
        };
        let shared = shared.lock().expect("recovery state");
        RecoveryState {
            attempts: shared.attempts,
            due_in: shared.pending.map(|pending| pending.delay),
            next_attempt: shared.pending.map(|pending| pending.attempt),
        }
    }

    /// Restarts now instead of waiting out the scheduled delay, as a user's
    /// retry does when a fiber keeps failing.
    pub fn retry_now(&self) -> Result<()> {
        let recovery = self.ctx.root.run(|st| {
            let fiber = st.fiber_mut(self.id())?;
            fiber.recovery.as_ref().map(|recovery| recovery.now.clone())
        });
        let Some(now) = recovery else {
            return Err(Error::msg("this fiber has no automatic recovery to retry now"));
        };
        now.notify_one();
        Ok(())
    }
}

/// The delay before attempt `attempt` (1 for the first retry). An exponential
/// backoff doubles it and stops growing at the 6th step, so a failure loop cannot
/// stretch the delay without bound.
fn backoff_of(backoff: Backoff, delay: Duration, attempt: u32) -> Duration {
    match backoff {
        Backoff::Fixed => delay,
        Backoff::Exponential => {
            let shift = attempt.saturating_sub(1).min(5);
            delay.saturating_mul(1u32 << shift)
        }
    }
}

async fn supervise(fiber: Fiber, policy: RecoveryPolicy, shared: Arc<Mutex<Shared>>, now: Arc<tokio::sync::Notify>) {
    let mut observed: Option<(u64, Error)> = None;
    // When each automatic restart happened, which is what a window counts.
    let mut restarts: Vec<std::time::Instant> = Vec::new();
    loop {
        fiber
            .ctx
            .root
            .wait_until(|st| {
                st.fiber(fiber.id()).is_none_or(|state| {
                    state.removed
                        || (!state.inertia
                            && state.state == FiberState::Failed
                            && state.error.as_ref().is_some_and(|error| {
                                observed
                                    .as_ref()
                                    .is_none_or(|(revision, old)| *revision != state.revision || !error.ptr_eq(old))
                            }))
                })
            })
            .await;
        if fiber.is_removed() {
            return;
        }
        let Some((revision, error)) = fiber.ctx.root.with(|st| {
            let state = st.fiber(fiber.id())?;
            Some((state.revision, state.error.clone()?))
        }) else {
            continue;
        };
        observed = Some((revision, error.clone()));
        let phase =
            fiber.ctx.root.with(|st| st.fiber(fiber.id()).and_then(|state| state.phase)).unwrap_or(Phase::Running);
        let in_window = |restarts: &mut Vec<std::time::Instant>| -> u32 {
            if let (Some(window), true) = (window_of(&policy), !restarts.is_empty()) {
                let cutoff = std::time::Instant::now() - window;
                restarts.retain(|at| *at >= cutoff);
            }
            u32::try_from(restarts.len()).unwrap_or(u32::MAX)
        };
        let action = match &policy {
            RecoveryPolicy::Manual => return,
            RecoveryPolicy::Retry { delay, max_retries, backoff } => {
                // A configuration that does not validate is not retried: the same
                // input fails the same way, so a retry only spins. A limit that has
                // been reached leaves the fiber failed for the same reason.
                let attempts = in_window(&mut restarts);
                let exhausted = error.is::<ValidationError>() || max_retries.is_some_and(|limit| attempts >= limit);
                if exhausted {
                    Ok(RecoveryAction::KeepFailed)
                } else {
                    Ok(RecoveryAction::RetryAfter(backoff_of(*backoff, *delay, attempts + 1)))
                }
            }
            RecoveryPolicy::Delays { delays, .. } => {
                let attempts = in_window(&mut restarts);
                match delays.get(attempts as usize) {
                    Some(delay) if !error.is::<ValidationError>() => Ok(RecoveryAction::RetryAfter(*delay)),
                    _ => Ok(RecoveryAction::KeepFailed),
                }
            }
            RecoveryPolicy::Host(handler) => {
                let failure =
                    Failure { fiber: fiber.clone(), error: error.clone(), phase, attempts: in_window(&mut restarts) };
                AssertUnwindSafe(async { handler(failure).await })
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|panic| Err(Error::from_panic(panic)))
                    .and_then(|action| {
                        if let RecoveryAction::RetryAfter(delay) = action {
                            validate_delay(delay)?;
                        }
                        Ok(action)
                    })
            }
        };
        let delay = match action {
            Ok(RecoveryAction::KeepFailed) => continue,
            Ok(RecoveryAction::RetryAfter(delay)) if !error.is::<ValidationError>() => delay,
            Ok(RecoveryAction::RetryAfter(_)) => continue,
            Err(error) => {
                fiber.context().logger().log_error(&error);
                continue;
            }
        };
        {
            let mut shared = shared.lock().expect("recovery state");
            shared.attempts = in_window(&mut restarts) + 1;
            shared.pending = Some(Pending { attempt: shared.attempts, delay });
        }
        {
            let scheduled =
                RecoveryScheduled { fiber: fiber.clone(), due_in: delay, attempt: in_window(&mut restarts) + 1 };
            fiber.ctx.root.run(|st| st.emit(move |_| scheduled));
        }
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = now.notified() => {}
            _ = fiber.ctx.root.wait_until(|st| st.fiber(fiber.id()).is_none_or(|state| {
                state.removed || state.revision != revision || state.error.as_ref().is_none_or(|current| !current.ptr_eq(&error))
            })) => continue,
        }
        shared.lock().expect("recovery state").pending = None;
        if fiber.is_removed() || fiber.error().is_none_or(|current| !current.ptr_eq(&error)) {
            continue;
        }
        restarts.push(std::time::Instant::now());
        match fiber.update_with(fiber.config(), true) {
            Ok(settle) => {
                let _ = settle.await;
            }
            Err(error) => fiber.context().logger().log_error(&error),
        }
    }
}

fn validate_delay(delay: Duration) -> Result<()> {
    if delay.is_zero() {
        return Err(Error::msg("recovery retry delay must be greater than zero"));
    }
    Ok(())
}

/// The window a policy counts automatic restarts in, if it has one.
fn window_of(policy: &RecoveryPolicy) -> Option<Duration> {
    match policy {
        RecoveryPolicy::Delays { window, .. } => *window,
        _ => None,
    }
}

/// A restart is scheduled: `due_in` from now, as the `attempt`-th automatic one.
pub struct RecoveryScheduled {
    pub fiber: Fiber,
    pub due_in: Duration,
    pub attempt: u32,
}

impl crate::events::Event for RecoveryScheduled {
    type Output = ();
}

#[cfg(test)]
mod tests {
    use super::{Backoff, Duration, backoff_of};

    #[test]
    fn a_fixed_backoff_returns_the_configured_delay() {
        let base = Duration::from_millis(10);
        for attempt in 1..10 {
            assert_eq!(backoff_of(Backoff::Fixed, base, attempt), base);
        }
    }

    #[test]
    fn an_exponential_backoff_doubles_and_stops_growing() {
        let base = Duration::from_millis(10);
        let of = |attempt| backoff_of(Backoff::Exponential, base, attempt);
        assert_eq!(of(1), Duration::from_millis(10));
        assert_eq!(of(2), Duration::from_millis(20));
        assert_eq!(of(3), Duration::from_millis(40));
        // Growth stops at the 6th step, so a failure loop cannot stretch it.
        assert_eq!(of(7), Duration::from_millis(320));
        assert_eq!(of(20), Duration::from_millis(320));
    }
}
