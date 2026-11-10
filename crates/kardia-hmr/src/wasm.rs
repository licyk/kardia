//! WASM guest modules on wasmtime. A module compiles once per version and
//! instantiates in its own store per fiber, so old and new versions run side
//! by side and dropping an instance releases all of its memory.

use std::sync::Arc;
use std::time::{Duration, Instant};

use kardia_core::{Error, Result};
use wasmtime::{Caller, Config, Engine, Extern, Linker, Module, Store, StoreLimits, StoreLimitsBuilder, Trap};

use crate::graph::{ModuleCode, ModuleSource};
use crate::guest::{CallControl, Export, GuestCode, GuestHost, GuestInstance, guest_module};

/// How often the engine's epoch advances when calls have a time limit.
const TICK: Duration = Duration::from_millis(10);

/// Limits of each WASM guest instance. None is set by default.
#[derive(Debug, Clone, Default)]
pub struct WasmLimits {
    fuel: Option<u64>,
    timeout: Option<Duration>,
    memory: Option<usize>,
}

impl WasmLimits {
    pub fn new() -> Self {
        Self::default()
    }

    /// The fuel each export may consume (about one unit per instruction).
    pub fn fuel(mut self, fuel: u64) -> Self {
        self.fuel = Some(fuel);
        self
    }

    /// How long each export may run (by epoch interruption); a caller's own
    /// deadline is then honoured too.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// The most linear memory an instance may have, in bytes; growing past it
    /// traps.
    pub fn memory(mut self, bytes: usize) -> Self {
        self.memory = Some(bytes);
        self
    }
}

/// Loads `.wasm` (or `.wat`) files exporting the guest ABI; module ids are
/// their paths. Exports are found as `kardia_*`, else `cordis_*`, and host
/// functions are offered under both import modules, `kardia` and `cordis`. An export that runs out of fuel or time traps, and the
/// instance fails its later calls.
pub struct WasmSource {
    engine: Engine,
    linker: Arc<Linker<State>>,
    limits: Arc<WasmLimits>,
}

impl WasmSource {
    pub fn new() -> Self {
        Self::with_limits(WasmLimits::default())
    }

    pub fn with_limits(limits: WasmLimits) -> Self {
        let mut config = Config::new();
        config.consume_fuel(limits.fuel.is_some()).epoch_interruption(limits.timeout.is_some());
        let engine = Engine::new(&config).expect("the engine's settings are supported");
        if limits.timeout.is_some() {
            tick(&engine);
        }
        let mut linker = Linker::new(&engine);
        for module in ["kardia", "cordis"] {
            define(&mut linker, module).expect("host functions have distinct names");
        }
        Self { engine, linker: Arc::new(linker), limits: Arc::new(limits) }
    }
}

impl Default for WasmSource {
    fn default() -> Self {
        Self::new()
    }
}

impl ModuleSource for WasmSource {
    fn load(&self, id: &str) -> Result<ModuleCode> {
        let module = Module::from_file(&self.engine, id).map_err(wasm_error)?;
        let code = WasmCode { module, linker: self.linker.clone(), limits: self.limits.clone() };
        guest_module(id, Arc::new(code))
    }
}

/// Advances the engine's epoch every tick, until the engine is dropped.
fn tick(engine: &Engine) {
    let engine = engine.weak();
    std::thread::Builder::new()
        .name("wasm-epoch".into())
        .spawn(move || {
            while let Some(engine) = engine.upgrade() {
                engine.increment_epoch();
                drop(engine);
                std::thread::sleep(TICK);
            }
        })
        .expect("the epoch thread starts");
}

/// What a store holds: the instance's host and its memory limit.
struct State {
    host: GuestHost,
    limits: StoreLimits,
}

struct WasmCode {
    module: Module,
    linker: Arc<Linker<State>>,
    limits: Arc<WasmLimits>,
}

impl GuestCode for WasmCode {
    fn instantiate(&self, host: GuestHost) -> Result<Box<dyn GuestInstance>> {
        let mut limits = StoreLimitsBuilder::new().trap_on_grow_failure(true);
        if let Some(memory) = self.limits.memory {
            limits = limits.memory_size(memory);
        }
        let mut store = Store::new(self.module.engine(), State { host, limits: limits.build() });
        store.limiter(|state| &mut state.limits);
        let mut instance = WasmInstance { store, instance: None, limits: self.limits.clone(), stopped: None };
        instance.budget(&CallControl::default()).map_err(wasm_error)?;
        instance.instance = Some(self.linker.instantiate(&mut instance.store, &self.module).map_err(wasm_error)?);
        Ok(Box::new(instance))
    }
}

struct WasmInstance {
    store: Store<State>,
    instance: Option<wasmtime::Instance>,
    limits: Arc<WasmLimits>,
    /// Why the guest was stopped: it fails every later call.
    stopped: Option<String>,
}

impl WasmInstance {
    /// Gives the next export its fuel and its deadline.
    fn budget(&mut self, control: &CallControl) -> wasmtime::Result<()> {
        if let Some(fuel) = self.limits.fuel {
            self.store.set_fuel(fuel)?;
        }
        if let Some(timeout) = self.limits.timeout {
            let now = Instant::now();
            let deadline = control.deadline().map_or(now + timeout, |deadline| deadline.min(now + timeout));
            let ticks = deadline.saturating_duration_since(now).as_nanos().div_ceil(TICK.as_nanos()).max(1);
            self.store.set_epoch_deadline(ticks as u64);
            self.store.epoch_deadline_trap();
        }
        Ok(())
    }

