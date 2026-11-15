//! Streaming HTTP and WebSocket reverse proxy with caller-owned transport.
mod plugin;
mod socket;
pub use plugin::{ProxyConfig, ProxyPlugin, ServerProxy};
