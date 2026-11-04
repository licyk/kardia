//! Port of `packages/core/tests/events.spec.ts` (dispatch modes; the
//! stringly-typed waterfall and internal hooks are replaced by typed hooks).

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::{Custom, Gate, Seq};
use cordis_core::{Context, EffectHandle, Error, Event, PluginDef};
use parking_lot::Mutex;
use serde_json::{Value, json};

fn counter() -> (Arc<AtomicUsize>, impl Fn(&Custom) + Send + Sync + Clone + 'static) {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    (calls, move |_: &Custom| {
        c.fetch_add(1, Ordering::SeqCst);
    })
}

fn calls(counter: &Arc<AtomicUsize>) -> usize {
    counter.load(Ordering::SeqCst)
}

#[tokio::test]
async fn on_and_dispose() {
    let root = Context::new();
    let (count, listener) = counter();
    let handle = root.on(listener).unwrap();
    root.emit(&Custom);
    root.emit(&Custom);
    assert_eq!(calls(&count), 2);
    handle.dispose().await.unwrap();
    root.emit(&Custom);
    assert_eq!(calls(&count), 2);
    assert_eq!(root.listener_count::<Custom>(), 0);
}

#[tokio::test]
async fn once() {
    let root = Context::new();
    let (count, listener) = counter();
    let handle = root.once(move |event: &Custom| listener(event)).unwrap();
    root.emit(&Custom);
    root.emit(&Custom);
    assert_eq!(calls(&count), 1);
    handle.dispose().await.unwrap();
    assert_eq!(root.listener_count::<Custom>(), 0);
}

#[tokio::test]
async fn once_runs_once_when_an_earlier_listener_emits_recursively() {
    let root = Context::new();
    let nested = Arc::new(Mutex::new(false));
    let inner = root.clone();
    root.on(move |event: &Custom| {
        let mut nested = nested.lock();
        if *nested {
            return;
        }
        *nested = true;
        drop(nested);
        inner.emit(event);
    })
    .unwrap();
    let (count, listener) = counter();
    root.once(move |event: &Custom| listener(event)).unwrap();
    root.emit(&Custom);
    assert_eq!(calls(&count), 1);
}

#[tokio::test]
async fn once_runs_once_across_concurrent_serial_dispatches() {
    let root = Context::new();
    let barrier = Gate::new();
    let b = barrier.clone();
    root.on_async(move |_: &Custom| {
        let barrier = b.clone();
        async move { barrier.wait().await }
    })
    .unwrap();
    let (count, listener) = counter();
    root.once(move |event: &Custom| listener(event)).unwrap();
    let (first, second) = (root.clone(), root.clone());
    let first = tokio::spawn(async move { first.serial(&Custom).await });
    let second = tokio::spawn(async move { second.serial(&Custom).await });
    barrier.open();
    let _ = tokio::join!(first, second);
    assert_eq!(calls(&count), 1);
}

async fn dispatch(root: &Context, mode: &str) {
    match mode {
        "emit" => root.emit(&Custom),
        "parallel" => root.parallel(&Custom).await.unwrap(),
        "serial" => drop(root.serial(&Custom).await),
        _ => drop(root.bail(&Custom)),
    }
}

#[tokio::test]
async fn skips_listeners_removed_during_dispatch() {
    for mode in ["emit", "parallel", "serial", "bail"] {
        let root = Context::new();
        let slot: Arc<Mutex<Option<EffectHandle>>> = Default::default();
        let s = slot.clone();
        root.on(move |_: &Custom| {
            if let Some(handle) = s.lock().take() {
                drop(handle.dispose());
            }
        })
        .unwrap();
        let (removed, l1) = counter();
        *slot.lock() = Some(root.on(l1).unwrap());
        let (remaining, l2) = counter();
        root.on(l2).unwrap();
        dispatch(&root, mode).await;
        assert_eq!((calls(&removed), calls(&remaining)), (0, 1), "{mode}");
    }
}

#[tokio::test]
async fn defers_added_listeners_until_the_next_dispatch() {
    for mode in ["emit", "parallel", "serial", "bail"] {
        let root = Context::new();
        let (added, listener) = counter();
        let inner = root.clone();
        root.on(move |_: &Custom| {
            inner.on(listener.clone()).unwrap();
        })
        .unwrap();
        dispatch(&root, mode).await;
        assert_eq!(calls(&added), 0, "{mode}");
        dispatch(&root, mode).await;
        assert_eq!(calls(&added), 1, "{mode}");
    }
}

