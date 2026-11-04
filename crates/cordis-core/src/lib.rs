//! Core of Cordis: contexts, revertible effects, fibers and reactive service resolution.

mod context;
mod effect;
mod error;
mod fiber;
mod id_table;
mod key;
mod logger;
mod root;
mod scope;
mod service;
mod state;

pub use context::{Context, merge_shallow};
pub use effect::{Disposal, Disposer, EffectHandle, EffectMeta, EffectScope};
pub use error::{AccessError, AggregateError, CordisError, Error, Interrupted, Panicked, Result};
pub use fiber::{Fiber, FiberState};
pub use id_table::IdTable;
pub use key::{FiberId, ImplId, Key, RealmId};
pub use logger::{Exporter, Level, Logger, Message, hyphenate};
pub use service::{Service, Svc};
