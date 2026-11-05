//! Facade crate re-exporting the kardia crates.

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

/// A catalog with every built-in plugin, under its own name and the name of
/// the corresponding JavaScript package.
pub fn catalog() -> loader::Catalog {
    let catalog = loader::builtins();
    catalog.register::<timer::TimerPlugin>();
    catalog.alias("@cordisjs/plugin-timer", "timer");
    catalog.register::<logger_console::ConsolePlugin>();
    catalog.alias("@cordisjs/plugin-logger-console", "logger-console");
    catalog.register::<fs::FsPlugin>();
    catalog.alias("@cordisjs/plugin-fs-node", "fs");
    catalog.register::<include::IncludePlugin>();
    catalog.alias("@cordisjs/plugin-include", "include");
    catalog
}

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
