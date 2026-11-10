//! The process transport of `serve` (`examples/process_guest.rs`), driven by
//! hand: protocol version 1 for a host that offers nothing else, and
//! `kardia-guest/2` after its handshake.

use std::env::consts::EXE_SUFFIX;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::{Value, json};

/// Builds the example once, in a target directory of its own.
fn example() -> &'static PathBuf {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("kardia-guest");
        let mut build = Command::new(env!("CARGO"));
        build.args(["build", "--quiet", "-p", "kardia-guest", "--example", "process_guest", "--target-dir"]).arg(&dir);
        assert!(build.status().unwrap().success(), "cannot build the example guest");
        dir.join(format!("debug/examples/process_guest{EXE_SUFFIX}"))
    })
}

struct Guest {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<Value>,
}

impl Guest {
    fn start() -> Self {
        let mut child = Command::new(example()).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let (stdin, stdout) = (child.stdin.take().unwrap(), child.stdout.take().unwrap());
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let _ = sender.send(serde_json::from_str(&line.unwrap()).unwrap());
            }
        });
        Self { child, stdin, lines }
    }

    fn send(&mut self, message: Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn receive(&self) -> Value {
        self.lines.recv_timeout(Duration::from_secs(10)).expect("the guest answers")
    }

    fn exits(mut self) -> bool {
        drop(self.stdin);
        self.child.wait().unwrap().success()
    }
}

#[test]
fn serves_protocol_1_to_a_host_that_offers_nothing_else() {
    let mut guest = Guest::start();
    guest.send(json!({ "export": "meta", "input": null }));
    let meta = guest.receive();
    let meta: Value = serde_json::from_str(meta["output"].as_str().unwrap()).unwrap();
    assert_eq!(meta["plugin"], json!({ "name": "echo", "inject": [], "provide": ["echo"] }));
    assert_eq!(guest.receive(), json!({ "done": 0 }));

    guest.send(json!({ "export": "apply", "input": { "config": { "a": 1 }, "imports": {} } }));
    assert_eq!(guest.receive(), json!({ "call": "provide", "key": "echo", "value": { "config": { "a": 1 } } }));
    guest.send(json!({ "result": null }));
    assert_eq!(guest.receive(), json!({ "done": 0 }));
    guest.send(json!({ "export": "call", "input": { "key": "echo", "method": "hi", "args": 2 } }));
    let output = guest.receive();
    let output: Value = serde_json::from_str(output["output"].as_str().unwrap()).unwrap();
    assert_eq!(output, json!({ "key": "echo", "method": "hi", "args": 2 }));
    assert_eq!(guest.receive(), json!({ "done": 0 }));
    guest.send(json!({ "export": "dispose", "input": null }));
    assert_eq!(guest.receive(), json!({ "done": 0 }));
    assert!(guest.exits());
}

#[test]
fn speaks_kardia_guest_2_after_the_handshake() {
    let mut guest = Guest::start();
    guest.send(json!({ "export": "hello", "input": { "protocol": "kardia-guest/2" } }));
    assert_eq!(guest.receive(), json!({ "ready": "kardia-guest/2" }));

    guest.send(json!({ "id": 1, "export": "apply", "input": { "config": null, "imports": {} } }));
    assert_eq!(guest.receive(), json!({ "id": 1, "call": "provide", "key": "echo", "value": { "config": null } }));
    guest.send(json!({ "id": 1, "result": null }));
    assert_eq!(guest.receive(), json!({ "id": 1, "done": 0 }));

    // a ping is answered while a call runs, and the call sees its cancellation
    guest.send(json!({ "id": 2, "export": "call", "input": { "key": "echo", "method": "sleep", "args": 10_000 } }));
    guest.send(json!({ "ping": 7 }));
    assert_eq!(guest.receive(), json!({ "pong": 7 }));
    guest.send(json!({ "cancel": 2 }));
    assert_eq!(guest.receive(), json!({ "id": 2, "output": r#""cancelled""# }));
    assert_eq!(guest.receive(), json!({ "id": 2, "done": 0 }));

    guest.send(json!({ "id": 3, "export": "dispose", "input": null }));
    assert_eq!(guest.receive(), json!({ "id": 3, "done": 0 }));
    assert!(guest.exits());
}

#[test]
fn answers_a_handshake_it_does_not_know_as_protocol_1() {
    let mut guest = Guest::start();
    guest.send(json!({ "export": "hello", "input": { "protocol": "kardia-guest/9" } }));
    assert_eq!(guest.receive(), json!({ "done": 1 }));
    guest.send(json!({ "export": "meta", "input": null }));
    assert!(guest.receive().get("output").is_some());
    assert_eq!(guest.receive(), json!({ "done": 0 }));
    assert!(guest.exits());
}
