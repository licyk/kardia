//! Host-side methods a guest may call from its `invoke`.

use kardia_core::{Context, Result};
use serde_json::Value;

/// A service the host implements, callable from a guest by name.
///
/// A guest already reaches another guest through [`GuestService`], which is a
/// JSON view a plugin provides. This is the other direction: the host registers
/// its own implementation under a key, and a guest calls it with `invoke` like
/// any other service. Registering is an effect of the calling plugin.
///
/// [`GuestService`]: crate::guest::GuestService
pub trait HostAction: Send + Sync + 'static {
    /// Calls `method` with the JSON arguments the guest sent.
    fn call(&self, method: &str, args: Value) -> Result<Value>;
}

/// Registers `action` under `key`, for as long as the calling plugin runs.
pub fn register(ctx: &Context, key: &str, action: impl HostAction) -> Result<kardia_core::EffectHandle> {
    ctx.provide_as(key, Registered(std::sync::Arc::new(action)))
}

/// The value stored under the key. A guest reaches it by key, so the concrete
/// action type never has to be named again.
pub(crate) struct Registered(pub(crate) std::sync::Arc<dyn HostAction>);
