use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    #[serde(default = "host")]
    pub host: String,
    pub port: u16,
    pub max_port: Option<u16>,
    pub public_url: Option<String>,
}

impl ServerConfig {
    pub fn local(port: u16) -> Self {
        Self { host: host(), port, max_port: None, public_url: None }
    }
}

fn host() -> String {
    "127.0.0.1".into()
}
