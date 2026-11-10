//! The dynamic-library guest ABI under the `kardia_*` names only, as a guest
//! built for newer hosts may export it.

use kardia_guest::__private::{self, HostApi, State};

use crate::Greeter;

#[unsafe(no_mangle)]
pub extern "C" fn kardia_new() -> State {
    __private::dylib_new::<Greeter>()
}

/// # Safety
/// Called by the host only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kardia_meta(_: State, host: *const HostApi) -> i32 {
    unsafe { __private::dylib_meta::<Greeter>(host) }
}

/// # Safety
/// Called by the host only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kardia_apply(state: State, host: *const HostApi, len: usize) -> i32 {
    unsafe { __private::dylib_apply::<Greeter>(state, host, len) }
}

/// # Safety
/// Called by the host only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kardia_call(state: State, host: *const HostApi, len: usize) -> i32 {
    unsafe { __private::dylib_call::<Greeter>(state, host, len) }
}

/// # Safety
/// Called by the host only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kardia_dispose(state: State, host: *const HostApi) {
    unsafe { __private::dylib_dispose::<Greeter>(state, host) }
}
