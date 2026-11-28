use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use kardia_core::{Context, Error, FiberState, PluginDef, RecoveryAction};
use kardia_loader::{Catalog, Loader, validate_options};
use serde_json::{Value, json};

async fn setup() -> (Context, Catalog, Loader) {
    let root = Context::new();
    let catalog = Catalog::new();
    root.plugin_def(&Loader::plugin(catalog.clone()), Value::Null).unwrap().settled().await.unwrap();
    let loader = (*root.get::<Loader>().unwrap()).clone();
    (root, catalog, loader)
}
async fn until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}
fn failing(calls: Arc<AtomicUsize>, count: usize) -> PluginDef {
    PluginDef::new(move |ctx, config| {
        let n = calls.fetch_add(1, Ordering::SeqCst);
        async move {
            if n < count {
                return Err(Error::msg("failure"));
            }
            ctx.provide_as("result", config)?;
            Ok(())
        }
    })
}

#[tokio::test]
async fn configured_retry_recovers_without_configuration_writeback() {
    let (root, catalog, loader) = setup().await;
    let calls = Arc::new(AtomicUsize::new(0));
    catalog.register_def("bad", failing(calls.clone(), 3));
    loader
        .create(json!({"id":"bad","name":"bad","config":42,"recovery":{"mode":"retry","delayMs":5}}), None, None)
        .await
        .unwrap();
    loader.settled().await;
    let entry = loader.resolve("bad").unwrap();
    let before = entry.options();
    until(|| entry.inner_fiber().unwrap().state() == FiberState::Active).await;
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert_eq!(root.view("result").unwrap(), 42);
    assert_eq!(entry.options(), before);
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn default_manual_and_named_host_decisions_are_obeyed() {
    let (root, catalog, loader) = setup().await;
    let calls = Arc::new(AtomicUsize::new(0));
    catalog.register_def("bad", failing(calls.clone(), 1));
    let decisions = Arc::new(AtomicUsize::new(0));
    let observed = decisions.clone();
    loader.register_recovery_handler("notify", move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
        async { Ok(RecoveryAction::KeepFailed) }
    });
    loader.create(json!({"id":"bad","name":"bad"}), None, None).await.unwrap();
    loader.settled().await;
    let fiber = loader.resolve("bad").unwrap().inner_fiber().unwrap();
    assert_eq!(fiber.state(), FiberState::Failed);
    loader.update("bad", json!({"recovery":{"mode":"host","handler":"notify"}}), None, None).await.unwrap();
    until(|| decisions.load(Ordering::SeqCst) == 1).await;
    assert_eq!(fiber.state(), FiberState::Failed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    loader.register_recovery_handler("retry", |_| async { Ok(RecoveryAction::RetryAfter(Duration::from_millis(5))) });
    loader.update("bad", json!({"recovery":{"mode":"host","handler":"retry"}}), None, None).await.unwrap();
    until(|| fiber.state() == FiberState::Active).await;
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn disabling_and_switching_to_manual_cancel_pending_recovery() {
    for disable in [false, true] {
        let (root, catalog, loader) = setup().await;
        let calls = Arc::new(AtomicUsize::new(0));
        catalog.register_def("bad", failing(calls.clone(), usize::MAX));
        loader
            .create(json!({"id":"bad","name":"bad","recovery":{"mode":"retry","delayMs":50}}), None, None)
            .await
            .unwrap();
        loader.settled().await;
        let patch = if disable { json!({"disabled":true}) } else { json!({"recovery":{"mode":"manual"}}) };
        loader.update("bad", patch, None, None).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        root.fiber().dispose().await.unwrap();
    }
}

#[tokio::test]
async fn unknown_host_handler_is_reported_without_running_plugin() {
    let (root, catalog, loader) = setup().await;
    let calls = Arc::new(AtomicUsize::new(0));
    catalog.register_def("bad", failing(calls.clone(), 1));
    loader
        .create(json!({"id":"bad","name":"bad","recovery":{"mode":"host","handler":"absent"}}), None, None)
        .await
        .unwrap();
    loader.settled().await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(loader.resolve("bad").unwrap().inner_fiber().is_none());
    root.fiber().dispose().await.unwrap();
}

#[test]
fn recovery_options_validate_only_required_fields() {
    for recovery in
        [json!({"mode":"manual","extra":true}), json!({"mode":"retry"}), json!({"mode":"host","handler":"notify"})]
    {
        assert!(validate_options(&json!({"name":"test","recovery":recovery}), false).is_empty());
    }
    for recovery in
        [json!(true), json!({"mode":"restart"}), json!({"mode":"retry","delayMs":0}), json!({"mode":"host"})]
    {
        assert!(!validate_options(&json!({"name":"test","recovery":recovery}), false).is_empty());
    }
}

#[tokio::test]
async fn a_delay_table_is_read_from_configuration_and_stops_at_its_end() {
    let (root, catalog, loader) = setup().await;
    let calls = Arc::new(AtomicUsize::new(0));
    catalog.register_def("bad", failing(calls.clone(), 10));
    loader
        .create(
            json!({"id":"bad","name":"bad","recovery":{"mode":"delays","delayMs":[2,4],"windowMs":60000}}),
            None,
            None,
        )
        .await
        .unwrap();
    // two delays allow two restarts, so the constructor is called three times
    until(|| calls.load(Ordering::SeqCst) >= 3).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    root.fiber().dispose().await.unwrap();
}

#[test]
fn a_delay_table_rejects_an_empty_or_non_positive_list() {
    for recovery in [
        json!({"mode":"delays"}),
        json!({"mode":"delays","delayMs":[]}),
        json!({"mode":"delays","delayMs":[2,0]}),
        json!({"mode":"delays","delayMs":[2],"windowMs":0}),
    ] {
        assert!(!validate_options(&json!({"name":"test","recovery":recovery}), false).is_empty());
    }
    assert!(validate_options(&json!({"name":"test","recovery":{"mode":"delays","delayMs":[2,10]}}), false).is_empty());
}
