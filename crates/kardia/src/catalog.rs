//! The catalog of every built-in plugin.

use crate::{
    fs, fs_memory, http, include, loader, logger_console, server, server_acl, server_echo, server_proxy, server_static,
    timer,
};

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
    catalog.alias("@cordisjs/fs", "fs");
    catalog.register::<fs::LocalFsPlugin>();
    catalog.alias("@cordisjs/fs-node", "fs:local");
    catalog.register::<fs_memory::MemoryFsPlugin>();
    catalog.alias("@cordisjs/fs-memory", "fs:memory");
    catalog.register::<http::HttpPlugin>();
    catalog.alias("@cordisjs/http", "http");
    catalog.register::<server::ServerPlugin>();
    catalog.alias("@cordisjs/server", "server");
    catalog.register::<server_acl::AclPlugin>();
    catalog.alias("@cordisjs/server-acl", "server:acl");
    catalog.register::<server_echo::EchoPlugin>();
    catalog.alias("@cordisjs/server-echo", "server:echo");
    catalog.register::<server_static::StaticPlugin>();
    catalog.alias("@cordisjs/server-static", "server:static");
    catalog.register::<server_proxy::ProxyPlugin>();
    catalog.alias("@cordisjs/server-proxy", "server:proxy");
    catalog.register::<include::IncludePlugin>();
    catalog.alias("@cordisjs/plugin-include", "include");
    catalog
}