    /// Calls `kardia_<export>`, or `cordis_<export>` when the guest has no
    /// `kardia_*` name for it.
    fn export(&mut self, export: Export) -> wasmtime::Result<i32> {
        let len = self.store.data().host.message.len() as i32;
        let (instance, store) = (self.instance.expect("instantiated"), &mut self.store);
        let mut find = |prefix: &str| {
            let name = format!("{prefix}_{}", export.name());
            instance.get_func(&mut *store, &name).map(|func| (name, func))
        };
        let Some((name, func)) = find("kardia").or_else(|| find("cordis")) else {
            return match export {
                Export::Dispose => Ok(0),
                _ => Err(wasmtime::Error::msg(format!("failed to find function export `cordis_{}`", export.name()))),
            };
        };
        let convert =
            |error: wasmtime::Error| error.context(format!("failed to convert function `{name}` to given type"));
        match export {
            Export::Meta => func.typed::<(), i32>(&*store).map_err(convert)?.call(store, ()),
            Export::Apply | Export::Call => func.typed::<i32, i32>(&*store).map_err(convert)?.call(store, len),
            Export::Dispose => func.typed::<(), ()>(&*store)?.call(store, ()).map(|()| 0),
        }
    }
}

impl GuestInstance for WasmInstance {
    fn call(&mut self, export: Export, input: Vec<u8>, control: &CallControl) -> Result<Vec<u8>> {
        if let Some(reason) = &self.stopped {
            return Err(Error::msg(reason.clone()));
        }
        self.store.data_mut().host.begin(input);
        let status = self.budget(control).and_then(|()| self.export(export));
        match status {
            Ok(status) => self.store.data_mut().host.finish(status),
            Err(error) => {
                let stopped = matches!(error.downcast_ref::<Trap>(), Some(Trap::OutOfFuel | Trap::Interrupt));
                let error = wasm_error(error);
                if stopped {
                    self.stopped = Some(format!("the guest was stopped: {error}"));
                }
                Err(error)
            }
        }
    }
}

/// The host functions, imported from the module `module` (`kardia` or
/// `cordis`, the same functions either way).
fn define(linker: &mut Linker<State>, module: &str) -> wasmtime::Result<()> {
    linker.func_wrap(module, "read", |mut caller: Caller<'_, State>, ptr: i32| {
        let (memory, host) = memory(&mut caller)?;
        let start = ptr as u32 as usize;
        memory.get_mut(start..start + host.message.len()).ok_or_else(out_of_bounds)?.copy_from_slice(&host.message);
        Ok(())
    })?;
    linker.func_wrap(module, "write", |mut caller: Caller<'_, State>, ptr: i32, len: i32| {
        let (memory, host) = memory(&mut caller)?;
        host.write(bytes(memory, ptr, len)?);
        Ok(())
    })?;
    linker.func_wrap(module, "log", |mut caller: Caller<'_, State>, level: i32, ptr: i32, len: i32| {
        let (memory, host) = memory(&mut caller)?;
        host.log_raw(level, bytes(memory, ptr, len)?);
        Ok(())
    })?;
    linker.func_wrap(
        module,
        "provide",
        |mut caller: Caller<'_, State>, key: i32, key_len: i32, value: i32, value_len: i32| {
            let (memory, host) = memory(&mut caller)?;
            Ok(host.provide_raw(bytes(memory, key, key_len)?, bytes(memory, value, value_len)?))
        },
    )?;
    linker.func_wrap(module, "view", |mut caller: Caller<'_, State>, key: i32, key_len: i32| {
        let (memory, host) = memory(&mut caller)?;
        Ok(host.view_raw(bytes(memory, key, key_len)?))
    })?;
    linker.func_wrap(
        module,
        "invoke",
        |mut caller: Caller<'_, State>,
         key: i32,
         key_len: i32,
         method: i32,
         method_len: i32,
         args: i32,
         args_len: i32| {
            let (memory, host) = memory(&mut caller)?;
            let (key, method) = (bytes(memory, key, key_len)?, bytes(memory, method, method_len)?);
            Ok(host.invoke_raw(key, method, bytes(memory, args, args_len)?))
        },
    )?;
    Ok(())
}

fn memory<'a>(caller: &'a mut Caller<'_, State>) -> wasmtime::Result<(&'a mut [u8], &'a mut GuestHost)> {
    let memory = caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .ok_or_else(|| wasmtime::Error::msg("the guest exports no memory"))?;
    let (memory, state) = memory.data_and_store_mut(caller);
    Ok((memory, &mut state.host))
}

fn bytes(memory: &[u8], ptr: i32, len: i32) -> wasmtime::Result<&[u8]> {
    let start = ptr as u32 as usize;
    memory.get(start..start + len as u32 as usize).ok_or_else(out_of_bounds)
}

fn out_of_bounds() -> wasmtime::Error {
    wasmtime::Error::msg("guest memory access out of bounds")
}

fn wasm_error(error: wasmtime::Error) -> Error {
    Error::msg(format!("{error:#}"))
}
