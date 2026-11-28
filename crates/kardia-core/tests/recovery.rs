use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use kardia_core::{Context, Error, FiberState, Phase, PluginDef, RecoveryAction, RecoveryPolicy, RecoveryState};
use serde_json::{Value, json};

async fn until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}

fn failing(attempts: Arc<AtomicUsize>, failures: usize) -> PluginDef {
    let error = Error::msg("same failure value");
    PluginDef::new(move |ctx, _| {
        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
        let error = error.clone();
        async move {
            ctx.provide_as("recovering", json!(attempt))?;
            if attempt < failures { Err(error) } else { Ok(()) }
        }
    })
}

/// Like [`failing`], under a key of its own so one test can run several.
fn failing_as(key: &'static str, attempts: Arc<AtomicUsize>, failures: usize) -> PluginDef {
    let error = Error::msg("same failure value");
    PluginDef::new(move |ctx, _| {
        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
        let error = error.clone();
        async move {
            ctx.provide_as(key, json!(attempt))?;
            if attempt < failures { Err(error) } else { Ok(()) }
        }
    })
}

#[tokio::test]
async fn retry_recovers_repeated_failures_without_stopping_other_plugins() {
    let root = Context::new();
    let healthy = root
        .plugin_def(
            &PluginDef::new(|ctx, _| async move {
                ctx.provide_as("healthy", json!("alive"))?;
                Ok(())
            }),
            Value::Null,
        )
        .unwrap();
    healthy.settled().await.unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let bad = root.plugin_def(&failing(calls.clone(), 3), Value::Null).unwrap();
    bad.set_recovery(RecoveryPolicy::retry(Duration::from_millis(5))).unwrap();
    until(|| {
        assert_eq!(healthy.state(), FiberState::Active);
        assert_eq!(root.view("healthy").unwrap(), json!("alive"));
        bad.state() == FiberState::Active
    })
    .await;
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert_eq!(root.view("recovering").unwrap(), json!(3));
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn a_retry_limit_stops_the_restarts_and_leaves_the_fiber_failed() {
    let root = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));
    // This plugin never stops failing, so without a limit it would restart
    // forever.
    let bad = root
        .plugin_def(
            &PluginDef::new({
                let calls = calls.clone();
                move |_, _| {
                    let calls = calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Err(Error::msg("always fails"))
                    }
                }
            }),
            Value::Null,
        )
        .unwrap();
    bad.set_recovery(RecoveryPolicy::retry_with(Duration::from_millis(1), Some(2), kardia_core::Backoff::Fixed))
        .unwrap();
    until(|| calls.load(Ordering::SeqCst) == 3).await;
    // Two retries happened; the fiber now stays failed instead of restarting.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 3, "no restart past the limit");
    assert_eq!(bad.state(), FiberState::Failed);
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn manual_policy_waits_for_explicit_handling() {
    let root = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let bad = root.plugin_def(&failing(calls.clone(), 1), Value::Null).unwrap();
    assert!(bad.settled().await.is_err());
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(bad.state(), FiberState::Failed);
    bad.update(Value::Null).unwrap().await.unwrap();
    assert_eq!(bad.state(), FiberState::Active);
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn host_decides_after_cleanup_and_can_keep_the_plugin_failed() {
    let root = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let notified = Arc::new(AtomicUsize::new(0));
    let bad = root.plugin_def(&failing(calls.clone(), 1), Value::Null).unwrap();
    let seen = notified.clone();
    bad.set_recovery(RecoveryPolicy::host(move |failure| {
        let seen = seen.clone();
        async move {
            assert_eq!(failure.fiber.state(), FiberState::Failed);
            assert_eq!(failure.fiber.effect_count(), 0);
            seen.fetch_add(1, Ordering::SeqCst);
            Ok(RecoveryAction::KeepFailed)
        }
    }))
    .unwrap();
    until(|| notified.load(Ordering::SeqCst) == 1).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(bad.state(), FiberState::Failed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    bad.set_recovery(RecoveryPolicy::host(|_| async { Ok(RecoveryAction::RetryAfter(Duration::from_millis(1))) }))
        .unwrap();
    until(|| bad.state() == FiberState::Active).await;
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn host_handler_panics_are_contained_before_and_after_await() {
    let root = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let bad = root.plugin_def(&failing(calls.clone(), 1), Value::Null).unwrap();
    let handled = Arc::new(AtomicUsize::new(0));
    let seen = handled.clone();
    bad.set_recovery(RecoveryPolicy::host(move |_| {
        seen.fetch_add(1, Ordering::SeqCst);
        panic!("synchronous host panic");
        #[allow(unreachable_code)]
        async {
            Ok(RecoveryAction::KeepFailed)
        }
    }))
    .unwrap();
    until(|| handled.load(Ordering::SeqCst) == 1).await;
    let seen = handled.clone();
    bad.set_recovery(RecoveryPolicy::host(move |_| {
        let seen = seen.clone();
        async move {
            tokio::task::yield_now().await;
            seen.fetch_add(1, Ordering::SeqCst);
            panic!("async host panic")
        }
    }))
    .unwrap();
    until(|| handled.load(Ordering::SeqCst) == 2).await;
    assert_eq!(bad.state(), FiberState::Failed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    root.provide_as("healthy", json!(true)).unwrap();
    assert_eq!(root.view("healthy").unwrap(), json!(true));
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn switching_to_manual_and_disposal_cancel_pending_retries() {
    let root = Context::new();
    for dispose in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let bad = root.plugin_def(&failing(calls.clone(), usize::MAX), Value::Null).unwrap();
        bad.set_recovery(RecoveryPolicy::retry(Duration::from_millis(25))).unwrap();
        assert!(bad.settled().await.is_err());
        tokio::task::yield_now().await;
        if dispose {
            bad.dispose().await.unwrap();
        } else {
            bad.set_recovery(RecoveryPolicy::Manual).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        if !dispose {
            bad.dispose().await.unwrap();
        }
    }
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn invalid_configuration_is_not_retried() {
    #[derive(serde::Deserialize)]
    struct Config {
        _required: String,
    }
    let root = Context::new();
    let attempts = Arc::new(AtomicUsize::new(0));
    let seen = attempts.clone();
    let def = PluginDef::typed(move |_, _: Arc<Config>| {
        seen.fetch_add(1, Ordering::SeqCst);
        async { Ok(()) }
    });
    let bad = root.plugin_def(&def, Value::Null).unwrap();
    bad.set_recovery(RecoveryPolicy::retry(Duration::from_millis(1))).unwrap();
    assert!(bad.settled().await.is_err());
    let error = bad.error().unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(bad.error().unwrap().ptr_eq(&error));
    assert_eq!(attempts.load(Ordering::SeqCst), 0);
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn a_policy_sees_whether_the_constructor_or_the_running_instance_failed() {
    let root = Context::new();
    let phases = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = phases.clone();
    // `failing` fails inside `apply`, so every fault is a failed start
    let bad = root.plugin_def(&failing(Arc::new(AtomicUsize::new(0)), 1), Value::Null).unwrap();
    bad.set_recovery(RecoveryPolicy::host(move |failure| {
        seen.lock().unwrap().push(failure.phase);
        async { Ok(RecoveryAction::KeepFailed) }
    }))
    .unwrap();
    assert!(bad.settled().await.is_err());
    until(|| !phases.lock().unwrap().is_empty()).await;
    assert_eq!(*phases.lock().unwrap(), [Phase::Start], "a failed constructor is a start failure");

    // a running instance reporting itself unusable is not
    let healthy = root
        .plugin_def(
            &PluginDef::new(|ctx, _| async move {
                ctx.provide_as("healthy", json!(1))?;
                Ok(())
            }),
            Value::Null,
        )
        .unwrap();
    healthy.settled().await.unwrap();
    let running = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = running.clone();
    healthy
        .set_recovery(RecoveryPolicy::host(move |failure| {
            seen.lock().unwrap().push(failure.phase);
            async { Ok(RecoveryAction::KeepFailed) }
        }))
        .unwrap();
    assert!(healthy.fail(Error::msg("running fault")).await.is_err());
    until(|| !running.lock().unwrap().is_empty()).await;
    assert_eq!(*running.lock().unwrap(), [Phase::Running]);
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn a_delay_table_waits_each_entry_and_stops_at_its_end() {
    let root = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let bad = root.plugin_def(&failing(calls.clone(), 10), Value::Null).unwrap();
    bad.set_recovery(RecoveryPolicy::retry_delays([Duration::from_millis(2), Duration::from_millis(4)], None)).unwrap();
    // two entries allow two restarts, so the fourth call never happens
    until(|| calls.load(Ordering::SeqCst) >= 3).await;
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(bad.state(), FiberState::Failed);
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn a_window_starts_the_count_over_after_it_passes() {
    let root = Context::new();
    // A constructor that succeeds; the instance is failed from outside, so
    // every restart reaches a fresh constructor call.
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let def = PluginDef::new(move |ctx, _| {
        let counted = counted.clone();
        async move {
            counted.fetch_add(1, Ordering::SeqCst);
            ctx.provide_as("recovering", json!(1))?;
            Ok(())
        }
    });
    let fiber = root.plugin_def(&def, Value::Null).unwrap();
    fiber.settled().await.unwrap();
    // One delay in the table, counted within a short window: a fault after the
    // window passes is the first restart again, not a spent limit.
    fiber
        .set_recovery(RecoveryPolicy::retry_delays([Duration::from_millis(2)], Some(Duration::from_millis(10))))
        .unwrap();
    assert!(fiber.fail(Error::msg("running fault")).await.is_err());
    until(|| calls.load(Ordering::SeqCst) == 2).await;
    // the restart's fresh constructor succeeded, so the fiber recovered
    fiber.settled().await.unwrap();
    // let the window pass, then fault again: the count starts over, so this is
    // retried rather than refused as a spent limit
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(fiber.fail(Error::msg("running fault again")).await.is_err());
    until(|| calls.load(Ordering::SeqCst) == 3).await;
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn a_scheduled_retry_is_visible_and_runs_at_once_when_asked() {
    let root = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let bad = root.plugin_def(&failing(calls.clone(), 10), Value::Null).unwrap();
    bad.set_recovery(RecoveryPolicy::retry_delays([Duration::from_secs(30)], None)).unwrap();
    assert!(bad.settled().await.is_err());
    until(|| bad.recovery_state().due_in.is_some()).await;
    let state = bad.recovery_state();
    assert_eq!(state.next_attempt, Some(1));
    assert!(state.due_in.unwrap() <= Duration::from_secs(30));

    // a user's retry does not wait out the 30 seconds
    bad.retry_now().unwrap();
    until(|| calls.load(Ordering::SeqCst) >= 2).await;
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn disposing_a_fiber_cancels_a_pending_retry() {
    let root = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let bad = root.plugin_def(&failing(calls.clone(), 10), Value::Null).unwrap();
    bad.set_recovery(RecoveryPolicy::retry(Duration::from_millis(20))).unwrap();
    assert!(bad.settled().await.is_err());
    until(|| bad.recovery_state().due_in.is_some()).await;
    bad.dispose().await.unwrap();
    let after = calls.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert_eq!(calls.load(Ordering::SeqCst), after, "a disposed fiber is not restarted again");
    assert_eq!(bad.recovery_state(), RecoveryState::default());
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn switching_to_manual_cancels_a_pending_retry() {
    let root = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let bad = root.plugin_def(&failing(calls.clone(), 10), Value::Null).unwrap();
    bad.set_recovery(RecoveryPolicy::retry(Duration::from_millis(20))).unwrap();
    assert!(bad.settled().await.is_err());
    until(|| bad.recovery_state().due_in.is_some()).await;
    bad.set_recovery(RecoveryPolicy::Manual).unwrap();
    let after = calls.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert_eq!(calls.load(Ordering::SeqCst), after);
    assert_eq!(bad.recovery_state(), RecoveryState::default());
    root.fiber().dispose().await.unwrap();
}

/// The acceptance the downstream host asked for: the three policies, disposal
/// cancelling a pending restart, repeated failure and recovery, and unrelated
/// plugins working throughout.
#[tokio::test]
async fn a_hosts_three_policies_hold_while_unrelated_plugins_keep_working() {
    let root = Context::new();
    let healthy = root
        .plugin_def(
            &PluginDef::new(|ctx, _| async move {
                ctx.provide_as("healthy", json!("alive"))?;
                Ok(())
            }),
            Value::Null,
        )
        .unwrap();
    healthy.settled().await.unwrap();

    // automatic: recovers without anything driving a restart
    let calls = Arc::new(AtomicUsize::new(0));
    let automatic = root.plugin_def(&failing_as("automatic", calls.clone(), 2), Value::Null).unwrap();
    automatic
        .set_recovery(RecoveryPolicy::retry_delays([Duration::from_millis(2), Duration::from_millis(4)], None))
        .unwrap();
    until(|| automatic.state() == FiberState::Active).await;
    assert_eq!(root.view("healthy").unwrap(), json!("alive"), "an unrelated plugin kept working");

    // manual: stays failed until it is handled
    let manual_calls = Arc::new(AtomicUsize::new(0));
    // fails its first constructor, succeeds on the one a manual retry runs
    let manual = root.plugin_def(&failing_as("manual", manual_calls.clone(), 1), Value::Null).unwrap();
    assert!(manual.settled().await.is_err());
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(manual_calls.load(Ordering::SeqCst), 1, "manual mode never retries on its own");
    assert_eq!(manual.state(), FiberState::Failed);
    manual.update(Value::Null).unwrap().await.unwrap();
    assert_eq!(manual.state(), FiberState::Active);

    // host-defined: follows the host, and a handler error is contained
    let decided = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = decided.clone();
    let hosted = root.plugin_def(&failing_as("hosted", Arc::new(AtomicUsize::new(0)), 1), Value::Null).unwrap();
    hosted
        .set_recovery(RecoveryPolicy::host(move |_| {
            seen.lock().unwrap().push(());
            async { Err(Error::msg("the host handler failed")) }
        }))
        .unwrap();
    assert!(hosted.settled().await.is_err());
    until(|| !decided.lock().unwrap().is_empty()).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(hosted.state(), FiberState::Failed, "a failed handler leaves the fiber failed");
    assert_eq!(
        root.view("healthy").unwrap(),
        json!("alive"),
        "a contained handler error never stops unrelated plugins"
    );

    // repeated failure and recovery, then disposal cancels a pending restart
    let looping = Arc::new(AtomicUsize::new(0));
    let cycling = root.plugin_def(&failing_as("cycling", looping.clone(), 4), Value::Null).unwrap();
    cycling.set_recovery(RecoveryPolicy::retry_delays([Duration::from_millis(1); 4], None)).unwrap();
    until(|| cycling.state() == FiberState::Active).await;
    assert_eq!(looping.load(Ordering::SeqCst), 5, "four failures, four restarts, one success");
    // the table is spent, so start a fresh one before the next fault
    cycling.set_recovery(RecoveryPolicy::retry_delays([Duration::from_secs(30)], None)).unwrap();
    assert!(cycling.fail(Error::msg("fault again")).await.is_err());
    until(|| cycling.recovery_state().due_in.is_some()).await;
    cycling.dispose().await.unwrap();
    let after = looping.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(looping.load(Ordering::SeqCst), after, "disposal cancelled the pending restart");
    assert_eq!(root.view("healthy").unwrap(), json!("alive"));
    root.fiber().dispose().await.unwrap();
}
