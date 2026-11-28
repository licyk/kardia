//! A guest reaching the host's own implementation through `invoke`.

use std::sync::Arc;

use kardia_core::{Context, PluginDef, Result};
use kardia_hmr::{
    CallControl, Export, GuestCode, GuestHost, GuestInstance, GuestMeta, GuestService, HostAction, guest_plugin,
    register_action,
};
use serde_json::{Value, json};

/// Calls the host action `native` from inside `invoke`, and answers with its
/// result.
struct Invoking;

impl GuestCode for Invoking {
    fn instantiate(&self, host: GuestHost) -> Result<Box<dyn GuestInstance>> {
        Ok(Box::new(Instance(host)))
    }
}

struct Instance(GuestHost);

impl GuestInstance for Instance {
    fn call(&mut self, export: Export, input: Vec<u8>, _: &CallControl) -> Result<Vec<u8>> {
        match export {
            Export::Meta => Ok(serde_json::to_vec(&json!({ "plugin": meta() }))?),
            Export::Apply => self.0.provide("caller", json!({})).map(|()| Vec::new()),
            Export::Call => {
                let request: Value = serde_json::from_slice(&input)?;
                let args = request["args"].clone();
                let answer = self.0.invoke("native", "ping", args)?;
                Ok(serde_json::to_vec(&answer)?)
            }
            _ => Ok(Vec::new()),
        }
    }
}

fn meta() -> GuestMeta {
    // A guest declares what it reaches, exactly as it does for `view`; the host
    // action is one more declared dependency.
    GuestMeta::new("caller").provide("caller").inject("native")
}

struct Native;
impl HostAction for Native {
    fn call(&self, method: &str, args: Value) -> Result<Value> {
        Ok(json!({ "method": method, "args": args }))
    }
}

#[tokio::test]
async fn a_guest_reaches_a_host_action_through_invoke() {
    let root = Context::new();
    // The host registers its own implementation under `native`, which is not a
    // guest service.
    root.plugin_def(
        &PluginDef::new(|ctx, _| async move {
            register_action(&ctx, "native", Native)?;
            Ok(())
        }),
        json!({}),
    )
    .unwrap()
    .settled()
    .await
    .unwrap();
    root.plugin_def(&guest_plugin(Arc::new(Invoking), meta()), json!({})).unwrap().settled().await.unwrap();

    let service = root.get_as::<GuestService>("caller").unwrap();
    assert_eq!(service.call("anything", json!({ "n": 7 })).unwrap(), json!({ "method": "ping", "args": { "n": 7 } }));
}
