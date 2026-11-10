//! WASM guest code: the engine a module compiles on, its limits, and the
//! module source over `.wasm` files.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use kardia_core::{Error, PluginDef, Result};
use parking_lot::Mutex;
use wasmtime::{Config, Engine, Linker, Module};

use crate::contained;
use crate::graph::{ModuleCode, ModuleSource};
use crate::guest::{GuestCode, GuestHost, GuestInstance, GuestMeta, guest_module, guest_plugin};
use crate::wasm::instance::{self, State, WasmInstance, wasm_error};

/// How often the engine's epoch advances when calls have a time limit.
pub(crate) const TICK: Duration = Duration::from_millis(10);

/// Limits of each WASM guest instance. None is set by default.
#[derive(Debug, Clone, Default)]
pub struct WasmLimits {
    pub(crate) fuel: Option<u64>,
    pub(crate) timeout: Option<Duration>,
    pub(crate) memory: Option<usize>,
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

/// Whether wasmtime handles traps through Mach exception ports on macOS, and
/// whether an engine was made with that setting.
struct MachPorts {
    enabled: bool,
    fixed: bool,
}

static MACH_PORTS: Mutex<MachPorts> = Mutex::new(MachPorts { enabled: true, fixed: false });

/// Sets how wasmtime handles WASM traps on macOS for every engine kardia-hmr
/// makes in this process: through Mach exception ports (`true`, wasmtime's
/// default) or through Unix signals (`false`). Elsewhere it changes nothing.
///
/// The setting is process-wide, as wasmtime's trap handler is: wasmtime
/// installs it at the first engine and panics on macOS when a later engine
/// asks for the other setting. Set it before the first [`WasmSource`] or
/// [`WasmGuest`] is made; once one is, setting the other value fails, and
/// nothing changes. An engine the application makes with wasmtime itself
/// must use the same setting.
pub fn set_macos_mach_ports(enabled: bool) -> Result<()> {
    let mut mach_ports = MACH_PORTS.lock();
    if mach_ports.fixed && mach_ports.enabled != enabled {
        return Err(Error::msg(format!(
            "wasmtime's trap handler is set up {}: macos_use_mach_ports cannot change once a WASM engine exists",
            if mach_ports.enabled { "with Mach ports" } else { "with signals" }
        )));
    }
    mach_ports.enabled = enabled;
    Ok(())
}

/// What every module of one engine shares: the host functions, the limits
/// and the epoch thread's fate.
#[derive(Clone)]
pub(crate) struct Shared {
    pub(crate) linker: Arc<Linker<State>>,
    pub(crate) limits: Arc<WasmLimits>,
    /// Why the epoch thread stopped: calls with a time limit then fail.
    pub(crate) epoch: Arc<OnceLock<String>>,
}

/// Loads `.wasm` (or `.wat`) files exporting the guest ABI; module ids are
/// their paths. Exports are found as `kardia_*`, else `cordis_*`, and host
/// functions are offered under both import modules, `kardia` and `cordis`.
/// An export that runs out of fuel or time traps, and the instance fails its
/// later calls.
///
/// Its modules share one engine: [`WasmSource::guest`] compiles a module of
/// its own on it.
pub struct WasmSource {
    engine: Engine,
    shared: Shared,
}

impl WasmSource {
    pub fn new() -> Self {
        Self::with_limits(WasmLimits::default())
    }

    pub fn with_limits(limits: WasmLimits) -> Self {
        let engine = engine(&limits);
        let epoch = Arc::new(OnceLock::new());
        if limits.timeout.is_some() {
            tick(&engine, &epoch);
        }
        let mut linker = Linker::new(&engine);
        for module in ["kardia", "cordis"] {
            instance::define(&mut linker, module).expect("host functions have distinct names");
        }
        Self { engine, shared: Shared { linker: Arc::new(linker), limits: Arc::new(limits), epoch } }
    }

    /// Compiles a module (WASM bytes, or WAT text) on this source's engine,
    /// with its limits. It blocks while it compiles, and starts nothing.
    pub fn guest(&self, bytes: impl AsRef<[u8]>) -> Result<WasmGuest> {
        let module = Module::new(&self.engine, bytes).map_err(wasm_error)?;
        Ok(WasmGuest { module, shared: self.shared.clone() })
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
        WasmGuest { module, shared: self.shared.clone() }.module(id)
    }
}

/// A compiled WASM guest module, instantiated in its own store per fiber,
/// with the limits it was compiled with. As a [`GuestCode`] it runs over the
/// custom-transport API, so a host can make its plugin with metadata of its
/// own ([`WasmGuest::plugin`]) without instantiating it first.
#[derive(Clone)]
pub struct WasmGuest {
    module: Module,
    shared: Shared,
}

impl WasmGuest {
    /// Compiles a module (WASM bytes, or WAT text) on an engine of its own,
    /// with `limits`. It blocks while it compiles, and starts nothing. Guests
    /// that share limits can share an engine instead: [`WasmSource::guest`].
    pub fn new(bytes: impl AsRef<[u8]>, limits: WasmLimits) -> Result<Self> {
        WasmSource::with_limits(limits).guest(bytes)
    }

    /// The module the guest exports, read from its `meta` (this instantiates
    /// the guest once to ask).
    pub fn module(&self, id: &str) -> Result<ModuleCode> {
        guest_module(id, Arc::new(self.clone()))
    }

    /// The guest's plugin, with metadata from the caller: making it
    /// instantiates nothing, and each instance must declare the same metadata
    /// when it starts.
    pub fn plugin(&self, meta: GuestMeta) -> PluginDef {
        guest_plugin(Arc::new(self.clone()), meta)
    }
}

impl GuestCode for WasmGuest {
    fn instantiate(&self, host: GuestHost) -> Result<Box<dyn GuestInstance>> {
        Ok(Box::new(WasmInstance::start(&self.module, &self.shared, host)?))
    }
}

/// An engine for `limits`, with the process-wide trap handling setting.
fn engine(limits: &WasmLimits) -> Engine {
    let mut config = Config::new();
    config.consume_fuel(limits.fuel.is_some()).epoch_interruption(limits.timeout.is_some());
    let mut mach_ports = MACH_PORTS.lock();
    config.macos_use_mach_ports(mach_ports.enabled);
    let engine = Engine::new(&config).expect("the engine's settings are supported");
    mach_ports.fixed = true;
    engine
}

/// Advances the engine's epoch every tick, until the engine is dropped. If
/// the thread panics, `epoch` keeps why.
fn tick(engine: &Engine, epoch: &Arc<OnceLock<String>>) {
    let (engine, epoch) = (engine.weak(), epoch.clone());
    let ticking = move || {
        while let Some(engine) = engine.upgrade() {
            engine.increment_epoch();
            drop(engine);
            std::thread::sleep(TICK);
        }
    };
    contained::spawn("wasm-epoch", ticking, move |reason| drop(epoch.set(reason))).expect("the epoch thread starts");
}
