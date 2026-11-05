//! The guest ABI shared by the WASM and dylib backends (see the crate docs),
//! and the bridge plugin that runs a guest module in a fiber.

use std::cell::RefCell;
use std::path::Path;
use std::sync::{Arc, Weak};

use cordis_core::{Context, Error, Inject, Level, PluginDef, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Map, Value, json};

use crate::graph::{Exports, ModuleCode};
use crate::hmr::normalize;

/// Guest code of one module version, instantiated once per fiber.
pub(crate) trait Code: Send + Sync + 'static {
    fn instantiate(&self, host: Host) -> Result<Box<dyn Instance>>;
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Export {
    Meta,
    Apply,
    Call,
    Dispose,
}

pub(crate) trait Instance: Send + 'static {
    fn host(&mut self) -> &mut Host;

    /// Calls `export` on the host's message and returns the guest's status.
    fn call(&mut self, export: Export) -> Result<i32>;
}

/// An instance owned by a fiber; `None` once the fiber disposed it.
type Slot = Mutex<Option<Box<dyn Instance>>>;

/// The host side of an instance: what its host functions act on.
pub(crate) struct Host {
    /// The fiber running the guest; `None` while the module's metadata is read.
    ctx: Option<Context>,
    this: Weak<Slot>,
    /// The input of the current export, then the result of the last host call.
    pub(crate) message: Vec<u8>,
    /// What the guest wrote: the result of the current export.
    pub(crate) output: Vec<u8>,
}

impl Host {
    fn new(ctx: Option<Context>, this: Weak<Slot>) -> Self {
        Self { ctx, this, message: Vec::new(), output: Vec::new() }
    }

    pub(crate) fn write(&mut self, bytes: &[u8]) {
        self.output = bytes.to_vec();
    }

    pub(crate) fn log(&self, level: i32, text: &[u8]) {
        let Some(ctx) = &self.ctx else { return };
        let level = u8::try_from(level).ok().and_then(|level| Level::try_from(level).ok()).unwrap_or(Level::Info);
        ctx.logger().log(level, String::from_utf8_lossy(text));
    }

    pub(crate) fn provide(&mut self, key: &[u8], value: &[u8]) -> i32 {
        let result = (|| {
            let key = utf8(key)?;
            let value = serde_json::from_slice(value)?;
            let service = GuestService { key: key.into(), value, instance: self.this.clone() };
            self.context()?.provide_serialized_as(key, service)?;
            Ok(Vec::new())
        })();
        self.reply(result)
    }

    pub(crate) fn view(&mut self, key: &[u8]) -> i32 {
        let result = (|| Ok(serde_json::to_vec(&self.context()?.view(utf8(key)?)?)?))();
        self.reply(result)
    }

    pub(crate) fn invoke(&mut self, key: &[u8], method: &[u8], args: &[u8]) -> i32 {
        let result = (|| {
            let service = self.context()?.get_as::<GuestService>(utf8(key)?)?;
            Ok(serde_json::to_vec(&service.call(utf8(method)?, serde_json::from_slice(args)?)?)?)
        })();
        self.reply(result)
    }

    fn context(&self) -> Result<&Context> {
        self.ctx.as_ref().ok_or_else(|| Error::msg("host calls are unavailable while reading module metadata"))
    }

    /// Stores the result as the message: returns its length, or `-1 - len`
    /// for an error message.
    fn reply(&mut self, result: Result<Vec<u8>>) -> i32 {
        match result {
            Ok(bytes) => {
                self.message = bytes;
                self.message.len() as i32
            }
            Err(error) => {
                self.message = error.to_string().into_bytes();
                -1 - self.message.len() as i32
            }
        }
    }
}

/// A service provided by a guest plugin. Its JSON view is the value the
/// guest provided it with; [`GuestService::call`] calls into the guest.
pub struct GuestService {
    key: String,
    value: Value,
    instance: Weak<Slot>,
}

impl GuestService {
    pub fn value(&self) -> &Value {
        &self.value
    }

    /// Calls the guest's `cordis_call` with `{ key, method, args }`.
    pub fn call(&self, method: &str, args: Value) -> Result<Value> {
        let slot = self.instance.upgrade().ok_or_else(disposed)?;
        let input = serde_json::to_vec(&json!({ "key": self.key, "method": method, "args": args }))?;
        let output = run(&slot, Export::Call, input)?;
        serde_json::from_slice(&output)
            .map_err(|error| Error::msg(format!("invalid result of {}.{method}: {error}", self.key)))
    }
}

impl Serialize for GuestService {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.value.serialize(serializer)
    }
}

