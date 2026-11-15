use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StaticConfig {
    pub root: String,
    pub disposition: Option<Disposition>,
    #[serde(default)]
    pub fallthrough: bool,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default = "default_index")]
    pub index: Vec<String>,
    #[serde(default = "default_redirect")]
    pub redirect: bool,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub error_pages: HashMap<String, String>,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Disposition {
    Attachment,
    Inline,
}
fn default_index() -> Vec<String> {
    vec!["index.html".into(), "index.htm".into()]
}
fn default_redirect() -> bool {
    true
}
