//! Static file serving through the caller's `fs` service.
mod config;
mod detect;
mod exclude;
mod generated;
mod plugin;
mod probe;
mod serve;

pub use config::{Disposition, StaticConfig};
pub use plugin::StaticPlugin;
