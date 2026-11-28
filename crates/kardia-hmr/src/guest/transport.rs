//! The traits a guest transport implements: the WASM, dylib and process
//! backends, or a host's own.

#[cfg(any(feature = "wasm", feature = "dylib"))]
use kardia_core::Error;
use kardia_core::Result;
#[cfg(any(feature = "wasm", feature = "dylib"))]
use parking_lot::Mutex;
use serde_json::Value;

#[cfg(any(feature = "wasm", feature = "dylib"))]
use crate::guest::bridge::timed_out;
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

    /// Whether the export reads or changes the instance: `meta`, `apply` and
    /// `dispose` do, so they run one at a time; `call` only answers a
    /// request, so a transport that can multiplex may run several at once.
    pub fn is_lifecycle(self) -> bool {
        !matches!(self, Export::Call)
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

/// A running guest, owned by one fiber. The bridge calls it off the async
/// workers: the lifecycle exports (`meta`, `apply`, `dispose`) one at a time,
/// `call` as many at once as the transport allows; dropping it ends the guest.
pub trait GuestInstance: Send + Sync + 'static {
    /// Calls `export` with `input` (UTF-8 JSON, empty for none) and returns
    /// what the guest wrote: the export's result, or the guest's error. Host
    /// calls the guest makes meanwhile act on the instance's [`GuestHost`].
    /// Lifecycle exports never overlap with any other call; `call` may, for
    /// a transport that multiplexes, and an implementation that cannot hold
    /// that serializes them itself.
    ///
    /// It blocks until the guest is done. A transport that can stop its guest
    /// stops it at `control`'s deadline (or its own default), failing this and
    /// every later call, and drops the result of a cancelled call.
    fn call(&self, export: Export, input: Vec<u8>, control: &CallControl) -> Result<Vec<u8>>;

    /// Sends an event to the guest, outside of any export, while the instance
    /// runs; the guest receives it through its own callback. A transport
    /// without a channel to a running guest (WASM, dylib) refuses it.
    fn emit(&self, event: &str, payload: Value) -> Result<()>;

    /// The process running the guest, for a transport that has one.
    #[cfg(feature = "process")]
    fn handle(&self) -> Option<GuestHandle> {
        None
    }
}

/// An instance that runs one call at a time in the host's own thread and
/// has no channel to its guest between calls (WASM, dylib): its state stays
/// behind a lock, as it did behind the slot's, and no event can reach it.
#[cfg(any(feature = "wasm", feature = "dylib"))]
pub(crate) struct Serial<T>(Mutex<T>);

#[cfg(any(feature = "wasm", feature = "dylib"))]
impl<T: Send + 'static> Serial<T> {
    pub(crate) fn new(inner: T) -> Self {
        Self(Mutex::new(inner))
    }
}

#[cfg(any(feature = "wasm", feature = "dylib"))]
impl<T: CallLocked> GuestInstance for Serial<T> {
    fn call(&self, export: Export, input: Vec<u8>, control: &CallControl) -> Result<Vec<u8>> {
        // waiting for the call ahead: this one fails, the guest is not stopped
        let mut inner = match control.deadline() {
            Some(deadline) => self.0.try_lock_until(deadline).ok_or_else(timed_out)?,
            None => self.0.lock(),
        };
        inner.call_locked(export, input, control)
    }

    fn emit(&self, _: &str, _: Value) -> Result<()> {
        Err(Error::msg("this guest cannot receive events between its calls"))
    }
}

/// What [`Serial`] wraps: the transport's own call, on its locked state.
#[cfg(any(feature = "wasm", feature = "dylib"))]
pub(crate) trait CallLocked: Send + 'static {
    fn call_locked(&mut self, export: Export, input: Vec<u8>, control: &CallControl) -> Result<Vec<u8>>;
}
