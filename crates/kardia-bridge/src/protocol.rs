//! The wire format: JSON objects tagged by `type`, with camelCase fields.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The protocol version. A client opening a session with another version is refused.
pub const PROTOCOL: u32 = 1;

/// A failed bridge request, as the client receives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum BridgeError {
    /// The bridge, the session or the service is not there (now).
    #[error("{message}")]
    Unavailable { message: String },
    /// The other side did not answer in time.
    #[error("{message}")]
    Timeout { message: String },
    /// The request is malformed or not allowed.
    #[error("{message}")]
    Invalid { message: String },
    /// The call ran and failed.
    #[error("{message}")]
    Failed { message: String },
}

impl BridgeError {
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::Unavailable { message: message.into() }
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::Timeout { message: message.into() }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid { message: message.into() }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self::Failed { message: message.into() }
    }
}

impl From<kardia_core::Error> for BridgeError {
    fn from(error: kardia_core::Error) -> Self {
        Self::failed(error.to_string())
    }
}

/// A request from the client. Unknown fields are ignored.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub(crate) enum ClientRequest {
    Open {
        protocol: u32,
        client: String,
    },
    Close {
        session: u64,
    },
    Call {
        session: u64,
        service: String,
        method: String,
        #[serde(default)]
        args: Value,
    },
    Resolve {
        session: u64,
        id: u64,
        #[serde(default)]
        value: Value,
    },
    Reject {
        session: u64,
        id: u64,
        error: BridgeError,
    },
    Provide {
        session: u64,
        service: String,
        #[serde(default)]
        value: Value,
    },
    Withdraw {
        session: u64,
        service: String,
    },
}

/// A service the host exposes: its name, the identity of this exposure, and its JSON value.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ServiceInfo {
    pub(crate) name: String,
    pub(crate) id: u64,
    pub(crate) value: Value,
}

/// A message the host pushes to a session.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub(crate) enum HostMessage<'a> {
    Services { services: &'a [ServiceInfo] },
    Event { name: &'a str, payload: &'a Value },
    Request { id: u64, service: &'a str, method: &'a str, args: &'a Value },
    Closed { reason: &'a str },
}

impl HostMessage<'_> {
    pub(crate) fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("host messages serialize")
    }
}
