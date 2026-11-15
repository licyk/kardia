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
    },
    Host(Handler),
}

impl RecoveryPolicy {
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
        if let RecoveryPolicy::Retry { delay } = &policy {
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

async fn supervise(fiber: Fiber, policy: RecoveryPolicy) {
    let mut observed: Option<(u64, Error)> = None;
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
            RecoveryPolicy::Retry { delay } if !error.is::<ValidationError>() => Ok(RecoveryAction::RetryAfter(*delay)),
            RecoveryPolicy::Retry { .. } => Ok(RecoveryAction::KeepFailed),
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
