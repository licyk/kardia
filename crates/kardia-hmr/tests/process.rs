//! Process guests (`fixtures/rust-guest`): the spawner and the command, the
//! metadata given from outside, call deadlines and cancellation, exit
//! reports, the frame limit and protocol version 1.

mod common;

use std::env::consts::EXE_SUFFIX;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{App, Kind, fiber, install, scratch};
use kardia_core::{Context, Fiber, FiberState, Svc};
use kardia_hmr::{
    ExitCause, GuestCommand, GuestExited, GuestMeta, GuestProcess, GuestService, GuestStarted, ProcessGuest,
    ProcessSource, Spawner, StdSpawner,
};
use parking_lot::Mutex;
use serde_json::{Value, json};

fn executable(dir: &Path) -> PathBuf {
    install(dir, &format!("greeter{EXE_SUFFIX}"), "v1", Kind::Process)
}

fn greeter_meta() -> GuestMeta {
    GuestMeta::new("greeter").provide("greeter")
}

/// Records what it spawns, and adds a variable to the environment.
#[derive(Default)]
struct Recorder(Mutex<Vec<GuestCommand>>);

impl Spawner for Recorder {
    fn spawn(&self, command: &GuestCommand) -> io::Result<GuestProcess> {
        self.0.lock().push(command.clone());
        StdSpawner.spawn(&command.clone().env("SPAWNED_BY", "recorder"))
    }
}

async fn start(root: &Context, guest: &ProcessGuest) -> (Fiber, Svc<GuestService>) {
    let fiber = root.plugin_def(&guest.plugin(greeter_meta()), json!({})).unwrap();
    fiber.settled().await.unwrap();
    (fiber, root.get_as::<GuestService>("greeter").unwrap())
}

