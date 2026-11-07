//! The bridge driven as a client would drive it, over an in-memory transport.

use std::sync::Arc;
use std::time::Duration;

use kardia_bridge::{Bridge, BridgeError, BridgeExt, BridgeOptions, Outbox, RemoteService};
use kardia_core::{Context, FiberState, PluginDef, Service};
use serde_json::{Value, json};
use tokio::sync::mpsc;

/// The client's end of a session's message stream.
struct Stream(mpsc::UnboundedSender<Value>);

impl Outbox for Stream {
    fn send(&self, message: Value) -> bool {
        self.0.send(message).is_ok()
    }
}

struct Client {
    bridge: Bridge,
    session: u64,
    messages: mpsc::UnboundedReceiver<Value>,
    opened: Value,
}

impl Client {
    async fn open(bridge: &Bridge, name: &str) -> Client {
        let (tx, messages) = mpsc::unbounded_channel();
        let opened = bridge
            .handle(json!({ "type": "open", "protocol": 1, "client": name }), Some(Arc::new(Stream(tx))))
            .await
            .expect("open");
        Client { bridge: bridge.clone(), session: opened["session"].as_u64().unwrap(), messages, opened }
    }

    async fn request(&self, mut request: Value) -> Result<Value, BridgeError> {
        request["session"] = json!(self.session);
        self.bridge.handle(request, None).await
    }

    async fn next(&mut self) -> Value {
        tokio::time::timeout(Duration::from_secs(5), self.messages.recv())
            .await
            .expect("a message")
            .expect("open stream")
    }

    /// The names in the next `services` message.
    async fn next_services(&mut self) -> Vec<String> {
        let message = self.next().await;
        assert_eq!(message["type"], "services", "{message}");
        names(&message["services"])
    }
}

fn names(services: &Value) -> Vec<String> {
    services.as_array().unwrap().iter().map(|service| service["name"].as_str().unwrap().to_string()).collect()
}

/// A root running the bridge.
async fn host(options: BridgeOptions) -> (Context, Bridge) {
    let root = Context::new();
    let bridge = Bridge::new(options);
    root.plugin_def(&bridge.plugin(), Value::Null).unwrap().settled().await.unwrap();
    (root, bridge)
}

/// A plugin exposing `echo`, whose `echo` method returns its arguments.
fn echo_plugin() -> PluginDef {
    PluginDef::new(|ctx, _| async move {
        let bridge = ctx.get::<Bridge>()?;
        bridge.expose("echo", |method: String, args: Value| async move {
            match method.as_str() {
                "echo" => Ok(args),
                _ => Err(BridgeError::failed(format!("no {method}"))),
            }
        })?;
        Ok(())
    })
    .name("echo")
    .inject([Bridge::NAME])
}

#[tokio::test]
async fn requests_fail_while_the_bridge_is_not_running() {
    let bridge = Bridge::default();
    let error = bridge.handle(json!({ "type": "open", "protocol": 1, "client": "main" }), None).await.unwrap_err();
    assert_eq!(error, BridgeError::unavailable("the bridge is not running"));
}

#[tokio::test]
async fn open_checks_the_protocol_and_needs_a_stream() {
    let (_root, bridge) = host(BridgeOptions::default()).await;
    let (tx, _rx) = mpsc::unbounded_channel();
    let stream: Arc<dyn Outbox> = Arc::new(Stream(tx));
    let error =
        bridge.handle(json!({ "type": "open", "protocol": 2, "client": "main" }), Some(stream)).await.unwrap_err();
    assert!(matches!(error, BridgeError::Invalid { .. }), "{error:?}");
    let error = bridge.handle(json!({ "type": "open", "protocol": 1, "client": "main" }), None).await.unwrap_err();
    assert!(matches!(error, BridgeError::Invalid { .. }), "{error:?}");
    let error = bridge.handle(json!({ "type": "nonsense" }), None).await.unwrap_err();
    assert!(matches!(error, BridgeError::Invalid { .. }), "{error:?}");
}

