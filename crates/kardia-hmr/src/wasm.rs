//! WASM guest modules on wasmtime. A module compiles once per version and
//! instantiates in its own store per fiber, so old and new versions run side
//! by side and dropping an instance releases all of its memory.

use std::sync::Arc;

use kardia_core::{Error, Result};
use wasmtime::{Caller, Engine, Extern, Linker, Module, Store};

use crate::graph::{ModuleCode, ModuleSource};
use crate::guest::{self, Code, Export, Host, Instance};

/// Loads `.wasm` (or `.wat`) files exporting the guest ABI; module ids are
/// their paths.
pub struct WasmSource {
    engine: Engine,
    linker: Arc<Linker<Host>>,
}

impl WasmSource {
    pub fn new() -> Self {
        let engine = Engine::default();
        let mut linker = Linker::new(&engine);
        define(&mut linker).expect("host functions have distinct names");
        Self { engine, linker: Arc::new(linker) }
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
        guest::module(id, Arc::new(WasmCode { module, linker: self.linker.clone() }))
    }
}

struct WasmCode {
    module: Module,
    linker: Arc<Linker<Host>>,
}

impl Code for WasmCode {
    fn instantiate(&self, host: Host) -> Result<Box<dyn Instance>> {
        let mut store = Store::new(self.module.engine(), host);
        let instance = self.linker.instantiate(&mut store, &self.module).map_err(wasm_error)?;
        Ok(Box::new(WasmInstance { store, instance }))
    }
}

struct WasmInstance {
    store: Store<Host>,
    instance: wasmtime::Instance,
}

impl Instance for WasmInstance {
    fn host(&mut self) -> &mut Host {
        self.store.data_mut()
    }

    fn call(&mut self, export: Export) -> Result<i32> {
        let len = self.store.data().message.len() as i32;
        let (instance, store) = (self.instance, &mut self.store);
        let result = match export {
            Export::Meta => {
                instance.get_typed_func::<(), i32>(&mut *store, "cordis_meta").and_then(|f| f.call(store, ()))
            }
            Export::Apply => {
                instance.get_typed_func::<i32, i32>(&mut *store, "cordis_apply").and_then(|f| f.call(store, len))
            }
            Export::Call => {
                instance.get_typed_func::<i32, i32>(&mut *store, "cordis_call").and_then(|f| f.call(store, len))
            }
            Export::Dispose => match instance.get_func(&mut *store, "cordis_dispose") {
                Some(dispose) => dispose.typed::<(), ()>(&*store).and_then(|f| f.call(store, ())).map(|()| 0),
                None => Ok(0),
            },
        };
        result.map_err(wasm_error)
    }
}

/// The host functions, imported from the module `cordis`.
fn define(linker: &mut Linker<Host>) -> wasmtime::Result<()> {
    linker.func_wrap("cordis", "read", |mut caller: Caller<'_, Host>, ptr: i32| {
        let (memory, host) = memory(&mut caller)?;
        let start = ptr as u32 as usize;
        memory.get_mut(start..start + host.message.len()).ok_or_else(out_of_bounds)?.copy_from_slice(&host.message);
        Ok(())
    })?;
    linker.func_wrap("cordis", "write", |mut caller: Caller<'_, Host>, ptr: i32, len: i32| {
        let (memory, host) = memory(&mut caller)?;
        host.write(bytes(memory, ptr, len)?);
        Ok(())
    })?;
    linker.func_wrap("cordis", "log", |mut caller: Caller<'_, Host>, level: i32, ptr: i32, len: i32| {
        let (memory, host) = memory(&mut caller)?;
        host.log(level, bytes(memory, ptr, len)?);
        Ok(())
    })?;
    linker.func_wrap(
        "cordis",
        "provide",
        |mut caller: Caller<'_, Host>, key: i32, key_len: i32, value: i32, value_len: i32| {
            let (memory, host) = memory(&mut caller)?;
            Ok(host.provide(bytes(memory, key, key_len)?, bytes(memory, value, value_len)?))
        },
    )?;
    linker.func_wrap("cordis", "view", |mut caller: Caller<'_, Host>, key: i32, key_len: i32| {
        let (memory, host) = memory(&mut caller)?;
        Ok(host.view(bytes(memory, key, key_len)?))
    })?;
    linker.func_wrap(
        "cordis",
        "invoke",
        |mut caller: Caller<'_, Host>,
         key: i32,
         key_len: i32,
         method: i32,
         method_len: i32,
         args: i32,
         args_len: i32| {
            let (memory, host) = memory(&mut caller)?;
            let (key, method) = (bytes(memory, key, key_len)?, bytes(memory, method, method_len)?);
            Ok(host.invoke(key, method, bytes(memory, args, args_len)?))
        },
    )?;
    Ok(())
}

fn memory<'a>(caller: &'a mut Caller<'_, Host>) -> wasmtime::Result<(&'a mut [u8], &'a mut Host)> {
    let memory = caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .ok_or_else(|| wasmtime::Error::msg("the guest exports no memory"))?;
    Ok(memory.data_and_store_mut(caller))
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
