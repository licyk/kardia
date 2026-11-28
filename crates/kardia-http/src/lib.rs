//! HTTP(S) and WebSocket clients whose operations belong to the caller's lifecycle.
mod body;
mod config;
mod data_url;
mod decode;
mod error;
mod event_stream;
mod request;
mod service;
mod socket;
mod transport;

pub use body::{Body, ByteStream};
pub use bytes::Bytes;
pub use config::{HttpConfig, Redirect, RequestConfig};
pub use error::HttpError;
pub use event_stream::{EventStream, ServerSentEvent, parse_event_stream};
pub use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
pub use request::{HttpExt, HttpResponse};
pub use service::{Http, HttpPlugin};
pub use socket::{ClientSocket, WebSocketExt};
pub use tokio_tungstenite::tungstenite::{Message, protocol::CloseFrame};
pub use url::Url;
