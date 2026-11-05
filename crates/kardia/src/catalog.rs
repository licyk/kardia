//! The catalog of every built-in plugin.

use crate::{fs, include, loader, logger_console, timer};

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
