//! The threads kardia-hmr starts: a panic on one is caught and becomes the
//! failure of the guest it serves.

use std::any::Any;
use std::cell::Cell;
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::thread;

thread_local! {
    static CONTAINED: Cell<bool> = const { Cell::new(false) };
}

/// Whether a panic raised now on this thread is caught by kardia-hmr: true
/// on the threads it starts (a process guest's `guest-stdin`,
/// `guest-stdout` and `guest-stderr` threads, the `wasm-epoch` thread),
/// where a panic becomes the failure of the guest the thread serves.
///
/// An application whose panic hook aborts on a panic it does not know to be
/// caught asks this too.
pub fn is_panic_contained() -> bool {
    CONTAINED.try_with(Cell::get).unwrap_or(false)
}

/// Starts a thread running `body`; a panic in it is caught and handed to
/// `failed` as its message, and a panic in `failed` is dropped.
pub(crate) fn spawn(
    name: &str,
    body: impl FnOnce() + Send + 'static,
    failed: impl FnOnce(String) + Send + 'static,
) -> io::Result<()> {
    let thread = name.to_string();
    thread::Builder::new()
        .name(thread.clone())
        .spawn(move || {
            let _ = CONTAINED.try_with(|contained| contained.set(true));
            if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(body)) {
                let reason = format!("the {thread} thread panicked: {}", message(&*payload));
                let _ = panic::catch_unwind(AssertUnwindSafe(|| failed(reason)));
            }
        })
        .map(drop)
}

/// The message of a panic payload.
pub(crate) fn message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|text| text.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}
