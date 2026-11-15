//! Host policy checks that skip denied HTTP and WebSocket routes.

mod plugin;

pub use plugin::{AclConfig, AclPlugin, AllowedHosts};
