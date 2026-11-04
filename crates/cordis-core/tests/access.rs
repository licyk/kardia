//! Port of `packages/core/tests/isolate.spec.ts`, `reflect.spec.ts` and the
//! lifecycle parts of `service.spec.ts`.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::{Custom, Seq, settle, sleep};
use cordis_core::{AccessError, Context, Disposer, Fiber, PluginDef, RealmId, ServiceChanged};
use serde_json::Value;

fn access(ctx: &Context) -> String {
    match ctx.get_as::<&str>("foo") {
        Ok(value) => value.to_string(),
        Err(error) => error.to_string(),
    }
}

async fn run(ctx: &Context, f: impl Fn(Context) + Send + Sync + 'static) -> Fiber {
    let f = Arc::new(f);
    let fiber = ctx
        .plugin_def(
            &PluginDef::new(move |ctx, _| {
                f(ctx);
                async { Ok(()) }
            }),
            Value::Null,
        )
        .unwrap();
    fiber.settled().await.unwrap();
    fiber
}

const UNDECLARED: &str = "cannot get property \"foo\" without inject";

#[tokio::test]
async fn does_not_expose_own_provisions_across_isolation_boundaries() {
    let root = Context::new();
    let results = Seq::new();
    let r = results.clone();
    root.plugin_def(
        &PluginDef::new(move |ctx, _| {
            let results = r.clone();
            async move {
                let isolated = ctx.isolate("foo");
                isolated.provide_as("foo", "own")?;
                results.push(access(&isolated));
                results.push(access(&ctx));
                let r1 = results.clone();
                run(&isolated, move |ctx| r1.push(access(&ctx))).await;
                let r2 = results.clone();
                run(&ctx, move |ctx| r2.push(access(&ctx))).await;
                Ok(())
            }
        }),
        Value::Null,
    )
    .unwrap()
    .settled()
    .await
    .unwrap();
    assert_eq!(results.get(), vec!["own", UNDECLARED, "own", UNDECLARED]);
}

#[tokio::test]
async fn does_not_inherit_the_injected_view_across_isolation_boundaries() {
    let root = Context::new();
    root.provide_as("foo", "outer").unwrap();
    let results = Seq::new();
    let r = results.clone();
    root.inject(["foo"], move |ctx| {
        let results = r.clone();
        async move {
            let r1 = results.clone();
            run(&ctx, move |ctx| r1.push(access(&ctx))).await;
            let r2 = results.clone();
            run(&ctx.isolate("foo"), move |ctx| r2.push(access(&ctx))).await;
            // pinning the same realm still derives an isolation of its own
            let r3 = results.clone();
            run(&ctx.isolate_in("foo", ctx.realm("foo").unwrap()), move |ctx| r3.push(access(&ctx))).await;
            Ok(())
        }
    })
    .unwrap()
    .settled()
    .await
    .unwrap();
    assert_eq!(results.get(), vec!["outer", UNDECLARED, UNDECLARED]);
}

fn counted(calls: &Arc<AtomicUsize>, disposed: &Arc<AtomicUsize>) -> PluginDef {
    let (calls, disposed) = (calls.clone(), disposed.clone());
    PluginDef::new(move |ctx, _| {
        calls.fetch_add(1, Ordering::SeqCst);
        let disposed = disposed.clone();
        async move {
            ctx.on_dispose(move || {
                disposed.fetch_add(1, Ordering::SeqCst);
            })?;
            Ok(())
        }
    })
    .inject(["foo"])
}

fn live(ctx: &Context) -> Option<i32> {
    ctx.live_as::<i32>("foo").map(|v| *v)
}

#[tokio::test]
async fn isolated_context() {
    let root = Context::new();
    let (calls, disposed) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let plugin = counted(&calls, &disposed);
    let count = |n: &Arc<AtomicUsize>| n.load(Ordering::SeqCst);
    root.plugin_def(&plugin, Value::Null).unwrap();
    let ctx1 = root.isolate("foo");
    ctx1.plugin_def(&plugin, Value::Null).unwrap();
    let ctx2 = root.isolate("foo");
    ctx2.plugin_def(&plugin, Value::Null).unwrap();

    let dispose0 = root.provide_as("foo", 100i32).unwrap();
    assert_eq!((live(&root), live(&ctx1), live(&ctx2)), (Some(100), None, None));
    settle().await;
    assert_eq!((count(&calls), count(&disposed)), (1, 0));

    ctx1.provide_as("foo", 200i32).unwrap();
    assert_eq!((live(&root), live(&ctx1), live(&ctx2)), (Some(100), Some(200), None));
    settle().await;
    assert_eq!((count(&calls), count(&disposed)), (2, 0));

    drop(dispose0.dispose());
    assert_eq!((live(&root), live(&ctx1), live(&ctx2)), (None, Some(200), None));
    settle().await;
    assert_eq!((count(&calls), count(&disposed)), (2, 1));

    ctx2.provide_as("foo", 300i32).unwrap();
    assert_eq!((live(&root), live(&ctx1), live(&ctx2)), (None, Some(200), Some(300)));
    settle().await;
    assert_eq!((count(&calls), count(&disposed)), (3, 1));
}

