//! Access to the timer service from a context.

use kardia_core::{Context, Svc};

use crate::service::Timer;

/// Convenience access to the timer service from a context that declares it.
pub trait TimerContext {
    fn timer(&self) -> std::result::Result<Svc<Timer>, kardia_core::AccessError>;
}

impl TimerContext for Context {
    fn timer(&self) -> std::result::Result<Svc<Timer>, kardia_core::AccessError> {
        self.get::<Timer>()
    }
}
