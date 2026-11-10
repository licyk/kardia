//! Executables as guests: each instance is a child process speaking the
//! guest ABI as JSON lines over its stdin and stdout (see the crate docs),
//! so a crashing guest fails only its own fiber.

mod command;
mod guest;
mod handle;
mod instance;
mod pipes;

pub use command::{GuestChild, GuestCommand, GuestProcess, Spawner, StdSpawner};
pub use guest::{ProcessGuest, ProcessSource};
pub use handle::{ExitCause, GuestExit, GuestExited, GuestHandle, GuestStarted};