#[tokio::test]
async fn shared_label() {
    let root = Context::new();
    let (calls, disposed) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let plugin = counted(&calls, &disposed);
    let count = |n: &Arc<AtomicUsize>| n.load(Ordering::SeqCst);
    let label = RealmId::new();
    root.plugin_def(&plugin, Value::Null).unwrap();
    let ctx1 = root.isolate_in("foo", label);
    ctx1.plugin_def(&plugin, Value::Null).unwrap();
    let ctx2 = root.isolate_in("foo", label);
    ctx2.plugin_def(&plugin, Value::Null).unwrap();
    settle().await;
    assert_eq!(count(&calls), 0);

    root.provide_as("foo", 100i32).unwrap();
    assert_eq!((live(&root), live(&ctx1), live(&ctx2)), (Some(100), None, None));
    settle().await;
    assert_eq!((count(&calls), count(&disposed)), (1, 0));

    let dispose12 = ctx1.provide_as("foo", 200i32).unwrap();
    assert_eq!((live(&root), live(&ctx1), live(&ctx2)), (Some(100), Some(200), Some(200)));
    settle().await;
    assert_eq!((count(&calls), count(&disposed)), (3, 0));

    drop(dispose12.dispose());
    assert_eq!((live(&root), live(&ctx1), live(&ctx2)), (Some(100), None, None));
    settle().await;
    assert_eq!((count(&calls), count(&disposed)), (3, 2));
}

#[tokio::test]
async fn service_changes_are_delivered_within_the_realm() {
    let root = Context::new();
    let ctx = root.isolate("foo");
    let (outer, inner) = (Seq::new(), Seq::new());
    let (o, i) = (outer.clone(), inner.clone());
    root.on(move |event: &ServiceChanged| o.push(event.key.to_string())).unwrap();
    ctx.on(move |event: &ServiceChanged| i.push(event.key.to_string())).unwrap();
    run(&ctx, |ctx| {
        ctx.provide_as("foo", ()).unwrap();
    })
    .await;
    assert_eq!(outer.len(), 0);
    assert_eq!(inner.get(), vec!["foo"]);
}

#[tokio::test]
async fn keeps_own_provisions_separate_from_the_injected_view() {
    let root = Context::new();
    root.provide_as("foo", "outer").unwrap();
    let results = Seq::new();
    let r = results.clone();
    let fiber = root
        .inject(["foo"], move |ctx| {
            let results = r.clone();
            async move {
                let (first, second) = (ctx.isolate("foo"), ctx.isolate("foo"));
                let withdraw = first.provide_as("foo", "first")?;
                second.provide_as("foo", "second")?;
                results.push(format!("{} {} {}", access(&ctx), access(&first), access(&second)));
                let r1 = results.clone();
                run(&first, move |ctx| r1.push(access(&ctx))).await;
                let r2 = results.clone();
                run(&second, move |ctx| r2.push(access(&ctx))).await;
                withdraw.dispose().await?;
                results.push(format!("{} | {} | {}", access(&ctx), access(&first), access(&second)));
                let reader = ctx.clone();
                ctx.on_dispose(move || assert_eq!(access(&reader), "outer"))?;
                Ok(())
            }
        })
        .unwrap();
    fiber.settled().await.unwrap();
    fiber.dispose().await.unwrap();
    assert_eq!(
        results.get(),
        vec![
            "outer first second".to_string(),
            "first".into(),
            "second".into(),
            format!("outer | {UNDECLARED} | second"),
        ]
    );
    assert!(root.log_buffer().is_empty());
}

#[tokio::test]
async fn access_check() {
    let root = Context::new();
    let results = Seq::new();
    let r = results.clone();
    run(&root, move |ctx| {
        r.push(ctx.get_as::<i32>("bar").unwrap_err());
        ctx.provide_as("foo", 0i32).unwrap();
        let error = ctx.provide_as("foo", 1i32).unwrap_err();
        assert_eq!(error.to_string(), "service \"foo\" has been registered at <root>");
    })
    .await;
    assert_eq!(results.get(), vec![AccessError::Undeclared { key: "bar".into() }]);
}

#[tokio::test]
async fn type_mismatch() {
    let root = Context::new();
    root.provide_as("foo", 1i32).unwrap();
    assert!(matches!(root.get_as::<String>("foo"), Err(AccessError::TypeMismatch { .. })));
}

