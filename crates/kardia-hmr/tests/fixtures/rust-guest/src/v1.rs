//! A guest speaking protocol version 1 by hand, as guests built with
//! kardia-guest 0.2.0 do: it answers any export it does not know, the
//! handshake of `kardia-guest/2` included, with status 1. It provides
//! `legacy`, whose calls answer their request.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn main() {
    let mut lines = std::io::stdin().lock().lines();
    while let Some(Ok(line)) = lines.next() {
        let Ok(request) = serde_json::from_str::<Value>(&line) else { return };
        let status = match request["export"].as_str().unwrap_or_default() {
            "meta" => {
                let meta = json!({ "plugin": { "name": "legacy", "provide": ["legacy"] } });
                send(json!({ "output": meta.to_string() }));
                0
            }
            "apply" => {
                send(json!({ "call": "provide", "key": "legacy", "value": { "protocol": 1 } }));
                drop(lines.next());
                0
            }
            "call" => {
                send(json!({ "output": request["input"].to_string() }));
                0
            }
            "dispose" => {
                send(json!({ "done": 0 }));
                return;
            }
            _ => 1,
        };
        send(json!({ "done": status }));
    }
}

fn send(message: Value) {
    let mut stdout = std::io::stdout().lock();
    if writeln!(stdout, "{message}").and_then(|()| stdout.flush()).is_err() {
        std::process::exit(1);
    }
}
