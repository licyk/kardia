//! Provides `greeter`, whose view is `{ version, input }` and whose calls
//! answer `{ version, calls, request }`; `fail` fails, `panic` panics,
//! `exit` ends the process and `print` writes to stdout; `view` and `invoke`
//! forward their arguments to the host.

use kardia_guest::{Error, Guest, Host, Level, Map, Meta, Request, Result, Value, json};

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

    fn call(&mut self, host: &Host, request: Request) -> Result<Value> {
        self.calls += 1;
        let args = &request.args;
        match request.method.as_str() {
            "view" => host.view(args.as_str().unwrap_or_default()),
            "invoke" => {
                let text = |key: &str| args[key].as_str().unwrap_or_default().to_string();
                host.invoke(&text("key"), &text("method"), &args["args"])
            }
            "print" => {
                println!("not a protocol message");
                Ok(json!(null))
            }
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

kardia_guest::export!(Greeter);