#[tokio::test]
async fn service_inject_leak() {
    let root = Context::new();
    root.provide_as("foo", 1i32).unwrap();
    let fiber = root.inject(["foo"], |_| async { Ok(()) }).unwrap();
    fiber.settled().await.unwrap();
    assert!(fiber.context().get_as::<i32>("foo").is_ok());
    fiber.dispose().await.unwrap();
    assert_eq!(
        fiber.context().get_as::<i32>("foo").unwrap_err().to_string(),
        "cannot get required service \"foo\" in inactive context"
    );
}

// --- service.spec.ts ---

#[tokio::test]
async fn pending_inject() {
    let root = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    root.inject(["foo"], move |_| {
        c.fetch_add(1, Ordering::SeqCst);
        async { Ok(()) }
    })
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    // the provider's own load blocks the consumer until it completes
    root.plugin_def(
        &PluginDef::new(|ctx, _| async move {
            ctx.provide_as("foo", ())?;
            let (tx, rx) = tokio::sync::oneshot::channel();
            let tx = parking_lot::Mutex::new(Some(tx));
            ctx.on(move |_: &Custom| {
                if let Some(tx) = tx.lock().take() {
                    let _ = tx.send(());
                }
            })?;
            let _ = rx.await;
            Ok(())
        }),
        Value::Null,
    )
    .unwrap();
    settle().await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    root.emit(&Custom);
    settle().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn multiple_injects() {
    let root = Context::new();
    let started = Seq::new();
    let service = |name: &'static str, inject: Vec<&'static str>| {
        let started = started.clone();
        PluginDef::new(move |ctx, _| {
            let started = started.clone();
            async move {
                ctx.provide_as(name, ())?;
                started.push(name);
                Ok(())
            }
        })
        .inject(inject)
    };
    root.plugin_def(&service("foo", vec!["qux"]), Value::Null).unwrap();
    root.plugin_def(&service("bar", vec!["foo", "qux"]), Value::Null).unwrap();
    root.plugin_def(&service("qux", vec![]), Value::Null).unwrap();
    settle().await;
    assert_eq!(started.get(), vec!["qux", "foo", "bar"]);
}

fn foo_service(seq: &Seq<String>) -> PluginDef {
    let seq = seq.clone();
    PluginDef::new(move |ctx, _| {
        let seq = seq.clone();
        async move {
            let active = Arc::new(std::sync::atomic::AtomicBool::new(true));
            ctx.provide_as("foo", active.clone())?;
            ctx.on_dispose(move || {
                active.store(false, Ordering::SeqCst);
                seq.push("foo".into());
            })?;
            Ok(())
        }
    })
}

fn dependent(seq: &Seq<String>, calls: &Arc<AtomicUsize>) -> PluginDef {
    let (seq, calls) = (seq.clone(), calls.clone());
    PluginDef::new(move |ctx, _| {
        calls.fetch_add(1, Ordering::SeqCst);
        let seq = seq.clone();
        async move {
            let reader = ctx.clone();
            ctx.defer(
                "cleanup",
                Disposer::future(move || async move {
                    sleep(10).await;
                    let active = reader.get_as::<Arc<std::sync::atomic::AtomicBool>>("foo").unwrap();
                    seq.push(if active.load(Ordering::SeqCst) { "dependent" } else { "dependent (stale)" }.into());
                }),
            )?;
            Ok(())
        }
    })
    .inject(["foo"])
}

#[tokio::test(start_paused = true)]
async fn dispose_dependents_before_provider() {
    let root = Context::new();
    let seq = Seq::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let (foo, dep) = (foo_service(&seq), dependent(&seq, &calls));
    let fiber = root
        .plugin_def(
            &PluginDef::new(move |ctx, _| {
                let (foo, dep) = (foo.clone(), dep.clone());
                async move {
                    ctx.plugin_def(&foo, Value::Null)?;
                    ctx.plugin_def(&dep, Value::Null)?;
                    Ok(())
                }
            }),
            Value::Null,
        )
        .unwrap();
    settle().await;
    fiber.dispose().await.unwrap();
    assert_eq!(seq.get(), vec!["dependent", "foo"]);
}

#[tokio::test(start_paused = true)]
async fn dispose_dependents_before_provider_restarts() {
    let root = Context::new();
    let seq = Seq::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let fiber = root.plugin_def(&foo_service(&seq), Value::Null).unwrap();
    root.plugin_def(&dependent(&seq, &calls), Value::Null).unwrap();
    settle().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    fiber.restart().await.unwrap();
    assert_eq!(seq.get(), vec!["dependent", "foo"]);
    settle().await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn dispose_dependents_before_isolated_provider() {
    let root = Context::new();
    let label = RealmId::new();
    let (calls, disposed) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    root.isolate_in("foo", label).plugin_def(&counted(&calls, &disposed), Value::Null).unwrap();
    let fiber = run(&root, move |ctx| {
        ctx.isolate_in("foo", label).provide_as("foo", ()).unwrap();
    })
    .await;
    settle().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    fiber.dispose().await.unwrap();
    assert_eq!(disposed.load(Ordering::SeqCst), 1);
}
