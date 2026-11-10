//! The metadata a guest module declares (`cordis_meta`), or that a caller
//! gives for it.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A guest plugin's metadata: its name, the services it reads (`inject`)
/// and the ones it provides.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct GuestMeta {
    pub name: Option<String>,
    #[serde(default)]
    pub inject: Vec<String>,
    #[serde(default)]
    pub provide: Vec<String>,
}

impl GuestMeta {
    /// A plugin named `name`.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: Some(name.into()), ..Self::default() }
    }

    pub fn inject(mut self, key: impl Into<String>) -> Self {
        self.inject.push(key.into());
        self
    }

    pub fn provide(mut self, key: impl Into<String>) -> Self {
        self.provide.push(key.into());
        self
    }

    /// The same name and services, in any order.
    pub(crate) fn matches(&self, other: &GuestMeta) -> bool {
        let set = |keys: &[String]| keys.iter().cloned().collect::<BTreeSet<_>>();
        self.name == other.name && set(&self.inject) == set(&other.inject) && set(&self.provide) == set(&other.provide)
    }
}

/// What `cordis_meta` writes.
#[derive(Deserialize)]
pub(crate) struct ModuleMeta {
    #[serde(default)]
    pub(crate) deps: Vec<String>,
    pub(crate) plugin: Option<GuestMeta>,
    pub(crate) value: Option<Value>,
}
