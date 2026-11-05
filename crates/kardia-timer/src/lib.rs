//! Timer service whose timeouts and intervals are revertible effects.
//!
//! Every scheduling call is an effect of the *caller*'s fiber (through
//! [`Svc::caller`](kardia_core::Svc::caller)), so timers die with the plugin that started them.

mod context;
mod schedule;
mod service;
mod ticks;

pub use context::TimerContext;
pub use schedule::Scheduled;
pub use service::{Disposed, Timer, TimerExt, TimerPlugin};
pub use ticks::Ticks;
