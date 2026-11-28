use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
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
}

type Handler = Arc<dyn Fn(Failure) -> BoxFuture<'static, Result<RecoveryAction>> + Send + Sync>;

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

    pub fn host<F, Fut>(handler: F) -> Self
    where
        F: Fn(Failure) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<RecoveryAction>> + Send + 'static,
    {
        Self::Host(Arc::new(move |failure| handler(failure).boxed()))
    }
}

pub(crate) struct RecoveryTask(AbortHandle);

impl Drop for RecoveryTask {
    fn drop(&mut self) {
        self.0.abort();
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
        let task = tokio::spawn(async move { supervise(fiber, policy).await });
        let recovery = RecoveryTask(task.abort_handle());
        self.ctx.root.run(|st| {
            if let Some(fiber) = st.fiber_mut(self.id()).filter(|fiber| !fiber.removed) {
                fiber.recovery = Some(recovery);
            } else {
                st.drop_later(recovery);
            }
        });
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

async fn supervise(fiber: Fiber, policy: RecoveryPolicy) {
    let mut observed: Option<(u64, Error)> = None;
    // Counts the failures this supervisor has acted on, which is what a retry
    // limit and an exponential backoff are measured against.
    let mut attempts: u32 = 0;
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
        let action = match &policy {
            RecoveryPolicy::Manual => return,
            RecoveryPolicy::Retry { delay, max_retries, backoff } => {
                // A configuration that does not validate is not retried: the same
                // input fails the same way, so a retry only spins. A limit that has
                // been reached leaves the fiber failed for the same reason.
                let exhausted = error.is::<ValidationError>() || max_retries.is_some_and(|limit| attempts >= limit);
                if exhausted {
                    Ok(RecoveryAction::KeepFailed)
                } else {
                    attempts += 1;
                    Ok(RecoveryAction::RetryAfter(backoff_of(*backoff, *delay, attempts)))
                }
            }
            RecoveryPolicy::Host(handler) => {
                let failure = Failure { fiber: fiber.clone(), error: error.clone() };
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
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = fiber.ctx.root.wait_until(|st| st.fiber(fiber.id()).is_none_or(|state| {
                state.removed || state.revision != revision || state.error.as_ref().is_none_or(|current| !current.ptr_eq(&error))
            })) => continue,
        }
        if fiber.is_removed() || fiber.error().is_none_or(|current| !current.ptr_eq(&error)) {
            continue;
        }
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
