//! A host's own guest transport: a `GuestCode` whose instance panics on
//! request, run through the bridge.

use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;
use std::time::Duration;

use kardia_core::{Context, Result};
use kardia_hmr::{
    CallControl, Export, GuestCode, GuestFailure, GuestFailureKind, GuestHost, GuestInstance, GuestMeta, GuestService,
    guest_plugin,
};
use serde_json::{Value, json};

/// Provides `panicky`, whose method `panic` panics and whose others answer
/// with their name.
struct Panicky;

impl GuestCode for Panicky {
    fn instantiate(&self, host: GuestHost) -> Result<Box<dyn GuestInstance>> {
        Ok(Box::new(Instance(host)))
    }
}

struct Instance(GuestHost);

impl GuestInstance for Instance {
    fn call(&mut self, export: Export, input: Vec<u8>, _: &CallControl) -> Result<Vec<u8>> {
        match export {
            Export::Meta => Ok(serde_json::to_vec(&json!({ "plugin": meta() }))?),
            Export::Apply => self.0.provide("panicky", json!({})).map(|()| Vec::new()),
            Export::Call => {
                let request: Value = serde_json::from_slice(&input)?;
                match request["method"].as_str() {
                    Some("panic") => panic!("the transport panics"),
                    method => Ok(serde_json::to_vec(&json!(method))?),
                }
            }
            _ => Ok(Vec::new()),
        }
    }
}

fn meta() -> GuestMeta {
    GuestMeta::new("panicky").provide("panicky")
}

async fn start(root: &Context) -> kardia_core::Svc<GuestService> {
    root.plugin_def(&guest_plugin(Arc::new(Panicky), meta()), json!({})).unwrap().settled().await.unwrap();
    root.get_as::<GuestService>("panicky").unwrap()
}

#[tokio::test]
async fn a_call_that_panicked_leaves_its_instance_callable_on_the_same_thread() {
    let root = Context::new();
    let service = start(&root).await;
    let panicked = panic::catch_unwind(AssertUnwindSafe(|| service.call("panic", json!(null))));
    assert!(panicked.is_err());
    assert_eq!(service.call("hello", json!(null)).unwrap(), json!("hello"));
}

#[test]
fn an_async_call_that_panicked_leaves_its_instance_callable_on_the_same_blocking_thread() {
    // one blocking thread: the second call runs on the thread the first one panicked on
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let root = Context::new();
        let service = start(&root).await;
        let timeout = Some(Duration::from_secs(5));
        let error = service.call_async("panic", json!(null), timeout).await.unwrap_err();
        assert_eq!(GuestFailure::of(&error).unwrap().kind, GuestFailureKind::Panicked, "{error}");
        assert_eq!(service.call_async("hello", json!(null), timeout).await.unwrap(), json!("hello"));
    });
}
