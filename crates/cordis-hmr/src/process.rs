//! Executables as guest modules: each instance is a child process speaking
//! the guest ABI as JSON lines over its stdin and stdout, so a crashing guest
//! fails only its own calls, and reloading a module respawns its processes.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;

use cordis_core::{Error, Result};
use serde_json::{Value, json};

use crate::graph::{ModuleCode, ModuleSource};
use crate::guest::{self, Code, Export, Host, Instance};

/// Runs executables that serve the guest ABI on stdin/stdout (see the crate
/// docs); module ids are their paths. Calls block their caller until the
/// guest answers, as with the other backends.
///
/// Replace an executable by renaming a new file over it: a running one
/// cannot be written to, and one held open for writing cannot be started.
#[derive(Default)]
pub struct ProcessSource;

impl ModuleSource for ProcessSource {
    fn load(&self, id: &str) -> Result<ModuleCode> {
        guest::module(id, Arc::new(ProcessCode(id.to_string())))
    }
}

struct ProcessCode(String);

impl Code for ProcessCode {
    fn instantiate(&self, host: Host) -> Result<Box<dyn Instance>> {
        let mut child = Command::new(&self.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| Error::msg(format!("cannot start guest {}: {error}", self.0)))?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = BufReader::new(child.stdout.take().expect("piped"));
        Ok(Box::new(ProcessInstance { child, stdin, stdout, host, broken: false }))
    }
}

struct ProcessInstance {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    host: Host,
    /// Out of step with the guest after a failed exchange: what it sent
    /// before it was killed may still be buffered, and must not be read as
    /// the replies of a later call.
    broken: bool,
}

impl Instance for ProcessInstance {
    fn host(&mut self) -> &mut Host {
        &mut self.host
    }

    fn call(&mut self, export: Export) -> Result<i32> {
        if self.broken {
            return Err(exited());
        }
        let result = self.exchange(export);
        if result.is_err() {
            self.broken = true;
            let _ = self.child.kill();
        }
        result
    }
}

impl ProcessInstance {
    fn exchange(&mut self, export: Export) -> Result<i32> {
        let input = match self.host.message.as_slice() {
            [] => Value::Null,
            message => serde_json::from_slice(message)?,
        };
        let name = match export {
            Export::Meta => "meta",
            Export::Apply => "apply",
            Export::Call => "call",
            Export::Dispose => "dispose",
        };
        self.send(&json!({ "export": name, "input": input }))?;
        loop {
            let message = self.receive()?;
            if let Some(status) = message.get("done") {
                return Ok(status.as_i64().unwrap_or(1) as i32);
            }
            if let Some(output) = message.get("output") {
                self.host.write(output.as_str().unwrap_or_default().as_bytes());
                continue;
            }
            let text = |key: &str| message.get(key).and_then(Value::as_str).unwrap_or_default().as_bytes().to_vec();
            let json = |key: &str| serde_json::to_vec(message.get(key).unwrap_or(&Value::Null)).expect("a JSON value");
            let status = match message.get("call").and_then(Value::as_str) {
                Some("log") => {
                    let level = message.get("level").and_then(Value::as_i64).unwrap_or(2) as i32;
                    self.host.log(level, &text("text"));
                    continue;
                }
                Some("provide") => self.host.provide(&text("key"), &json("value")),
                Some("view") => self.host.view(&text("key")),
                Some("invoke") => self.host.invoke(&text("key"), &text("method"), &json("args")),
                _ => return Err(Error::msg(format!("unexpected message from guest: {message}"))),
            };
            let reply = self.reply(status);
            self.send(&reply)?;
        }
    }

    fn send(&mut self, message: &Value) -> Result<()> {
        writeln!(self.stdin, "{message}").and_then(|()| self.stdin.flush()).map_err(|_| exited())
    }

    fn receive(&mut self) -> Result<Value> {
        let mut line = String::new();
        if self.stdout.read_line(&mut line).map_err(|_| exited())? == 0 {
            return Err(exited());
        }
        serde_json::from_str(&line).map_err(|error| Error::msg(format!("invalid message from guest: {error}")))
    }

    /// The result of a host call, which the host left in its message.
    fn reply(&self, status: i32) -> Value {
        let message = &self.host.message;
        if status < 0 {
            return json!({ "error": String::from_utf8_lossy(message) });
        }
        match serde_json::from_slice::<Value>(message) {
            Ok(result) => json!({ "result": result }),
            Err(_) => json!({ "result": null }),
        }
    }
}

impl Drop for ProcessInstance {
    /// The process of a disposed (or failed) instance is never left running.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn exited() -> Error {
    Error::msg("the guest process exited")
}
