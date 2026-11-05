//! Provides `greeter`, whose view is `{ version, input }` and whose calls
//! answer `{ version, calls, request }`; `fail` fails, `panic` panics and
//! `exit` ends the process.

use cordis_guest::{Error, Guest, Host, Level, Map, Meta, Request, Result, Value, json};

const VERSION: &str = if cfg!(feature = "v2") { "v2" } else { "v1" };

pub struct Greeter {
    calls: u64,
}

impl Guest for Greeter {
    type Config = Value;

    fn meta() -> Meta {
        Meta::plugin("greeter").provide("greeter")
    }

    fn apply(host: &Host, config: Value, imports: Map<String, Value>) -> Result<Self> {
        let input = json!({ "config": config, "imports": imports });
        host.provide("greeter", &json!({ "version": VERSION, "input": input }))?;
        host.log(Level::Info, &format!("greeter {VERSION} applied"));
        Ok(Greeter { calls: 0 })
    }

    fn call(&mut self, _: &Host, request: Request) -> Result<Value> {
        self.calls += 1;
        match request.method.as_str() {
            "fail" => Err(Error::msg("greeter refuses")),
            "panic" => panic!("greeter panics"),
            "exit" => std::process::exit(3),
            "pid" => Ok(json!(std::process::id())),
            _ => Ok(json!({ "version": VERSION, "calls": self.calls, "request": request })),
        }
    }

    fn dispose(self, host: &Host) {
        host.log(Level::Info, &format!("greeter {VERSION} disposed"));
    }
}

cordis_guest::export!(Greeter);
