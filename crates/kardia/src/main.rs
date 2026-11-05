//! `kardia [kardia.yml]`: boots a root with the loader and the built-in
//! plugins, includes the configuration file (YAML or JSON) and keeps the app
//! in step with it until interrupted: editing the file reconfigures the
//! running app, and runtime changes are written back to it.
//!
//! An entry name that is no built-in plugin is a guest module (`.wasm` /
//! `.wat`, or a dynamic library) relative to the configuration file; the
//! `hmr` plugin reloads guest modules in place as they change.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use futures::future::BoxFuture;
use kardia::fs::FsPlugin;
use kardia::hmr::{DylibSource, Hmr, ModuleCode, ModuleGraph, ModuleSource, WasmSource};
use kardia::loader::{Catalog, Resolver};
use kardia::logger_console::{ConsoleConfig, ConsoleExporter};
use kardia::prelude::*;

#[tokio::main]
async fn main() -> ExitCode {
    let path = std::env::args().nth(1).unwrap_or_else(|| "kardia.yml".into());
    let root = Context::new();
    if let Err(error) = run(&root, &path).await {
        root.logger_named("app").log_error(&error);
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

async fn run(root: &Context, path: &str) -> Result<()> {
    root.add_exporter(Arc::new(ConsoleExporter::new(ConsoleConfig::default())))?;
    root.plugin::<FsPlugin>(())?;
    let catalog = kardia::catalog();
    let graph = ModuleGraph::new(GuestFiles { wasm: WasmSource::new() });
    catalog.register_def("hmr", Hmr::plugin(graph.clone()));
    catalog.alias("@cordisjs/plugin-hmr", "hmr");
    let base = std::path::absolute(path)?.parent().map(Path::to_path_buf).unwrap_or_default();
    let plugins = Plugins { catalog, graph, base };
    root.plugin_def(&Loader::plugin(plugins), Value::Null)?.settled().await?;
    let loader = (*root.get::<Loader>()?).clone();
    loader.set_logs(true);
    let entry = json!({ "name": "include", "config": { "path": path, "watch": true } });
    let id = loader.create(entry, None, None).await?;
    if let Some(error) = loader.resolve(&id)?.inner_fiber().and_then(|fiber| fiber.error()) {
        return Err(error);
    }
    tokio::signal::ctrl_c().await?;
    root.fiber().dispose().await
}

/// Built-in plugins by name; any other name is a guest module path.
struct Plugins {
    catalog: Catalog,
    graph: ModuleGraph,
    /// The directory of the configuration file.
    base: PathBuf,
}

impl Resolver for Plugins {
    fn resolve(&self, name: &str) -> BoxFuture<'static, Result<PluginDef>> {
        if self.catalog.get(name).is_some() {
            return self.catalog.resolve(name);
        }
        // module ids are clean absolute paths, as the watch reports them
        let id: PathBuf = self.base.join(name).components().collect();
        self.graph.resolve(&id.to_string_lossy())
    }
}

/// Guest modules by extension: WASM, or else a dynamic library.
struct GuestFiles {
    wasm: WasmSource,
}

impl ModuleSource for GuestFiles {
    fn load(&self, id: &str) -> Result<ModuleCode> {
        match Path::new(id).extension().and_then(|ext| ext.to_str()) {
            Some("wasm" | "wat") => self.wasm.load(id),
            _ => DylibSource.load(id),
        }
    }
}
