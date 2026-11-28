//! Provides `greeter`, whose view is `{ version, input }` and whose calls
//! answer `{ version, calls, request }`; `fail` fails, `panic` panics,
//! `exit` ends the process and `print` writes to stdout; `view` and `invoke`
//! forward their arguments to the host; `events` answers the events the host
//! sent so far, each with what the host's view of `greeter` was when it
//! arrived; `detach` answers what a host kept from `apply` sees of `greeter`.
//!
//! As a process: `args`, `env` and `cwd` answer what the process got;
//! `sleep` sleeps for `args` milliseconds unless the host cancels it, and
//! `cancelled` counts the calls it cancelled; `hang` never answers; `big`
//! answers a string of `args` bytes; `stderr` writes `args` to stderr;
//! `exit_later` writes to stderr and exits with status 3 after it answered.
//! The plugin is named by `GREETER_NAME` if set. With the `kardia-names`
//! feature, the dynamic library exports only the `kardia_*` symbols.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kardia_guest::{Error, Guest, Host, Level, Map, Meta, Request, Result, Value, json};

const VERSION: &str = if cfg!(feature = "v2") { "v2" } else { "v1" };

pub struct Greeter {
    calls: AtomicU64,
    cancelled: AtomicU64,
    events: Arc<Mutex<Vec<(String, Value)>>>,
    detached: Option<Host>,
}

impl Guest for Greeter {
    type Config = Value;

    fn meta() -> Meta {
        Meta::plugin(std::env::var("GREETER_NAME").unwrap_or_else(|_| "greeter".into())).provide("greeter")
    }

    fn apply(host: &Host, config: Value, imports: Map<String, Value>) -> Result<Self> {
        let input = json!({ "config": config, "imports": imports });
        host.provide("greeter", &json!({ "version": VERSION, "input": input }))?;
        host.log(Level::Info, &format!("greeter {VERSION} applied"));
        eprintln!("greeter {VERSION} started");
        Ok(Greeter {
            calls: AtomicU64::new(0),
            cancelled: AtomicU64::new(0),
            events: Arc::default(),
            detached: Some(host.detached()),
        })
    }

    fn call(&self, host: &Host, request: Request) -> Result<Value> {
        let calls = self.calls.fetch_add(1, Ordering::AcqRel) + 1;
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
            "args" => Ok(json!(std::env::args().skip(1).collect::<Vec<_>>())),
            "env" => Ok(json!(std::env::var(args.as_str().unwrap_or_default()).ok())),
            "cwd" => Ok(json!(std::env::current_dir()?)),
            "sleep" => {
                let until = Instant::now() + Duration::from_millis(args.as_u64().unwrap_or_default());
                while Instant::now() < until {
                    if host.cancelled() {
                        self.cancelled.fetch_add(1, Ordering::AcqRel);
                        return Ok(json!("cancelled"));
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Ok(json!("slept"))
            }
            "cancelled" => Ok(json!(self.cancelled.load(Ordering::Acquire))),
            // K7: the events reached the guest between exports
            "events" => Ok(json!(self.events.lock().unwrap().clone())),
            // K8: a host kept past `apply` reaches the host outside any export
            "detach" => Ok(self
                .detached
                .as_ref()
                .expect("kept from apply")
                .view("greeter")
                .unwrap_or_else(|error| json!(error.message()))),
            "hang" => loop {
                std::thread::sleep(Duration::from_secs(60));
            },
            "big" => Ok(json!("x".repeat(args.as_u64().unwrap_or_default() as usize))),
            "stderr" => {
                eprintln!("{}", args.as_str().unwrap_or_default());
                Ok(json!(null))
            }
            "exit_later" => {
                eprintln!("greeter {VERSION} dying");
                std::thread::spawn(|| {
                    std::thread::sleep(Duration::from_millis(100));
                    std::process::exit(3);
                });
                Ok(json!(null))
            }
            _ => Ok(json!({ "version": VERSION, "calls": calls, "request": request })),
        }
    }

    fn dispose(self, host: &Host) {
        host.log(Level::Info, &format!("greeter {VERSION} disposed"));
    }

    fn event(&self, host: &Host, name: &str, payload: Value) {
        let seen = host.view("greeter").unwrap_or_else(|error| json!(error.message()));
        self.events.lock().unwrap().push((name.to_string(), json!({ "payload": payload, "seen": seen })));
    }
}

#[cfg(not(feature = "kardia-names"))]
kardia_guest::export!(Greeter);

// with `kardia-names`, the library exports only the `kardia_*` symbols
#[cfg(feature = "kardia-names")]
mod kardia_names;
