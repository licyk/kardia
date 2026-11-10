//! The `export!` macro, which exports a guest with the ABI of the target.

/// Exports `$guest` with the guest ABI of the target: WASM exports on
/// `wasm32`, C symbols for a dynamic library elsewhere. Each entry is exported
/// as `cordis_*`, which every host looks up, and as `kardia_*`; WASM host
/// functions are imported from the module `cordis`, which every host provides.
#[macro_export]
macro_rules! export {
    ($guest:ty) => {
        #[cfg(target_family = "wasm")]
        const _: () = {
            #[unsafe(no_mangle)]
            pub extern "C" fn cordis_meta() -> i32 {
                $crate::__private::wasm_meta::<$guest>()
            }

            #[unsafe(no_mangle)]
            pub extern "C" fn cordis_apply(len: usize) -> i32 {
                $crate::__private::wasm_apply::<$guest>(len)
            }

            #[unsafe(no_mangle)]
            pub extern "C" fn cordis_call(len: usize) -> i32 {
                $crate::__private::wasm_call::<$guest>(len)
            }

            #[unsafe(no_mangle)]
            pub extern "C" fn cordis_dispose() {
                $crate::__private::wasm_dispose::<$guest>()
            }

            // the same exports under the `kardia_*` names
            #[unsafe(no_mangle)]
            pub extern "C" fn kardia_meta() -> i32 {
                cordis_meta()
            }

            #[unsafe(no_mangle)]
            pub extern "C" fn kardia_apply(len: usize) -> i32 {
                cordis_apply(len)
            }

            #[unsafe(no_mangle)]
            pub extern "C" fn kardia_call(len: usize) -> i32 {
                cordis_call(len)
            }

            #[unsafe(no_mangle)]
            pub extern "C" fn kardia_dispose() {
                cordis_dispose()
            }
        };

        #[cfg(not(target_family = "wasm"))]
        const _: () = {
            use $crate::__private::{HostApi, State};

            #[unsafe(no_mangle)]
            pub extern "C" fn cordis_new() -> State {
                $crate::__private::dylib_new::<$guest>()
            }

            /// # Safety
            /// Called by the host only.
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn cordis_meta(_: State, host: *const HostApi) -> i32 {
                unsafe { $crate::__private::dylib_meta::<$guest>(host) }
            }

            /// # Safety
            /// Called by the host only.
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn cordis_apply(state: State, host: *const HostApi, len: usize) -> i32 {
                unsafe { $crate::__private::dylib_apply::<$guest>(state, host, len) }
            }

            /// # Safety
            /// Called by the host only.
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn cordis_call(state: State, host: *const HostApi, len: usize) -> i32 {
                unsafe { $crate::__private::dylib_call::<$guest>(state, host, len) }
            }

            /// # Safety
            /// Called by the host only.
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn cordis_dispose(state: State, host: *const HostApi) {
                unsafe { $crate::__private::dylib_dispose::<$guest>(state, host) }
            }

            // the same symbols under the `kardia_*` names

            #[unsafe(no_mangle)]
            pub extern "C" fn kardia_new() -> State {
                cordis_new()
            }

            /// # Safety
            /// Called by the host only.
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn kardia_meta(state: State, host: *const HostApi) -> i32 {
                unsafe { cordis_meta(state, host) }
            }

            /// # Safety
            /// Called by the host only.
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn kardia_apply(state: State, host: *const HostApi, len: usize) -> i32 {
                unsafe { cordis_apply(state, host, len) }
            }

            /// # Safety
            /// Called by the host only.
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn kardia_call(state: State, host: *const HostApi, len: usize) -> i32 {
                unsafe { cordis_call(state, host, len) }
            }

            /// # Safety
            /// Called by the host only.
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn kardia_dispose(state: State, host: *const HostApi) {
                unsafe { cordis_dispose(state, host) }
            }
        };
    };
}
