//! A bridge host that speaks JSON lines over stdin and stdout; the
//! end-to-end tests of `@kardiajs/bridge` drive it as a client would.
//!
//! * stdin: `{ "id": n, "request": { ... } }`, one per line;
//! * stdout: `{ "id": n, "ok": result }` or `{ "id": n, "error": error }`,
//!   and `{ "message": { ... } }` for messages pushed to the session.
//!
//! stdout carries the protocol, so logs go to stderr.
//!
//! The host exposes `echo` (methods `echo` and `fail`), `info` (a value) and
//! `control`, whose methods drive the host: `emit` sends an event,
//! `askClient` calls the `client.prompt` service a client provides, and
//! `restartBridge` stops and remounts the bridge plugin.

use std::io::Write;
use std::sync::Arc;

use kardia_bridge::{Bridge, BridgeError, BridgeExt, Outbox, PROTOCOL, RemoteService};
use kardia_core::{Context, Exporter, Fiber, Message, PluginDef, Service};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::main]
async fn main() -> kardia_core::Result<()> {
    let root = Context::new();
    root.add_exporter(Arc::new(Stderr))?;
    let bridge = Bridge::default();
    let mounted = Arc::new(Mutex::new(root.plugin_def(&bridge.plugin(), Value::Null)?));
    root.plugin_def(&echo(), Value::Null)?;
    root.plugin_def(&control(root.clone(), bridge.clone(), mounted), Value::Null)?;

    let stdout = Arc::new(Stdout);
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let Ok(input) = serde_json::from_str::<Value>(&line) else {
            eprintln!("ignoring malformed line: {line}");
            continue;
        };
        let (bridge, stdout) = (bridge.clone(), stdout.clone());
        // concurrently: a call may wait for a later `resolve`
        tokio::spawn(async move {
            let request = input["request"].clone();
            let outbox = (request["type"] == "open").then(|| stdout.clone() as Arc<dyn Outbox>);
            let reply = match bridge.handle(request, outbox).await {
                Ok(ok) => json!({ "id": input["id"], "ok": ok }),
                Err(error) => json!({ "id": input["id"], "error": error }),
            };
            stdout.write(&reply);
        });
    }
    Ok(())
}

fn echo() -> PluginDef {
    PluginDef::new(|ctx, _| async move {
        let bridge = ctx.get::<Bridge>()?;
        bridge.expose("info", json!({ "protocol": PROTOCOL }))?;
        bridge.expose("echo", |method: String, args: Value| async move {
            match method.as_str() {
                "echo" => Ok(args),
                "fail" => Err(BridgeError::failed(args.as_str().unwrap_or("failed").to_string())),
                _ => Err(BridgeError::invalid(format!("unknown method {method}"))),
            }
        })?;
        Ok(())
    })
    .name("echo")
    .inject([Bridge::NAME])
}

fn control(root: Context, bridge: Bridge, mounted: Arc<Mutex<Fiber>>) -> PluginDef {
    PluginDef::new(move |ctx, _| {
        let (root, bridge, mounted) = (root.clone(), bridge.clone(), mounted.clone());
        async move {
            let consumer = ctx.clone();
            ctx.get::<Bridge>()?.expose("control", move |method: String, args: Value| {
                let (root, bridge, mounted, consumer) =
                    (root.clone(), bridge.clone(), mounted.clone(), consumer.clone());
                async move {
                    match method.as_str() {
                        "emit" => {
                            bridge.emit(args["name"].as_str().unwrap_or_default(), &args["payload"]);
                            Ok(Value::Null)
                        }
                        "askClient" => {
                            let prompt = consumer
                                .live_as::<RemoteService>("client.prompt")
                                .ok_or_else(|| BridgeError::unavailable("no client provides client.prompt"))?;
                            prompt.call(args["method"].as_str().unwrap_or_default(), args["args"].clone()).await
                        }
                        "restartBridge" => {
                            tokio::spawn(async move {
                                let old = mounted.lock().clone();
                                let _ = old.dispose().await;
                                match root.plugin_def(&bridge.plugin(), Value::Null) {
                                    Ok(fiber) => *mounted.lock() = fiber,
                                    Err(error) => eprintln!("cannot remount the bridge: {error}"),
                                }
                            });
                            Ok(Value::Null)
                        }
                        _ => Err(BridgeError::invalid(format!("unknown method {method}"))),
                    }
                }
            })?;
            Ok(())
        }
    })
    .name("control")
    .inject([Bridge::NAME])
}

struct Stdout;

impl Stdout {
    fn write(&self, value: &Value) -> bool {
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "{value}").and_then(|()| stdout.flush()).is_ok()
    }
}

impl Outbox for Stdout {
    fn send(&self, message: Value) -> bool {
        self.write(&json!({ "message": message }))
    }
}

struct Stderr;

impl Exporter for Stderr {
    fn export(&self, message: &Message) {
        eprintln!("[{}] {}", message.name, message.text);
    }
}
