//! A guest speaking `kardia-guest/2` by hand, as guests built with
//! kardia-guest 0.2 do: it answers the handshake with `ready`, then
//! exchanges one export at a time, every message tagged with its `id`.
//! It provides `legacy`, whose calls answer their request.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn main() {
    let mut lines = std::io::stdin().lock().lines();
    let Some(Ok(first)) = lines.next() else { return };
    let Ok(request) = serde_json::from_str::<Value>(&first) else { return };
    if request["export"] != "hello" {
        // no handshake: answer like any unknown export
        send(json!({ "done": 1 }));
        return;
    }
    send(json!({ "ready": "kardia-guest/2" }));
    while let Some(Ok(line)) = lines.next() {
        let Ok(request) = serde_json::from_str::<Value>(&line) else { return };
        // answered between exports only: this guest runs them on its one thread
        if let Some(ping) = request.get("ping") {
            send(json!({ "pong": ping }));
            continue;
        }
        let id = request["id"].clone();
        let status = match request["export"].as_str().unwrap_or_default() {
            "meta" => {
                send(json!({ "id": id, "output": meta().to_string() }));
                0
            }
            "apply" => {
                send(json!({ "id": id, "call": "provide", "key": "legacy", "value": { "protocol": 2 } }));
                drop(lines.next());
                0
            }
            "call" => {
                send(json!({ "id": id, "output": request["input"].to_string() }));
                0
            }
            "dispose" => {
                send(json!({ "id": id, "done": 0 }));
                return;
            }
            _ => 1,
        };
        send(json!({ "id": id, "done": status }));
    }
}

fn meta() -> Value {
    json!({ "plugin": { "name": "legacy", "provide": ["legacy"] } })
}

fn send(message: Value) {
    let mut stdout = std::io::stdout().lock();
    if writeln!(stdout, "{message}").and_then(|()| stdout.flush()).is_err() {
        std::process::exit(1);
    }
}
