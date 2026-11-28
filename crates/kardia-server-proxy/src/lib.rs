//! Streaming HTTP and WebSocket reverse proxy with caller-owned transport.
mod config;
mod plugin;
mod socket;
pub use config::ProxyConfig;
pub use plugin::ProxyPlugin;
