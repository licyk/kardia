use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use kardia_core::{Context, Error, FiberState, PluginDef, RecoveryAction, RecoveryPolicy};
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
