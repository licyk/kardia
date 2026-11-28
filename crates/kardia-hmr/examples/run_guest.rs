//! Runs one guest module under the real loader and prints what it did as one
//! JSON object on stdout, so another language's test suite can drive it.
//!
//! ```sh
//! cargo run -p kardia-hmr --example run_guest -- <path> --provides <key> \
//!     [--config <json>] [--call <method> <json>]... [--replace <file>] [--call <method> <json>]...
//! ```
//!
//! `--replace <file>` moves that file over the guest's own path and then
//! hot-reloads it through HMR, before the calls after it: the same fiber, a
//! fresh instance running the new file.
//!
//! The source follows the file: a `.wasm` or `.wat` module runs on wasmtime,
//! anything else given the executable bit runs as a child process, and any
//! other file is loaded as a dynamic library.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use kardia_core::{Context, Result, Svc};
use kardia_hmr::{DylibSource, GuestService, Hmr, HmrExt, ModuleGraph, ModuleSource, ProcessSource, WasmSource};
use kardia_loader::Loader;
use serde_json::{Map, Value, json};

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args).await {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!("{}", json!({ "error": error.to_string() }));
            ExitCode::FAILURE
        }
    }
}

/// What the guest was asked: its path, its config, and the calls to make.
struct Asked {
    path: String,
    provides: String,
    config: Value,
    steps: Vec<Step>,
}

enum Step {
    Call(String, Value),
    Replace(PathBuf),
}

fn asked(args: &[String]) -> Result<Asked> {
    let mut path = None;
    let mut provides = None;
    let mut config = Value::Object(Map::new());
    let mut steps = Vec::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--provides" => {
                provides = Some(rest.next().ok_or_else(|| kardia_core::Error::msg("--provides needs a key"))?.clone());
            }
            "--config" => {
                config = rest.next().ok_or_else(|| kardia_core::Error::msg("--config needs a value"))?.parse()?
            }
            "--call" => {
                let method = rest.next().ok_or_else(|| kardia_core::Error::msg("--call needs a method"))?;
                let args = rest.next().ok_or_else(|| kardia_core::Error::msg("--call needs arguments"))?;
                steps.push(Step::Call(method.clone(), args.parse()?));
            }
            "--replace" => {
                let file = rest.next().ok_or_else(|| kardia_core::Error::msg("--replace needs a file"))?;
                steps.push(Step::Replace(file.into()));
            }
            other if path.is_none() => path = Some(other.to_string()),
            other => return Err(kardia_core::Error::msg(format!("unexpected argument {other}"))),
        }
    }
    Ok(Asked {
        path: path.ok_or_else(|| kardia_core::Error::msg("a guest path is required"))?,
        provides: provides.ok_or_else(|| kardia_core::Error::msg("--provides names the service to read"))?,
        config,
        steps,
    })
}

async fn run(args: &[String]) -> Result<Value> {
    let asked = asked(args)?;
    let path = Path::new(&asked.path);
    let root = Context::new();
    root.plugin::<kardia_fs::FsPlugin>(Default::default())?.settled().await?;
    let graph = ModuleGraph::new(ByFile::for_path(path));
    root.plugin_def(&Loader::plugin(graph.clone()), Value::Null)?.settled().await?;
    root.plugin_def(&Hmr::plugin(graph), Value::Null)?.settled().await?;
    let loader = (*root.get::<Loader>()?).clone();
    let hmr = root.get::<Hmr>()?;

    let options = json!({ "name": asked.path, "config": asked.config });
    let id = loader.create(options, None, None).await?;
    loader.settled().await;
    let entry = loader.resolve(&id)?;
    if let Some(fiber) = entry.inner_fiber() {
        fiber.settled().await?;
    }
    let mut results = Vec::new();
    for step in &asked.steps {
        match step {
            Step::Replace(file) => {
                // a rename, so the loader never reads a half-written guest
                let staged = path.with_extension("new");
                std::fs::copy(file, &staged)?;
                std::fs::rename(&staged, path)?;
                let result = hmr.reconcile(|| async { Ok(vec![path.to_path_buf()]) }).await?;
                results.push(json!({ "replace": result.settled_fibers.len() }));
                loader.settled().await;
            }
            Step::Call(method, arguments) => {
                let service: Svc<GuestService> =
                    root.get_as(asked.provides.as_str()).map_err(|error| kardia_core::Error::msg(error.to_string()))?;
                let result = match service.call(method, arguments.clone()) {
                    Ok(value) => json!({ "ok": value }),
                    Err(error) => json!({ "error": error.to_string() }),
                };
                results.push(json!({ "method": method, "result": result }));
            }
        }
    }
    let service: Svc<GuestService> =
        root.get_as(asked.provides.as_str()).map_err(|error| kardia_core::Error::msg(error.to_string()))?;
    let report = json!({
        "view": service.value(),
        "protocol": service.handle().map(|handle| handle.protocol()),
        "steps": results,
        "logs": root.log_buffer().into_iter().map(|message| message.text).collect::<Vec<_>>(),
    });
    root.fiber().dispose().await?;
    Ok(report)
}

/// The source that runs a guest of this file: a WASM module, a process when
/// it is executable, or a dynamic library.
struct ByFile(Inner);

enum Inner {
    Wasm(WasmSource),
    Process,
    Dylib,
}

impl ByFile {
    fn for_path(path: &Path) -> Self {
        Self(match path.extension().and_then(|ext| ext.to_str()) {
            Some("wasm" | "wat") => Inner::Wasm(WasmSource::new()),
            _ if executable(path) => Inner::Process,
            _ => Inner::Dylib,
        })
    }
}

impl ModuleSource for ByFile {
    fn load(&self, id: &str) -> Result<kardia_hmr::ModuleCode> {
        match &self.0 {
            Inner::Wasm(wasm) => wasm.load(id),
            Inner::Process => ProcessSource.load(id),
            Inner::Dylib => DylibSource.load(id),
        }
    }
}

#[cfg(unix)]
fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn executable(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
}
