//! HTTP routes and streams owned by their calling plugin's lifecycle.

mod accept;
mod body;
mod config;
mod dispatch;
mod form;
mod ownership;
mod pattern;
mod request;
mod response;
mod route;
mod server;
mod socket;
mod transport;
mod websocket;

pub use body::{Body, ByteStream};
pub use bytes::Bytes;
pub use config::ServerConfig;
pub use form::{Blob, FormData, FormEntry, FormValue};
pub use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
pub use pattern::Path;
pub use request::{Request, ValidationError};
pub use response::Response;
pub use route::{Handler, Next, Route, RouteCheck, RouteHook, RouteInfo, handler};
pub use server::{Server, ServerExt, ServerPlugin};
pub use socket::WebSocket;
pub use tokio_tungstenite::tungstenite::{
    Message,
    protocol::{CloseFrame, frame::coding::CloseCode},
};
pub use websocket::{Accept, UpgradeHook, WsExt, WsHandler, WsRoute, ws_handler};
