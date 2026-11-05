//! Core of kardia: contexts, revertible effects, fibers and reactive service resolution.

mod access;
mod config;
mod context;
mod deps;
mod effect;
mod error;
mod events;
mod fiber;
mod id_table;
mod key;
mod lifecycle;
mod logger;
mod plugin;
mod registry;
mod root;
mod scope;
mod service;
mod state;

pub use config::{ErasedConfig, Issue, PathSegment, ValidationError, decode};
pub use context::{Context, merge_shallow};
pub use deps::{Deps, Has, Injected, index};
pub use effect::{Disposal, Disposer, EffectHandle, EffectMeta, EffectScope};
pub use error::{AccessError, AggregateError, KardiaError, Error, Interrupted, Panicked, Result};
pub use events::{
    Bailable, ConfigCommitted, ConfigHook, Event, PluginInserted, PluginRemoved, Replaced, ServiceChanged,
    StatusChanged,
};
pub use fiber::{Fiber, FiberState, Settle};
pub use id_table::IdTable;
pub use key::{FiberId, ImplId, Key, RealmId};
pub use logger::{Exporter, Level, Logger, Message, hyphenate};
pub use plugin::{Inject, Plugin, PluginDef, RuntimeKey};
pub use registry::Registry;
pub use scope::Overlay;
pub use service::{Service, Svc};
pub use state::Check;
