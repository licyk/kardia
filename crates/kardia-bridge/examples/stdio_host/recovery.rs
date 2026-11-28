use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use kardia_bridge::{Bridge, BridgeError, BridgeExt};
use kardia_core::{Context, Disposer, Error, Fiber, PluginDef, RecoveryAction, RecoveryPolicy, Service};
use parking_lot::Mutex;
use serde_json::{Value, json};

#[derive(Default)]
struct State {
    fiber: Mutex<Option<Fiber>>,
    loads: AtomicUsize,
    live: AtomicUsize,
    cleaned: AtomicUsize,
    decisions: AtomicUsize,
    overlap: AtomicUsize,
}

pub fn controller(root: Context) -> PluginDef {
    let state = Arc::new(State::default());
    PluginDef::new(move |ctx, _| {
        let (root, state) = (root.clone(), state.clone());
        async move {
            ctx.get::<Bridge>()?.expose("recovery", move |method: String, args: Value| {
                let (root, state) = (root.clone(), state.clone());
                async move { handle(root, state, &method, args).await }
            })?;
            Ok(())
        }
    })
    .name("recovery-controller")
    .inject([Bridge::NAME])
}

async fn handle(root: Context, state: Arc<State>, method: &str, args: Value) -> Result<Value, BridgeError> {
    match method {
        "start" => {
            let policy = match args.as_str() {
                Some("manual") => RecoveryPolicy::Manual,
                Some("retry") => RecoveryPolicy::retry(Duration::from_millis(200)),
                Some("host") => {
                    let observed = state.clone();
                    RecoveryPolicy::host(move |_| {
                        observed.decisions.fetch_add(1, Ordering::SeqCst);
                        async { Ok(RecoveryAction::RetryAfter(Duration::from_millis(200))) }
                    })
                }
                _ => return Err(BridgeError::invalid("expected manual, retry or host")),
            };
            let fiber = root.plugin_def(&flaky(state.clone()), Value::Null).map_err(failed)?;
            fiber.set_recovery(policy).map_err(failed)?;
            fiber.settled().await.map_err(failed)?;
            *state.fiber.lock() = Some(fiber);
        }
        "fail" => {
            let fiber = current(&state)?;
            // Failure is the expected result; waiting also establishes that cleanup has finished.
            fiber.fail(Error::msg("injected bridge provider failure")).await.expect_err("injected failure");
        }
        "recover" => current(&state)?.update_with(Value::Null, true).map_err(failed)?.await.map_err(failed)?,
        "stop" => current(&state)?.dispose().await.map_err(failed)?,
        "status" => {}
        _ => return Err(BridgeError::invalid(format!("unknown recovery method {method}"))),
    }
    Ok(json!({
        "loads": state.loads.load(Ordering::SeqCst),
        "live": state.live.load(Ordering::SeqCst),
        "cleaned": state.cleaned.load(Ordering::SeqCst),
        "decisions": state.decisions.load(Ordering::SeqCst),
        "overlap": state.overlap.load(Ordering::SeqCst),
    }))
}

fn current(state: &State) -> Result<Fiber, BridgeError> {
    state.fiber.lock().clone().ok_or_else(|| BridgeError::unavailable("start the recovery fixture first"))
}

fn failed(error: Error) -> BridgeError {
    BridgeError::failed(error.to_string())
}

fn flaky(state: Arc<State>) -> PluginDef {
    PluginDef::new(move |ctx, _| {
        let state = state.clone();
        async move {
            let generation = state.loads.fetch_add(1, Ordering::SeqCst) + 1;
            if state.live.fetch_add(1, Ordering::SeqCst) != 0 {
                state.overlap.fetch_add(1, Ordering::SeqCst);
            }
            ctx.defer(
                "fixture cleanup",
                Disposer::future(move || async move {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    state.live.fetch_sub(1, Ordering::SeqCst);
                    state.cleaned.fetch_add(1, Ordering::SeqCst);
                }),
            )?;
            ctx.get::<Bridge>()?.expose("flaky", move |_: String, args: Value| async move {
                Ok(json!({ "generation": generation, "value": args }))
            })?;
            Ok(())
        }
    })
    .name("flaky-provider")
    .inject([Bridge::NAME])
}
