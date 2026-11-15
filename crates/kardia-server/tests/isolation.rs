use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use kardia_core::{Context, Error, FiberState, PluginDef, RecoveryAction, RecoveryPolicy};
use kardia_server::{Server, ServerConfig, ServerExt, ServerPlugin};
use serde_json::{Value, json};

async fn healthy(root: &Context) -> String {
    root.plugin::<ServerPlugin>(ServerConfig::local(0)).unwrap().settled().await.unwrap();
    root.get::<Server>()
        .unwrap()
        .get("/healthy", |_, res, _| async move {
            res.set_body("healthy");
            Ok(None)
        })
        .unwrap();
    root.get::<Server>().unwrap().public_url()
}

#[tokio::test]
async fn callback_panics_before_and_after_await_leave_other_routes_serving() {
    let root = Context::new();
    let url = healthy(&root).await;
    let server = root.get::<Server>().unwrap();
    server
        .get("/sync", |_, _, _| {
            panic!("sync callback panic");
            #[allow(unreachable_code)]
            async {
                Ok(None)
            }
        })
        .unwrap();
    server
        .get("/async", |_, _, _| async {
            tokio::task::yield_now().await;
            panic!("async callback panic")
        })
        .unwrap();
    let client = reqwest::Client::new();
    for _ in 0..3 {
        for path in ["sync", "async"] {
            assert_eq!(client.get(format!("{url}/{path}")).send().await.unwrap().status(), 500);
            assert_eq!(client.get(format!("{url}/healthy")).send().await.unwrap().text().await.unwrap(), "healthy");
        }
    }
    root.fiber().dispose().await.unwrap();
}

async fn recovery(mode: u8) {
    let root = Context::new();
    let url = healthy(&root).await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = attempts.clone();
    let def = PluginDef::new(move |ctx, _| {
        counter.fetch_add(1, Ordering::SeqCst);
        async move {
            ctx.get::<Server>()?.get("/recovering", |_, res, _| async move {
                res.set_body("recovered");
                Ok(None)
            })?;
            Ok(())
        }
    })
    .inject(["server"]);
    let bad = root.plugin_def(&def, Value::Null).unwrap();
    bad.settled().await.unwrap();
    let policy = match mode {
        0 => RecoveryPolicy::Manual,
        1 => RecoveryPolicy::Retry { delay: Duration::from_millis(20) },
        _ => RecoveryPolicy::host(|_| async { Ok(RecoveryAction::RetryAfter(Duration::from_millis(20))) }),
    };
    bad.set_recovery(policy).unwrap();
    let client = reqwest::Client::builder().timeout(Duration::from_secs(2)).build().unwrap();
    let progress = Arc::new(AtomicUsize::new(0));
    let observed = progress.clone();
    let healthy_url = url.clone();
    let probe = client.clone();
    let monitor = tokio::spawn(async move {
        loop {
            assert_eq!(
                probe.get(format!("{healthy_url}/healthy")).send().await.unwrap().text().await.unwrap(),
                "healthy"
            );
            observed.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
        }
    });
    for round in 0..3 {
        let baseline = progress.load(Ordering::SeqCst);
        bad.fail(Error::msg("injected plugin failure")).await.unwrap_err();
        assert_eq!(root.get::<Server>().unwrap().route_count(), 1);
        if mode == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert_eq!(bad.state(), FiberState::Failed);
            assert_eq!(attempts.load(Ordering::SeqCst), round + 1);
            assert_eq!(client.get(format!("{url}/recovering")).send().await.unwrap().status(), 404);
            bad.update(json!(null)).unwrap().await.unwrap();
        } else {
            tokio::time::timeout(Duration::from_secs(2), async {
                while bad.state() != FiberState::Active {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
        }
        assert!(progress.load(Ordering::SeqCst) > baseline);
        assert_eq!(root.get::<Server>().unwrap().route_count(), 2);
        assert_eq!(client.get(format!("{url}/recovering")).send().await.unwrap().text().await.unwrap(), "recovered");
    }
    monitor.abort();
    let _ = monitor.await;
    bad.dispose().await.unwrap();
    assert_eq!(root.get::<Server>().unwrap().route_count(), 1);
    root.fiber().dispose().await.unwrap();
}
#[tokio::test]
async fn manual_recovery_isolates_three_failures_over_real_http() {
    recovery(0).await;
}
#[tokio::test]
async fn automatic_recovery_isolates_three_failures_over_real_http() {
    recovery(1).await;
}
#[tokio::test]
async fn host_recovery_isolates_three_failures_over_real_http() {
    recovery(2).await;
}
