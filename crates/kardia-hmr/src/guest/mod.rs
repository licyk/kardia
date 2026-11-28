//! The guest ABI shared by the backends (see the crate docs): the transport
//! traits a backend implements, and the bridge plugin that runs a guest in a
//! fiber.

mod action;
mod bridge;
mod control;
mod failure;
mod host;
mod meta;
mod service;
mod transport;

pub use action::HostAction;
pub(crate) use action::Registered;
pub use action::register as register_action;
pub(crate) use bridge::Slot;
#[cfg(feature = "process")]
pub(crate) use bridge::{cancelled, exited, timed_out};
pub use bridge::{guest_module, guest_plugin};
pub use control::CallControl;
pub use failure::{GuestFailure, GuestFailureKind};
pub use host::GuestHost;
#[cfg(feature = "process")]
pub(crate) use host::level;
pub use meta::GuestMeta;
pub use service::GuestService;
pub use transport::{Export, GuestCode, GuestInstance};
