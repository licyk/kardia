//! The host side of the kardia bridge.
//!
//! A kardia app (the host) and a client runtime, such as the TypeScript code
//! of a Tauri webview running `@kardiajs/core`, each compose their own
//! plugins. The bridge connects the two over any request/response transport
//! plus a message stream (in Tauri: one command and a `Channel`):
//!
//! * **Services, host to client.** Host plugins inject `bridge` and
//!   [`BridgeExt::expose`] an [`Endpoint`] under a name. The exposure is an
//!   effect of the exposing plugin. Every client sees the set of exposed
//!   services and gets a proxy service for each, so client plugins that
//!   depend on one run exactly while it is exposed, and restart when it is
//!   exposed anew.
//! * **Services, client to host.** A client provides a service; the host app
//!   sees it as a [`RemoteService`] provided by the client's session fiber,
//!   withdrawn when the session ends. A client can close at any time, so host
//!   plugins should look these up with `live_as` rather than depend on them.
//! * **Events, host to client.** [`Bridge::emit`] reaches every client.
//! * **Calls** go both ways and are asynchronous: a call into a client fails
//!   with `Timeout` after [`BridgeOptions::timeout`], and with `Unavailable`
//!   once the client is gone.
//!
//! Effects never cross the bridge: each side reverts its own, and the bridge
//! keeps the other side's view in step.
//!
//! The host trusts its clients: any connected client may call every exposed
//! service and provide services under any free name. An app that runs
//! third-party client code must gate those requests itself before they reach
//! [`Bridge::handle`].
//!
//! # Wiring it up
//!
//! In a Tauri app (checked against `tauri` 2.12; this crate does not depend on it):
//!
//! ```ignore
//! use std::sync::Arc;
//! use kardia_bridge::{Bridge, BridgeError, Outbox};
//! use serde_json::Value;
//! use tauri::ipc::{Channel, JavaScriptChannelId};
//!
//! let bridge = Bridge::default();
//! root.plugin_def(&bridge.plugin(), Value::Null)?;
//! // then: tauri::Builder::default().manage(bridge.clone())
//! //   .invoke_handler(tauri::generate_handler![kardia_bridge])
//! //   .on_window_event(move |window, event| {
//! //       if let tauri::WindowEvent::Destroyed = event { bridge.close_client(window.label()) }
//! //   })
//!
//! /// The only entry point of the client (`@kardiajs/tauri` invokes it).
//! /// `channel` comes with `open` requests only, hence the optional channel id.
//! #[tauri::command]
//! async fn kardia_bridge(
//!     bridge: tauri::State<'_, Bridge>,
//!     webview: tauri::Webview,
//!     request: Value,
//!     channel: Option<JavaScriptChannelId>,
//! ) -> Result<Value, BridgeError> {
//!     let outbox = channel.map(|id| Arc::new(ChannelOutbox(id.channel_on(webview))) as Arc<dyn Outbox>);
//!     bridge.handle(request, outbox).await
//! }
//!
//! struct ChannelOutbox(Channel<Value>);
//!
//! impl Outbox for ChannelOutbox {
//!     fn send(&self, message: Value) -> bool {
//!         self.0.send(message).is_ok()
//!     }
//! }
//! ```
//!
//! [`Bridge::handle`] runs its work on the runtime the bridge plugin runs on,
//! so it can be called from any thread.
//!
//! # Protocol
//!
//! Version [`PROTOCOL`]. Every message is a JSON object tagged by `type`,
//! with camelCase fields; unknown fields are ignored.
//!
//! Client requests (each answered with a result, or a [`BridgeError`]):
//!
//! | `type` | Fields | Result |
//! |---|---|---|
//! | `open` | `protocol`, `client` (a label; a new session replaces older ones of the same client) | `{ session, protocol, services }` |
//! | `close` | `session` | `null` |
//! | `call` | `session`, `service`, `method`, `args` | the method's result |
//! | `resolve` | `session`, `id`, `value` | `null` (answers a host `request`) |
//! | `reject` | `session`, `id`, `error` | `null` |
//! | `provide` | `session`, `service`, `value` | `null` |
//! | `withdraw` | `session`, `service` | `null` |
//!
//! Host messages, pushed to the session's stream:
//!
//! | `type` | Fields | Meaning |
//! |---|---|---|
//! | `services` | `services: [{ name, id, value }]` | the exposed services, after every change; a new `id` is a new exposure |
//! | `event` | `name`, `payload` | an event |
//! | `request` | `id`, `service`, `method`, `args` | a call into a service the client provides |
//! | `closed` | `reason`: `"stopped"` or `"replaced"` | the host ended the session: the bridge stopped (reconnect once it is back), or a newer session of the same client label replaced it (do not reconnect) |
//!
//! A [`BridgeError`] is `{ type: "unavailable" | "timeout" | "invalid" | "failed", message }`.

mod bridge;
mod endpoint;
mod protocol;
mod session;

pub use bridge::{Bridge, BridgeExt, BridgeOptions};
pub use endpoint::Endpoint;
pub use protocol::{BridgeError, PROTOCOL};
pub use session::{Outbox, RemoteService};
