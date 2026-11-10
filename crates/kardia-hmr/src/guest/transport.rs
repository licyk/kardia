//! The traits a guest transport implements: the WASM, dylib and process
//! backends, or a host's own.

use kardia_core::Result;

use crate::guest::{CallControl, GuestHost};
#[cfg(feature = "process")]
use crate::process::GuestHandle;

/// An export of the guest ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Export {
    Meta,
    Apply,
    Call,
    Dispose,
}

impl Export {
    /// The export's name: `meta`, `apply`, `call` or `dispose`.
    pub fn name(self) -> &'static str {
        match self {
            Export::Meta => "meta",
            Export::Apply => "apply",
            Export::Call => "call",
            Export::Dispose => "dispose",
        }
    }
}

/// Guest code of one module version, instantiated once per fiber.
///
/// [`guest_module`](crate::guest_module) and [`guest_plugin`](crate::guest_plugin)
/// turn any implementation into module code or a plugin, so a host can run
/// guests over a transport of its own.
pub trait GuestCode: Send + Sync + 'static {
    /// Starts an instance whose host calls act on `host`. It may block: the
    /// bridge calls it off the async workers.
    fn instantiate(&self, host: GuestHost) -> Result<Box<dyn GuestInstance>>;
}

/// A running guest, owned by one fiber. The bridge calls it from one thread
/// at a time, off the async workers; dropping it ends the guest.
pub trait GuestInstance: Send + 'static {
    /// Calls `export` with `input` (UTF-8 JSON, empty for none) and returns
    /// what the guest wrote: the export's result, or the guest's error. Host
    /// calls the guest makes meanwhile act on the instance's [`GuestHost`].
    ///
    /// It blocks until the guest is done. A transport that can stop its guest
    /// stops it at `control`'s deadline (or its own default), failing this and
    /// every later call, and drops the result of a cancelled call.
    fn call(&mut self, export: Export, input: Vec<u8>, control: &CallControl) -> Result<Vec<u8>>;

    /// The process running the guest, for a transport that has one.
    #[cfg(feature = "process")]
    fn handle(&self) -> Option<GuestHandle> {
        None
    }
}
