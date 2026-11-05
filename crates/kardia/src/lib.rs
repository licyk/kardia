//! Facade crate re-exporting the kardia crates.

mod catalog;

pub use kardia_core::*;
/// `#[derive(Service)]` and `#[plugin]`; with this crate, pass `crate = ::kardia`.
pub use kardia_macros::{Service, plugin};
pub use serde_json::{Value, json};

pub use kardia_fs as fs;
pub use kardia_hmr as hmr;
pub use kardia_include as include;
pub use kardia_loader as loader;
pub use kardia_logger_console as logger_console;
pub use kardia_timer as timer;

pub use catalog::catalog;

/// The names most plugins need.
pub mod prelude {
    pub use kardia_core::{
        Context, Deps, Disposer, EffectScope, Error, Fiber, FiberState, Inject, Injected, Plugin, PluginDef, Result,
        Service, Svc,
    };
    pub use kardia_loader::Loader;
    pub use kardia_macros::{Service, plugin};
    pub use kardia_timer::{Timer, TimerContext, TimerExt, TimerPlugin};
    pub use serde::Deserialize;
    pub use serde_json::{Value, json};
}
