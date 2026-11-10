//! A process guest, driven by `tests/serve.rs`: it provides `echo`, whose
//! calls answer their request; `sleep` waits for `args` milliseconds unless
//! the host cancels it.
//!
//! ```sh
//! cargo run -p kardia-guest --example process_guest
//! ```

use std::time::{Duration, Instant};

use kardia_guest::{Guest, Host, Map, Meta, Request, Result, Value, json};

struct Echo;

impl Guest for Echo {
    type Config = Value;

    fn meta() -> Meta {
        Meta::plugin("echo").provide("echo")
    }

    fn apply(host: &Host, config: Value, _: Map<String, Value>) -> Result<Self> {
        host.provide("echo", &json!({ "config": config }))?;
        Ok(Echo)
    }

    fn call(&mut self, host: &Host, request: Request) -> Result<Value> {
        if request.method != "sleep" {
            return Ok(json!(request));
        }
        let until = Instant::now() + Duration::from_millis(request.args.as_u64().unwrap_or_default());
        while Instant::now() < until {
            if host.cancelled() {
                return Ok(json!("cancelled"));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(json!("slept"))
    }
}

fn main() {
    kardia_guest::serve::<Echo>();
}