#[tokio::test]
async fn skips_a_listener_disposed_by_an_earlier_plugin_during_dispatch() {
    let root = Context::new();
    let slot: Arc<Mutex<Option<cordis_core::Fiber>>> = Default::default();
    let s = slot.clone();
    root.on_async(move |_: &Custom| {
        let child = s.lock().clone();
        async move {
            if let Some(child) = child {
                child.dispose().await.unwrap();
            }
        }
    })
    .unwrap();
    let (removed, listener) = counter();
    let child = root
        .plugin_def(
            &PluginDef::new(move |ctx, _| {
                let listener = listener.clone();
                async move {
                    ctx.on(listener)?;
                    Ok(())
                }
            }),
            Value::Null,
        )
        .unwrap();
    child.settled().await.unwrap();
    *slot.lock() = Some(child);
    root.serial(&Custom).await;
    assert_eq!(calls(&removed), 0);
}

/// An event delivered only to listeners whose context carries a matching
/// `filter` interception (the analogue of `Session` / `ctx.filter`).
struct Session {
    flag: bool,
}

impl Event for Session {
    type Output = Result<(), Error>;

    fn filter(&self, listener: &Context) -> bool {
        match listener.intercepts("filter").last() {
            Some(meta) => meta["flag"] == json!(self.flag),
            None => true,
        }
    }
}

#[tokio::test]
async fn filters_listeners_by_their_context() {
    let root = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    root.intercept("filter", json!({ "flag": true }))
        .on(move |_: &Session| {
            c.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .unwrap();
    root.emit(&Session { flag: false });
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    root.emit(&Session { flag: true });
    root.bail(&Session { flag: true });
    root.serial(&Session { flag: true }).await;
    root.parallel(&Session { flag: true }).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

#[tokio::test(start_paused = true)]
async fn parallel_collects_all_failures() {
    let root = Context::new();
    root.parallel(&Session { flag: true }).await.unwrap();
    root.on(|_: &Session| Err(Error::msg("test"))).unwrap();
    let settled = Arc::new(AtomicUsize::new(0));
    let s = settled.clone();
    root.on_async(move |_: &Session| {
        let settled = s.clone();
        async move {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            settled.fetch_add(1, Ordering::SeqCst);
            Err(Error::msg("async"))
        }
    })
    .unwrap();
    let error = root.parallel(&Session { flag: true }).await.unwrap_err();
    assert_eq!(error.message, "one or more listeners of event Session failed");
    let mut messages: Vec<_> = error.errors.iter().map(ToString::to_string).collect();
    messages.sort();
    assert_eq!(messages, vec!["async", "test"]);
    assert_eq!(settled.load(Ordering::SeqCst), 1);
}

struct Query;

impl Event for Query {
    type Output = Option<&'static str>;
}

#[tokio::test]
async fn bail_and_serial_return_the_first_bailed_result() {
    let root = Context::new();
    let seen = Seq::new();
    let s1 = seen.clone();
    root.on(move |_: &Query| {
        s1.push(1);
        None
    })
    .unwrap();
    root.on_async(|_: &Query| async { Some("async") }).unwrap();
    let s3 = seen.clone();
    root.on(move |_: &Query| {
        s3.push(3);
        Some("sync")
    })
    .unwrap();
    assert_eq!(root.bail(&Query), Some(Some("sync")));
    assert_eq!(root.serial(&Query).await, Some(Some("async")));
    assert_eq!(seen.get(), vec![1, 3, 1]);
}

#[tokio::test]
async fn listener_panics_are_logged() {
    let root = Context::new();
    root.on(|_: &Custom| panic!("listener failed")).unwrap();
    let (count, listener) = counter();
    root.on(listener).unwrap();
    root.emit(&Custom);
    assert_eq!(calls(&count), 1);
    assert!(root.log_buffer()[0].text.contains("listener failed"));
}

#[tokio::test]
async fn update_hooks_do_not_accumulate_across_reloads() {
    let root = Context::new();
    let received = Seq::new();
    let r = received.clone();
    let def = PluginDef::new(move |ctx, _| {
        let received = r.clone();
        async move {
            ctx.on_update(move |config: &Value| {
                received.push(config.clone());
                false
            })?;
            Ok(())
        }
    });
    let fiber = root.plugin_def(&def, json!(0)).unwrap();
    fiber.settled().await.unwrap();
    fiber.update(json!(1)).unwrap().await.unwrap();
    fiber.update(json!(2)).unwrap().await.unwrap();
    assert_eq!(received.get(), vec![json!(1), json!(2)]);
}
