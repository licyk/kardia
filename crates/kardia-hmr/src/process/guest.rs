//! Process guests: a command, how it is spawned and the limits of its calls.

use std::sync::Arc;
use std::time::Duration;

use kardia_core::{Error, PluginDef, Result};

use crate::graph::{ModuleCode, ModuleSource};
use crate::guest::{GuestCode, GuestHost, GuestInstance, GuestMeta, guest_module, guest_plugin};
use crate::process::instance::{Options, ProcessInstance};
use crate::process::{GuestCommand, Spawner, StdSpawner};

/// How long a guest has to answer the handshake by default.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// The longest message a guest may send by default.
const FRAME_LIMIT: usize = 16 * 1024 * 1024;

/// A guest run as a child process: one process per instance, speaking the
/// guest ABI as JSON lines over its stdin and stdout (protocol version 1 or
/// `kardia-guest/2`, see the crate docs).
///
/// Its [`Spawner`] starts the process ([`StdSpawner`] by default). A call
/// that outlives its deadline kills the process, as does a message that is
/// not JSON or longer than the frame limit; a process that ends fails its
/// fiber (see [`GuestExited`](crate::GuestExited)).
#[derive(Clone)]
pub struct ProcessGuest {
    command: GuestCommand,
    spawner: Arc<dyn Spawner>,
    timeout: Option<Duration>,
    handshake_timeout: Duration,
    frame_limit: usize,
}

impl ProcessGuest {
    pub fn new(command: GuestCommand) -> Self {
        Self {
            command,
            spawner: Arc::new(StdSpawner),
            timeout: None,
            handshake_timeout: HANDSHAKE_TIMEOUT,
            frame_limit: FRAME_LIMIT,
        }
    }

    pub fn spawner(mut self, spawner: impl Spawner) -> Self {
        self.spawner = Arc::new(spawner);
        self
    }

    /// How long each export may take when its caller sets no deadline: past
    /// it, the process is killed. None by default, but `dispose` is bounded
    /// to 5 seconds then.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// How long the guest has to answer the handshake (10 seconds by default).
    pub fn handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = timeout;
        self
    }

    /// The longest line the guest may send, in bytes (16 MiB by default).
    pub fn frame_limit(mut self, bytes: usize) -> Self {
        self.frame_limit = bytes;
        self
    }

    /// The module the guest exports, read from its `meta` (this starts a
    /// process to ask).
    pub fn module(&self, id: &str) -> Result<ModuleCode> {
        guest_module(id, Arc::new(self.clone()))
    }

    /// The guest's plugin, with metadata from the caller: making it starts
    /// nothing, and each process must declare the same metadata when it
    /// starts.
    pub fn plugin(&self, meta: GuestMeta) -> PluginDef {
        guest_plugin(Arc::new(self.clone()), meta)
    }
}

impl GuestCode for ProcessGuest {
    fn instantiate(&self, host: GuestHost) -> Result<Box<dyn GuestInstance>> {
        let process = self.spawner.spawn(&self.command).map_err(|error| {
            Error::msg(format!("cannot start guest {}: {error}", self.command.get_program().to_string_lossy()))
        })?;
        let options =
            Options { timeout: self.timeout, handshake_timeout: self.handshake_timeout, frame_limit: self.frame_limit };
        Ok(Box::new(ProcessInstance::start(process, host, &options)?))
    }
}

/// Runs executables that serve the guest ABI on stdin/stdout (see the crate
/// docs); module ids are their paths, run without arguments by
/// [`StdSpawner`]. Use [`ProcessGuest::module`] in a source of your own for
/// arguments, a spawner or limits.
///
/// Replace an executable by renaming a new file over it: a running one
/// cannot be written to, and one held open for writing cannot be started.
#[derive(Default)]
pub struct ProcessSource;

impl ModuleSource for ProcessSource {
    fn load(&self, id: &str) -> Result<ModuleCode> {
        ProcessGuest::new(GuestCommand::new(id)).module(id)
    }
}