#[tokio::test]
async fn mirrors_exposed_services_and_calls_them() {
    let (root, bridge) = host(BridgeOptions::default()).await;
    let mut client = Client::open(&bridge, "main").await;
    assert_eq!(client.opened["services"], json!([]));

    let echo = root.plugin_def(&echo_plugin(), Value::Null).unwrap();
    echo.settled().await.unwrap();
    assert_eq!(client.next_services().await, ["echo"]);

    let call = json!({ "type": "call", "service": "echo", "method": "echo", "args": { "a": 1 } });
    assert_eq!(client.request(call.clone()).await.unwrap(), json!({ "a": 1 }));
    let failing = json!({ "type": "call", "service": "echo", "method": "nope" });
    assert_eq!(client.request(failing).await.unwrap_err(), BridgeError::failed("no nope"));

    echo.dispose().await.unwrap();
    assert!(client.next_services().await.is_empty());
    assert_eq!(client.request(call).await.unwrap_err(), BridgeError::unavailable("service echo is not available"));

    // a new session starts from the current snapshot
    let other = Client::open(&bridge, "other").await;
    assert_eq!(other.opened["services"], json!([]));
}

#[tokio::test]
async fn exposes_values_with_a_new_id_per_exposure() {
    let (root, bridge) = host(BridgeOptions::default()).await;
    let mut client = Client::open(&bridge, "main").await;
    let status = PluginDef::new(|ctx, config| async move {
        ctx.get::<Bridge>()?.expose("status", config)?;
        Ok(())
    })
    .inject([Bridge::NAME]);
    let fiber = root.plugin_def(&status, json!({ "ready": true })).unwrap();
    fiber.settled().await.unwrap();
    let first = client.next().await;
    assert_eq!(first["services"][0]["value"], json!({ "ready": true }));
    fiber.update(json!({ "ready": false })).unwrap().await.unwrap();
    assert!(client.next_services().await.is_empty());
    let second = client.next().await;
    assert_eq!(second["services"][0]["value"], json!({ "ready": false }));
    assert_ne!(first["services"][0]["id"], second["services"][0]["id"]);
}

#[tokio::test]
async fn delivers_events_to_every_session() {
    let (_root, bridge) = host(BridgeOptions::default()).await;
    let mut a = Client::open(&bridge, "a").await;
    let mut b = Client::open(&bridge, "b").await;
    bridge.emit("agent/status", &json!({ "busy": true }));
    let expected = json!({ "type": "event", "name": "agent/status", "payload": { "busy": true } });
    assert_eq!(a.next().await, expected);
    assert_eq!(b.next().await, expected);
}

#[tokio::test]
async fn client_services_run_host_consumers_while_the_session_lasts() {
    let (root, bridge) = host(BridgeOptions::default()).await;
    let mut client = Client::open(&bridge, "main").await;

    let consumer = root.plugin_def(&PluginDef::new(|_, _| async { Ok(()) }).inject(["picker"]), Value::Null).unwrap();
    client.request(json!({ "type": "provide", "service": "picker", "value": { "kind": "files" } })).await.unwrap();
    consumer.settled().await.unwrap();
    assert_eq!(consumer.state(), FiberState::Active);
    assert_eq!(root.view("picker").unwrap(), json!({ "kind": "files" }));

    // the host calls into the client, which answers
    let picker = root.get_as::<RemoteService>("picker").unwrap();
    let call = tokio::spawn(async move { picker.call("pick", json!({ "multiple": true })).await });
    let request = client.next().await;
    assert_eq!(request["type"], "request");
    assert_eq!(
        (&request["service"], &request["method"], &request["args"]),
        (&json!("picker"), &json!("pick"), &json!({ "multiple": true }))
    );
    client.request(json!({ "type": "resolve", "id": request["id"], "value": ["a.txt"] })).await.unwrap();
    assert_eq!(call.await.unwrap().unwrap(), json!(["a.txt"]));

    // a rejection reaches the host caller
    let picker = root.get_as::<RemoteService>("picker").unwrap();
    let call = tokio::spawn(async move { picker.call("pick", Value::Null).await });
    let request = client.next().await;
    let error = json!({ "type": "failed", "message": "cancelled by the user" });
    client.request(json!({ "type": "reject", "id": request["id"], "error": error })).await.unwrap();
    assert_eq!(call.await.unwrap().unwrap_err(), BridgeError::failed("cancelled by the user"));

    client.request(json!({ "type": "withdraw", "service": "picker" })).await.unwrap();
    consumer.settled().await.unwrap();
    assert_eq!(consumer.state(), FiberState::Pending);

    client.request(json!({ "type": "provide", "service": "picker" })).await.unwrap();
    consumer.settled().await.unwrap();
    assert_eq!(consumer.state(), FiberState::Active);
    client.request(json!({ "type": "close" })).await.unwrap();
    consumer.settled().await.unwrap();
    assert_eq!(consumer.state(), FiberState::Pending);
}