#[derive(Deserialize)]
struct Meta {
    #[serde(default)]
    deps: Vec<String>,
    plugin: Option<PluginMeta>,
    value: Option<Value>,
}

#[derive(Deserialize)]
struct PluginMeta {
    name: Option<String>,
    #[serde(default)]
    inject: Vec<String>,
    #[serde(default)]
    provide: Vec<String>,
}

/// Reads what a guest module depends on and exports, and wraps it as module code.
pub(crate) fn module(id: &str, code: Arc<dyn Code>) -> Result<ModuleCode> {
    let mut instance = code.instantiate(Host::new(None, Weak::new()))?;
    let output = invoke(&mut *instance, Export::Meta, Vec::new());
    invoke(&mut *instance, Export::Dispose, Vec::new())?;
    let meta: Meta = serde_json::from_slice(&output?)
        .map_err(|error| Error::msg(format!("invalid metadata of module {id}: {error}")))?;
    let deps: Vec<String> = meta.deps.iter().map(|dep| resolve(id, dep)).collect();
    let plugin = meta.plugin.map(Arc::new);
    Ok(ModuleCode::new(deps.clone(), move |imports| {
        Ok(match (&plugin, &meta.value) {
            (Some(plugin), _) => {
                let values: Map<String, Value> =
                    deps.iter().filter_map(|dep| Some((dep.clone(), imports.value(dep)?))).collect();
                Exports::Plugin(bridge(code.clone(), plugin, Value::Object(values)))
            }
            (None, Some(value)) => Exports::Value(value.clone()),
            (None, None) => Exports::None,
        })
    }))
}

/// A plugin that instantiates the guest in each fiber and applies it with
/// `{ config, imports }`. Disposing the fiber disposes the instance.
fn bridge(code: Arc<dyn Code>, meta: &PluginMeta, imports: Value) -> PluginDef {
    let def = PluginDef::new(move |ctx, config| {
        let code = code.clone();
        let input = json!({ "config": config, "imports": imports });
        async move {
            let slot: Arc<Slot> = Arc::new(Mutex::new(None));
            *slot.lock() = Some(code.instantiate(Host::new(Some(ctx.clone()), Arc::downgrade(&slot)))?);
            let logger = ctx.logger();
            let instance = slot.clone();
            ctx.on_dispose(move || {
                if let Err(error) = run(&instance, Export::Dispose, Vec::new()) {
                    logger.log_error(&error);
                }
            })?;
            run(&slot, Export::Apply, serde_json::to_vec(&input)?)?;
            Ok(())
        }
    });
    let inject = meta.inject.iter().fold(Inject::new(), |inject, key| inject.with(key.as_str()));
    let def = def.inject(inject).provides(meta.provide.iter().map(String::as_str));
    match &meta.name {
        Some(name) => def.name(name),
        None => def,
    }
}

thread_local! {
    static ENTERED: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

/// Calls an export of the instance in `slot`; `Dispose` also releases it.
/// A guest calling back into an instance that is running on this thread
/// (directly or through other guests) fails instead of deadlocking.
fn run(slot: &Slot, export: Export, input: Vec<u8>) -> Result<Vec<u8>> {
    let id = std::ptr::from_ref(slot) as usize;
    if ENTERED.with_borrow(|entered| entered.contains(&id)) {
        return Err(Error::msg("re-entrant call into a guest instance"));
    }
    ENTERED.with_borrow_mut(|entered| entered.push(id));
    let mut instance = slot.lock();
    let result = match instance.as_deref_mut() {
        Some(running) => invoke(running, export, input),
        None => Err(disposed()),
    };
    if export == Export::Dispose {
        *instance = None;
    }
    drop(instance);
    ENTERED.with_borrow_mut(|entered| entered.pop());
    result
}

fn invoke(instance: &mut dyn Instance, export: Export, input: Vec<u8>) -> Result<Vec<u8>> {
    let host = instance.host();
    host.message = input;
    host.output.clear();
    let status = instance.call(export)?;
    let output = std::mem::take(&mut instance.host().output);
    match status {
        0 => Ok(output),
        _ if output.is_empty() => Err(Error::msg(format!("guest failed with status {status}"))),
        _ => Err(Error::msg(String::from_utf8_lossy(&output).into_owned())),
    }
}

/// Resolves a dependency relative to the module that declares it.
fn resolve(id: &str, dep: &str) -> String {
    let base = Path::new(id).parent().unwrap_or(Path::new(""));
    normalize(&base.join(dep)).to_string_lossy().into_owned()
}

fn utf8(bytes: &[u8]) -> Result<&str> {
    Ok(std::str::from_utf8(bytes)?)
}

fn disposed() -> Error {
    Error::msg("the guest instance is disposed")
}
