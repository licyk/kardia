//! A WASM guest instance: its store, the limits of its exports and the host
//! functions it imports.

use std::time::Instant;

use kardia_core::{Error, Result};
use wasmtime::{
    Caller, Extern, Func, Linker, Module, ResourceLimiter, Store, StoreLimits, StoreLimitsBuilder, Trap, TypedFunc,
    WasmParams, WasmResults,
};

use crate::guest::{CallControl, Export, GuestFailure, GuestFailureKind, GuestHost, GuestInstance};
use crate::wasm::source::{Shared, TICK};

/// What a store holds: the instance's host and its memory limit.
pub(crate) struct State {
    host: GuestHost,
    limiter: Limiter,
}

/// The store's limits, noting a memory growth they refused: the trap that
/// follows is the guest running out of memory.
struct Limiter {
    limits: StoreLimits,
    refused: bool,
}

impl ResourceLimiter for Limiter {
    fn memory_growing(&mut self, current: usize, desired: usize, maximum: Option<usize>) -> wasmtime::Result<bool> {
        let growing = self.limits.memory_growing(current, desired, maximum);
        self.refused |= growing.is_err();
        growing
    }

    fn memory_grow_failed(&mut self, error: wasmtime::Error) -> wasmtime::Result<()> {
        let failed = self.limits.memory_grow_failed(error);
        self.refused |= failed.is_err();
        failed
    }

    fn table_growing(&mut self, current: usize, desired: usize, maximum: Option<usize>) -> wasmtime::Result<bool> {
        self.limits.table_growing(current, desired, maximum)
    }

    fn table_grow_failed(&mut self, error: wasmtime::Error) -> wasmtime::Result<()> {
        self.limits.table_grow_failed(error)
    }

    fn instances(&self) -> usize {
        self.limits.instances()
    }

    fn tables(&self) -> usize {
        self.limits.tables()
    }

    fn memories(&self) -> usize {
        self.limits.memories()
    }
}

pub(crate) struct WasmInstance {
    store: Store<State>,
    instance: Option<wasmtime::Instance>,
    shared: Shared,
    /// Why the guest was stopped: it fails every later call.
    stopped: Option<GuestFailure>,
}

impl WasmInstance {
    /// Instantiates `module` in a store of its own.
    pub(crate) fn start(module: &Module, shared: &Shared, host: GuestHost) -> Result<Self> {
        let mut limits = StoreLimitsBuilder::new().trap_on_grow_failure(true);
        if let Some(memory) = shared.limits.memory {
            limits = limits.memory_size(memory);
        }
        let limiter = Limiter { limits: limits.build(), refused: false };
        let mut store = Store::new(module.engine(), State { host, limiter });
        store.limiter(|state| &mut state.limiter);
        let mut instance = Self { store, instance: None, shared: shared.clone(), stopped: None };
        instance.budget(&CallControl::default())?;
        let instantiated = shared.linker.instantiate(&mut instance.store, module);
        // a link error is not the instance failing; a trap in its start function is
        instance.instance = Some(instantiated.map_err(|error| match instance.failure_kind(&error) {
            Some(kind) => instance.failure(kind, error),
            None => wasm_error(error),
        })?);
        Ok(instance)
    }

    /// Gives the next export its fuel and its deadline.
    fn budget(&mut self, control: &CallControl) -> Result<()> {
        if let Some(fuel) = self.shared.limits.fuel {
            self.store.set_fuel(fuel).map_err(wasm_error)?;
        }
        if let Some(timeout) = self.shared.limits.timeout {
            if let Some(reason) = self.shared.epoch.get() {
                return Err(GuestFailure::new(GuestFailureKind::Panicked, reason.clone()).into());
            }
            let now = Instant::now();
            let deadline = control.deadline().map_or(now + timeout, |deadline| deadline.min(now + timeout));
            let ticks = deadline.saturating_duration_since(now).as_nanos().div_ceil(TICK.as_nanos()).max(1);
            self.store.set_epoch_deadline(ticks as u64);
            self.store.epoch_deadline_trap();
        }
        Ok(())
    }

    /// Calls `kardia_<export>`, or `cordis_<export>` when the guest has no
    /// `kardia_*` name for it. Only an error of the call itself is the
    /// instance failing.
    fn export(&mut self, export: Export) -> Result<i32> {
        let len = self.store.data().host.message.len() as i32;
        let (instance, store) = (self.instance.expect("instantiated"), &mut self.store);
        let mut find = |prefix: &str| {
            let name = format!("{prefix}_{}", export.name());
            instance.get_func(&mut *store, &name).map(|func| (name, func))
        };
        let Some((name, func)) = find("kardia").or_else(|| find("cordis")) else {
            return match export {
                Export::Dispose => Ok(0),
                _ => Err(Error::msg(format!("failed to find function export `cordis_{}`", export.name()))),
            };
        };
        let called = match export {
            Export::Meta => typed::<(), i32>(func, store, &name)?.call(&mut *store, ()),
            Export::Apply | Export::Call => typed::<i32, i32>(func, store, &name)?.call(&mut *store, len),
            Export::Dispose => func.typed::<(), ()>(&*store).map_err(wasm_error)?.call(&mut *store, ()).map(|()| 0),
        };
        called.map_err(|error| {
            let kind = self.failure_kind(&error).unwrap_or(GuestFailureKind::Trap);
            self.failure(kind, error)
        })
    }

    /// How the instance failed, if `error` is a failure: a refused memory
    /// growth, or a trap.
    fn failure_kind(&mut self, error: &wasmtime::Error) -> Option<GuestFailureKind> {
        if std::mem::take(&mut self.store.data_mut().limiter.refused) {
            return Some(GuestFailureKind::OutOfMemory);
        }
        error.downcast_ref::<Trap>().map(|trap| match trap {
            Trap::OutOfFuel => GuestFailureKind::OutOfFuel,
            Trap::Interrupt => GuestFailureKind::Timeout,
            _ => GuestFailureKind::Trap,
        })
    }

    /// The failure of the instance; out of fuel or time, it is stopped.
    fn failure(&mut self, kind: GuestFailureKind, error: wasmtime::Error) -> Error {
        let failure = GuestFailure::new(kind, format!("{error:#}"));
        if matches!(kind, GuestFailureKind::OutOfFuel | GuestFailureKind::Timeout) {
            self.stopped = Some(GuestFailure::new(kind, format!("the guest was stopped: {failure}")));
        }
        failure.into()
    }
}

impl GuestInstance for WasmInstance {
    fn call(&mut self, export: Export, input: Vec<u8>, control: &CallControl) -> Result<Vec<u8>> {
        if let Some(stopped) = &self.stopped {
            return Err(stopped.clone().into());
        }
        self.store.data_mut().host.begin(input);
        self.budget(control)?;
        let status = self.export(export)?;
        self.store.data_mut().host.finish(status)
    }
}

/// `func` as a typed function, or why it is not one.
fn typed<P: WasmParams, R: WasmResults>(func: Func, store: &Store<State>, name: &str) -> Result<TypedFunc<P, R>> {
    func.typed(store)
        .map_err(|error| wasm_error(error.context(format!("failed to convert function `{name}` to given type"))))
}

/// The host functions, imported from the module `module` (`kardia` or
/// `cordis`, the same functions either way).
pub(crate) fn define(linker: &mut Linker<State>, module: &str) -> wasmtime::Result<()> {
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

pub(crate) fn wasm_error(error: wasmtime::Error) -> Error {
    Error::msg(format!("{error:#}"))
}