/// Waits for the exit watcher to fail the fiber.
async fn failed(fiber: &Fiber) -> String {
    for _ in 0..500 {
        if let Some(error) = fiber.error() {
            fiber.settled().await.unwrap_err();
            return error.to_string();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{fiber:?} did not fail")
}

fn process_exists(pid: Value) -> bool {
    !cfg!(target_os = "linux") || Path::new(&format!("/proc/{pid}")).exists()
}

#[tokio::test]
async fn spawns_the_command_through_the_spawner_only_when_a_fiber_starts() {
    let dir = scratch("process-spawner");
    let recorder = Arc::new(Recorder::default());
    let command =
        GuestCommand::new(executable(&dir)).args(["--flag", "two words"]).env("GREETING", "hi").current_dir(&dir);
    let guest = ProcessGuest::new(command).spawner(recorder.clone());
    let root = Context::new();
    let def = guest.plugin(greeter_meta());
    assert!(recorder.0.lock().is_empty());

    root.plugin_def(&def, json!({})).unwrap().settled().await.unwrap();
    let greeter = root.get_as::<GuestService>("greeter").unwrap();
    assert_eq!(greeter.call("args", json!(null)).unwrap(), json!(["--flag", "two words"]));
    assert_eq!(greeter.call("env", json!("GREETING")).unwrap(), json!("hi"));
    assert_eq!(greeter.call("env", json!("SPAWNED_BY")).unwrap(), json!("recorder"));
    let cwd = greeter.call("cwd", json!(null)).unwrap();
    assert_eq!(Path::new(cwd.as_str().unwrap()).canonicalize().unwrap(), dir.canonicalize().unwrap());
    assert_eq!(recorder.0.lock().len(), 1);
    assert_eq!(greeter.handle().unwrap().protocol(), 2);
}

#[tokio::test]
async fn fails_a_guest_whose_metadata_differs_from_its_definition() {
    let dir = scratch("process-meta");
    let guest = ProcessGuest::new(GuestCommand::new(executable(&dir)).env("GREETER_NAME", "impostor"));
    let root = Context::new();
    let fiber = root.plugin_def(&guest.plugin(greeter_meta()), json!({})).unwrap();
    let error = fiber.settled().await.unwrap_err().to_string();
    assert!(error.contains("impostor"), "{error}");
    assert_eq!(fiber.state(), FiberState::Failed);
    assert!(root.get_as::<GuestService>("greeter").is_err());
}

#[tokio::test]
async fn a_call_past_its_deadline_kills_the_guest_and_fails_its_fiber() {
    let dir = scratch("process-deadline");
    let root = Context::new();
    let exits = Arc::new(Mutex::new(Vec::new()));
    let seen = exits.clone();
    root.on(move |event: &GuestExited| seen.lock().push((event.fiber.clone(), event.exit.cause.clone()))).unwrap();
    let (fiber, greeter) = start(&root, &ProcessGuest::new(GuestCommand::new(executable(&dir)))).await;
    let (pid, handle) = (greeter.call("pid", json!(null)).unwrap(), greeter.handle().unwrap());

    let error = greeter.call_timeout("hang", json!(null), Duration::from_millis(200)).unwrap_err();
    assert_eq!(error.to_string(), "the guest did not answer in time");
    assert_eq!(handle.exit().unwrap().cause, ExitCause::Killed("the guest did not answer in time".into()));
    assert!(!process_exists(pid));
    assert_eq!(greeter.call("hello", json!(null)).unwrap_err().to_string(), "the guest process exited");
    assert!(!greeter.is_available());

    failed(&fiber).await;
    assert_eq!(fiber.state(), FiberState::Failed);
    assert_eq!(*exits.lock(), vec![(fiber.clone(), handle.exit().unwrap().cause.clone())]);
    assert!(root.get_as::<GuestService>("greeter").is_err());
    // an update starts the guest again
    fiber.update(json!({})).unwrap().await.unwrap();
    let greeter = root.get_as::<GuestService>("greeter").unwrap();
    assert_eq!(greeter.call("hello", json!(null)).unwrap()["calls"], 1);
}

#[tokio::test]
async fn the_default_timeout_bounds_calls_without_a_deadline() {
    let dir = scratch("process-timeout");
    let guest = ProcessGuest::new(GuestCommand::new(executable(&dir))).timeout(Duration::from_millis(300));
    let (_, greeter) = start(&Context::new(), &guest).await;
    assert_eq!(greeter.call("sleep", json!(10)).unwrap(), json!("slept"));
    let error = greeter.call_async("hang", json!(null), None).await.unwrap_err();
    assert_eq!(error.to_string(), "the guest did not answer in time");
}

#[tokio::test]
async fn dropping_an_async_call_cancels_it_in_the_guest() {
    let dir = scratch("process-cancel");
    let (_, greeter) = start(&Context::new(), &ProcessGuest::new(GuestCommand::new(executable(&dir)))).await;
    let call = greeter.call_async("sleep", json!(10_000), None);
    assert!(tokio::time::timeout(Duration::from_millis(200), call).await.is_err());
    // the next call waits for the cancelled one, which the guest ends early
    let result = greeter.call_async("cancelled", json!(null), Some(Duration::from_secs(5))).await.unwrap();
    assert_eq!(result, json!(1));
    assert!(greeter.is_available());
}

#[tokio::test(flavor = "multi_thread")]
async fn answers_pings_while_a_call_runs() {
    let dir = scratch("process-ping");
    let (_, greeter) = start(&Context::new(), &ProcessGuest::new(GuestCommand::new(executable(&dir)))).await;
    let handle = greeter.handle().unwrap();
    let call = tokio::spawn(greeter.call_async("sleep", json!(1000), None));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let round_trip = handle.ping(Duration::from_secs(5)).await.unwrap();
    assert!(round_trip < Duration::from_millis(800), "{round_trip:?}");
    assert!(!call.is_finished());
    assert_eq!(call.await.unwrap().unwrap(), json!("slept"));
}

#[tokio::test]
async fn reports_a_guest_that_exits_with_its_stderr_tail() {
    let dir = scratch("process-exit");
    let root = Context::new();
    let started = Arc::new(Mutex::new(Vec::new()));
    let seen = started.clone();
    root.on(move |event: &GuestStarted| seen.lock().push(event.handle.clone())).unwrap();
    let (fiber, greeter) = start(&root, &ProcessGuest::new(GuestCommand::new(executable(&dir)))).await;
    let handle = started.lock().pop().unwrap();
    assert_eq!(handle.id(), greeter.handle().unwrap().id());

    greeter.call("exit_later", json!(null)).unwrap();
    let exit = tokio::time::timeout(Duration::from_secs(5), handle.exited()).await.unwrap();
    assert_eq!(exit.cause, ExitCause::Exited);
    assert_eq!(exit.status.unwrap().code(), Some(3));
    let tail = String::from_utf8_lossy(&exit.stderr_tail);
    assert!(tail.contains("greeter v1 started") && tail.contains("greeter v1 dying"), "{tail}");

    assert_eq!(failed(&fiber).await, exit.to_string());
    assert_eq!(fiber.state(), FiberState::Failed);
    assert!(!greeter.is_available());
    assert_eq!(greeter.call("hello", json!(null)).unwrap_err().to_string(), "the guest process exited");
}

#[tokio::test]
async fn reports_a_killed_guest_with_its_stderr_tail() {
    let dir = scratch("process-killed");
    let (fiber, greeter) = start(&Context::new(), &ProcessGuest::new(GuestCommand::new(executable(&dir)))).await;
    greeter.call("stderr", json!("last words")).unwrap();
    let handle = greeter.handle().unwrap();
    handle.kill();
    let exit = tokio::time::timeout(Duration::from_secs(5), handle.exited()).await.unwrap();
    assert_eq!(exit.cause, ExitCause::Killed("killed by the host".into()));
    assert!(!exit.status.unwrap().success());
    assert!(String::from_utf8_lossy(&exit.stderr_tail).contains("last words"));
    failed(&fiber).await;
    assert_eq!(fiber.state(), FiberState::Failed);
}

#[tokio::test]
async fn a_message_over_the_frame_limit_kills_the_guest() {
    let dir = scratch("process-frame");
    let guest = ProcessGuest::new(GuestCommand::new(executable(&dir))).frame_limit(4096);
    let (_, greeter) = start(&Context::new(), &guest).await;
    assert_eq!(greeter.call("big", json!(100)).unwrap().as_str().unwrap().len(), 100);
    let error = greeter.call("big", json!(10_000)).unwrap_err().to_string();
    assert_eq!(error, "a message from the guest exceeds the limit of 4096 bytes");
    let exit = greeter.handle().unwrap().exit().unwrap();
    assert_eq!(exit.cause, ExitCause::Killed(error));
    assert_eq!(greeter.call("hello", json!(null)).unwrap_err().to_string(), "the guest process exited");
}

#[tokio::test]
async fn disposing_reports_a_disposed_guest_without_failing_its_fiber() {
    let dir = scratch("process-disposed");
    let root = Context::new();
    let exits = Arc::new(Mutex::new(0));
    let seen = exits.clone();
    root.on(move |_: &GuestExited| *seen.lock() += 1).unwrap();
    let (fiber, greeter) = start(&root, &ProcessGuest::new(GuestCommand::new(executable(&dir)))).await;
    let handle = greeter.handle().unwrap();
    fiber.dispose().await.unwrap();
    assert_eq!(handle.exit().unwrap().cause, ExitCause::Disposed);
    assert_eq!(*exits.lock(), 0);
    assert_eq!(greeter.call("hello", json!(null)).unwrap_err().to_string(), "the guest instance is disposed");
}

#[tokio::test]
async fn serves_a_guest_speaking_protocol_version_1() {
    let dir = scratch("process-v1");
    let app = App::setup(ProcessSource).await;
    let entry = app.load(&install(&dir, &format!("legacy{EXE_SUFFIX}"), "v1", Kind::V1), json!({})).await;
    assert_eq!(fiber(&entry).state(), FiberState::Active);
    assert_eq!(app.view("legacy"), json!({ "protocol": 1 }));
    let legacy = app.root.get_as::<GuestService>("legacy").unwrap();
    let request = json!({ "key": "legacy", "method": "echo", "args": [1] });
    assert_eq!(legacy.call_async("echo", json!([1]), Some(Duration::from_secs(5))).await.unwrap(), request);
    let handle = legacy.handle().unwrap();
    assert_eq!(handle.protocol(), 1);
    assert!(handle.ping(Duration::from_secs(1)).await.is_err());
    app.loader.remove(&entry.id()).unwrap();
    app.loader.settled().await;
    assert_eq!(handle.exit().unwrap().cause, ExitCause::Disposed);
}