#[tokio::test]
async fn calls_into_a_client_time_out() {
    let (root, bridge) = host(BridgeOptions { timeout: Duration::from_millis(50) }).await;
    let client = Client::open(&bridge, "main").await;
    client.request(json!({ "type": "provide", "service": "slow" })).await.unwrap();
    let slow = root.get_as::<RemoteService>("slow").unwrap();
    let error = slow.call("never", Value::Null).await.unwrap_err();
    assert!(matches!(error, BridgeError::Timeout { .. }), "{error:?}");
}

#[tokio::test]
async fn a_new_session_of_the_same_client_replaces_the_old_one() {
    let (root, bridge) = host(BridgeOptions::default()).await;
    let mut old = Client::open(&bridge, "main").await;
    old.request(json!({ "type": "provide", "service": "picker" })).await.unwrap();
    let picker = root.get_as::<RemoteService>("picker").unwrap();

    let new = Client::open(&bridge, "main").await;
    assert_eq!(old.next().await, json!({ "type": "closed", "reason": "replaced" }));
    assert_eq!(
        picker.call("pick", Value::Null).await.unwrap_err(),
        BridgeError::unavailable("the client of picker is gone")
    );
    assert!(matches!(
        old.request(json!({ "type": "provide", "service": "x" })).await,
        Err(BridgeError::Unavailable { .. })
    ));
    // the realm is free for the new session
    new.request(json!({ "type": "provide", "service": "picker" })).await.unwrap();
}

#[tokio::test]
async fn closes_sessions_when_the_bridge_stops() {
    let root = Context::new();
    let bridge = Bridge::default();
    let fiber = root.plugin_def(&bridge.plugin(), Value::Null).unwrap();
    fiber.settled().await.unwrap();
    let mut client = Client::open(&bridge, "main").await;
    fiber.dispose().await.unwrap();
    assert_eq!(client.next().await, json!({ "type": "closed", "reason": "stopped" }));
    let error = client.request(json!({ "type": "call", "service": "echo", "method": "echo" })).await.unwrap_err();
    assert_eq!(error, BridgeError::unavailable("the bridge is not running"));
}

#[tokio::test]
async fn close_client_ends_its_sessions() {
    let (root, bridge) = host(BridgeOptions::default()).await;
    let client = Client::open(&bridge, "main").await;
    let consumer = root.plugin_def(&PluginDef::new(|_, _| async { Ok(()) }).inject(["picker"]), Value::Null).unwrap();
    client.request(json!({ "type": "provide", "service": "picker" })).await.unwrap();
    consumer.settled().await.unwrap();
    bridge.close_client("main");
    tokio::time::timeout(Duration::from_secs(5), async {
        while consumer.state() != FiberState::Pending {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[test]
fn handle_runs_on_the_bridge_runtime_from_any_thread() {
    let host_runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let (_root, bridge) = host_runtime.block_on(host(BridgeOptions::default()));
    let echo_root = _root.clone();
    host_runtime
        .block_on(async { echo_root.plugin_def(&echo_plugin(), Value::Null).unwrap().settled().await.unwrap() });

    // a caller on another runtime, as a Tauri command would be
    let caller = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let result = caller.block_on(async {
        let client = Client::open(&bridge, "main").await;
        client.request(json!({ "type": "call", "service": "echo", "method": "echo", "args": 7 })).await
    });
    assert_eq!(result.unwrap(), json!(7));
}
