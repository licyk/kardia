//! A process guest, driven by `tests/serve.rs`: it provides `echo`, whose
//! calls answer their request; `sleep` waits for `args` milliseconds unless
//! the host cancels it; `events` answers the events the host sent so far,
//! each with what a host kept past its export saw of `echo` when it arrived.
//!
//! ```sh
//! cargo run -p kardia-guest --example process_guest
//! ```

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kardia_guest::{Guest, Host, Map, Meta, Request, Result, Value, json};

struct Echo {
    events: Arc<Mutex<Vec<(String, Value)>>>,
}

impl Guest for Echo {
    type Config = Value;

    fn meta() -> Meta {
        Meta::plugin("echo").provide("echo")
    }

    fn apply(host: &Host, config: Value, _: Map<String, Value>) -> Result<Self> {
        host.provide("echo", &json!({ "config": config }))?;
        let events = Arc::new(Mutex::new(Vec::new()));
        Ok(Echo { events })
    }

    fn call(&self, host: &Host, request: Request) -> Result<Value> {
        match request.method.as_str() {
            "sleep" => {
                let until = Instant::now() + Duration::from_millis(request.args.as_u64().unwrap_or_default());
                while Instant::now() < until {
                    if host.cancelled() {
                        return Ok(json!("cancelled"));
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Ok(json!("slept"))
            }
            "events" => Ok(json!(self.events.lock().unwrap().clone())),
            // the host of a detached `Host`: a view through it needs no export
            "detach" => Ok(host.detached().view("echo").unwrap_or_else(|error| json!(error.message()))),
            _ => Ok(json!(request)),
        }
    }

    fn event(&self, host: &Host, name: &str, payload: Value) {
        // the host hands an event its own host, which reaches the host outside
        // any export: a call through it is answered while no export runs
        let seen = host.view("echo").unwrap_or_else(|error| json!(error.message()));
        self.events.lock().unwrap().push((name.to_string(), json!({ "payload": payload, "seen": seen })));
    }
}

fn main() {
    kardia_guest::serve::<Echo>();
}
