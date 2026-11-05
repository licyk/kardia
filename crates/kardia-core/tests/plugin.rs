//! Port of `packages/core/tests/plugin.spec.ts` and `replace.spec.ts`.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::{Custom, Seq, settle, sleep};
use kardia_core::{Context, KardiaError, EffectMeta, Error, FiberState, Plugin, PluginDef, Replaced};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

fn recording(applied: &Seq<Value>) -> PluginDef {
    let applied = applied.clone();
    PluginDef::new(move |_, config| {
        applied.push(config);
        async { Ok(()) }
    })
}

#[tokio::test]
async fn dispose_by_plugin() {
    let root = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let fiber = root
        .plugin_def(
            &PluginDef::new(move |ctx, _| {
                let calls = c.clone();
                async move {
                    ctx.effect_sync("test", |scope| {
                        scope.defer_fn(move || {
                            calls.fetch_add(1, Ordering::SeqCst);
                        });
                        Ok(())
                    })?;
                    Ok(())
                }
            }),
            Value::Null,
        )
        .unwrap();
    fiber.settled().await.unwrap();
    assert_eq!(fiber.effects(), vec![EffectMeta::new("test", vec![])]);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    fiber.dispose().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    fiber.dispose().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Options {
    foo: String,
}

struct Typed;

static TYPED_SEEN: parking_lot::Mutex<Vec<String>> = parking_lot::Mutex::new(Vec::new());

impl Plugin for Typed {
    const NAME: &'static str = "typed";
    type Config = Options;

    async fn apply(_ctx: Context, config: Arc<Options>) -> kardia_core::Result<()> {
        TYPED_SEEN.lock().push(config.foo.clone());
        Ok(())
    }
}

#[tokio::test]
async fn apply_typed_and_closure_plugins() {
    let root = Context::new();
    let fiber = root.plugin::<Typed>(Options { foo: "bar".into() }).unwrap();
    fiber.settled().await.unwrap();
    assert_eq!(*TYPED_SEEN.lock(), vec!["bar"]);
    assert_eq!(fiber.name(), "typed");

    let applied = Seq::new();
    root.plugin_def(&recording(&applied), json!({ "bar": "foo" })).unwrap().settled().await.unwrap();
    assert_eq!(applied.get(), vec![json!({ "bar": "foo" })]);
}

#[tokio::test]
async fn inactive_context() {
    let root = Context::new();
    let results = Seq::new();
    let r = results.clone();
    let fiber = root
        .plugin_def(
            &PluginDef::new(move |ctx, _| {
                let results = r.clone();
                async move {
                    let inner = ctx.clone();
                    ctx.on_dispose(move || {
                        let noop = PluginDef::new(|_, _| async { Ok(()) });
                        results.push(inner.plugin_def(&noop, Value::Null).err().map(|e| e.to_string()));
                        results.push(inner.on_dispose(|| {}).err().map(|e| e.to_string()));
                        results.push(inner.on(|_: &Custom| {}).err().map(|e| e.to_string()));
                    })?;
                    Ok(())
                }
            }),
            Value::Null,
        )
        .unwrap();
    fiber.settled().await.unwrap();
    fiber.dispose().await.unwrap();
    let inactive = Some(KardiaError::InactiveEffect.to_string());
    assert_eq!(results.get(), vec![inactive.clone(), inactive.clone(), inactive]);
}

#[tokio::test]
async fn context_debug_names_the_fiber() {
    let root = Context::new();
    assert_eq!(format!("{root:?}"), "Context <root>");
    let names = Seq::new();
    let n = names.clone();
    let record = move |ctx: Context| {
        n.push(format!("{ctx:?}"));
        async { Ok(()) }
    };
    let (a, b) = (record.clone(), record);
    root.plugin_def(&PluginDef::new(move |ctx, _| a(ctx)), Value::Null).unwrap().settled().await.unwrap();
    root.plugin_def(&PluginDef::new(move |ctx, _| b(ctx)).name("bar"), Value::Null).unwrap().settled().await.unwrap();
    assert_eq!(names.get(), vec!["Context <root>", "Context <bar>"]);
}

fn nested_plugin(callback: &Arc<AtomicUsize>) -> PluginDef {
    let listener = |callback: &Arc<AtomicUsize>| {
        let callback = callback.clone();
        move |_: &Custom| {
            callback.fetch_add(1, Ordering::SeqCst);
        }
    };
    let (c1, c2, c3) = (callback.clone(), callback.clone(), callback.clone());
    PluginDef::new(move |ctx, _| {
        let (c1, c2, c3) = (c1.clone(), c2.clone(), c3.clone());
        async move {
            ctx.on(listener(&c1))?;
            let middle = PluginDef::new(move |ctx, _| {
                let (c2, c3) = (c2.clone(), c3.clone());
                async move {
                    ctx.on(listener(&c2))?;
                    let inner = PluginDef::new(move |ctx, _| {
                        let c3 = c3.clone();
                        async move {
                            ctx.on(listener(&c3))?;
                            Ok(())
                        }
                    });
                    ctx.plugin_def(&inner, Value::Null)?.settled().await
                }
            });
            ctx.plugin_def(&middle, Value::Null)?.settled().await
        }
    })
}

#[tokio::test]
async fn nested_plugins() {
    let root = Context::new();
    let callback = Arc::new(AtomicUsize::new(0));
    let c = callback.clone();
    root.on(move |_: &Custom| {
        c.fetch_add(1, Ordering::SeqCst);
    })
    .unwrap();
    let fiber = root.plugin_def(&nested_plugin(&callback), Value::Null).unwrap();
    fiber.settled().await.unwrap();
    assert_eq!(root.registry().size(), 3);
    root.emit(&Custom);
    assert_eq!(callback.swap(0, Ordering::SeqCst), 4);

    fiber.dispose().await.unwrap();
    assert_eq!(root.registry().size(), 0);
    root.emit(&Custom);
    assert_eq!(callback.swap(0, Ordering::SeqCst), 1);

    fiber.dispose().await.unwrap();
    root.emit(&Custom);
    assert_eq!(callback.swap(0, Ordering::SeqCst), 1);
}

#[tokio::test]
async fn listener_snapshot_is_restored() {
    let root = Context::new();
    let callback = Arc::new(AtomicUsize::new(0));
    let plugin = nested_plugin(&callback);
    let before = root.listener_count::<Custom>();
    let fiber = root.plugin_def(&plugin, Value::Null).unwrap();
    fiber.settled().await.unwrap();
    let after = root.listener_count::<Custom>();
    fiber.dispose().await.unwrap();
    assert_eq!(root.listener_count::<Custom>(), before);
    root.plugin_def(&plugin, Value::Null).unwrap().settled().await.unwrap();
    assert_eq!(root.listener_count::<Custom>(), after);
}

#[tokio::test]
async fn root_dispose() {
    let root = Context::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let fiber = root
        .plugin_def(
            &PluginDef::new(move |ctx, _| {
                let calls = c.clone();
                async move {
                    ctx.on_dispose(move || {
                        calls.fetch_add(1, Ordering::SeqCst);
                    })?;
                    Ok(())
                }
            }),
            Value::Null,
        )
        .unwrap();
    assert_eq!(root.fiber().id().get(), 0);
    assert_eq!(fiber.id().get(), 1);
    assert_eq!(root.fiber().effect_count(), 1);
    fiber.settled().await.unwrap();
    root.fiber().dispose().await.unwrap();
    assert!(fiber.is_removed());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(root.fiber().effect_count(), 0);
    assert_eq!(root.fiber().state(), FiberState::Active);
    root.fiber().dispose().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(root.fiber().effect_count(), 0);
}

#[tokio::test]
async fn a_panicking_apply_fails_the_fiber() {
    let root = Context::new();
    let fiber = root.plugin_def(&PluginDef::new(|_, _| async { panic!("boom") }), Value::Null).unwrap();
    let error = fiber.settled().await.unwrap_err();
    assert!(error.to_string().contains("boom"));
    assert_eq!(fiber.state(), FiberState::Failed);
}

// --- replace.spec.ts ---

fn counting(calls: &Arc<AtomicUsize>) -> PluginDef {
    let calls = calls.clone();
    PluginDef::new(move |_, _| {
        calls.fetch_add(1, Ordering::SeqCst);
        async { Ok(()) }
    })
}

#[tokio::test]
async fn swaps_the_implementation_behind_a_fiber_in_place() {
    let root = Context::new();
    let disposed = Arc::new(AtomicUsize::new(0));
    let d = disposed.clone();
    let old = PluginDef::new(move |ctx, _| {
        let disposed = d.clone();
        async move {
            ctx.on_dispose(move || {
                disposed.fetch_add(1, Ordering::SeqCst);
            })?;
            Ok(())
        }
    });
    let applied = Seq::new();
    let next = recording(&applied);
    let fiber = root.plugin_def(&old, json!({ "foo": 1 })).unwrap();
    fiber.settled().await.unwrap();
    let id = fiber.id();

    let replaced = root.registry().replace(&old, &next);
    for fiber in &replaced {
        fiber.settled().await.unwrap();
    }
    assert_eq!(replaced, vec![fiber.clone()]);
    assert_eq!(disposed.load(Ordering::SeqCst), 1);
    assert_eq!(applied.get(), vec![json!({ "foo": 1 })]);
    assert_eq!(fiber.id(), id);
    assert_eq!(fiber.state(), FiberState::Active);
    assert_eq!(fiber.runtime(), root.registry().get(&next));
    assert_eq!(root.registry().keys(), vec![next.key()]);
}

#[tokio::test]
async fn emits_replaced_for_each_moved_fiber() {
    let root = Context::new();
    let old = PluginDef::new(|_, _| async { Ok(()) });
    let next = PluginDef::new(|_, _| async { Ok(()) });
    let runtimes = Seq::new();
    let r = runtimes.clone();
    root.on(move |event: &Replaced| r.push(event.fiber.runtime())).unwrap();
    let a = root.plugin_def(&old, Value::Null).unwrap();
    let b = root.plugin_def(&old, Value::Null).unwrap();
    let replaced = root.registry().replace(&old, &next);
    assert_eq!(replaced, vec![a, b]);
    assert_eq!(runtimes.get(), vec![Some(next.key()), Some(next.key())]);
}

#[tokio::test]
async fn validates_the_config_with_the_new_schema() {
    let root = Context::new();
    let with_version = |version: i64| {
        move |value: &Value| {
            let mut value = value.clone();
            value["version"] = json!(version);
            Ok(value)
        }
    };
    let applied = Seq::new();
    let old = PluginDef::new(|_, _| async { Ok(()) }).validate_with(with_version(1));
    let next = recording(&applied).validate_with(with_version(2));
    root.plugin_def(&old, json!({ "foo": 1 })).unwrap().settled().await.unwrap();
    for fiber in root.registry().replace(&old, &next) {
        fiber.settled().await.unwrap();
    }
    assert_eq!(applied.get(), vec![json!({ "foo": 1, "version": 2 })]);
}

#[tokio::test]
async fn takes_the_dependencies_of_the_new_plugin() {
    let root = Context::new();
    let old = PluginDef::new(|_, _| async { Ok(()) });
    let calls = Arc::new(AtomicUsize::new(0));
    let next = counting(&calls).inject(["dep"]);
    let fiber = root.plugin_def(&old, Value::Null).unwrap();
    fiber.settled().await.unwrap();
    root.registry().replace(&old, &next);
    fiber.settled().await.unwrap();
    assert_eq!(fiber.inject().keys().map(|k| k.to_string()).collect::<Vec<_>>(), vec!["dep"]);
    assert_eq!(fiber.state(), FiberState::Pending);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    root.provide_as("dep", ()).unwrap();
    fiber.settled().await.unwrap();
    assert_eq!(fiber.state(), FiberState::Active);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn starts_a_pending_fiber_when_the_new_plugin_drops_or_finds_the_dependency() {
    let root = Context::new();
    root.provide_as("available", ()).unwrap();
    for inject in [vec![], vec!["available"]] {
        let old = PluginDef::new(|_, _| async { Ok(()) }).inject(["missing"]);
        let calls = Arc::new(AtomicUsize::new(0));
        let fiber = root.plugin_def(&old, Value::Null).unwrap();
        fiber.settled().await.unwrap();
        assert_eq!(fiber.state(), FiberState::Pending);
        root.registry().replace(&old, &counting(&calls).inject(inject));
        fiber.settled().await.unwrap();
        assert_eq!(fiber.state(), FiberState::Active);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn stops_depending_on_a_dependency_that_the_new_plugin_drops() {
    let root = Context::new();
    let provider = root
        .plugin_def(
            &PluginDef::new(|ctx, _| async move {
                ctx.provide_as("dep", ())?;
                Ok(())
            }),
            Value::Null,
        )
        .unwrap();
    provider.settled().await.unwrap();
    let old = PluginDef::new(|_, _| async { Ok(()) }).inject(["dep"]);
    let calls = Arc::new(AtomicUsize::new(0));
    let fiber = root.plugin_def(&old, Value::Null).unwrap();
    fiber.settled().await.unwrap();
    root.registry().replace(&old, &counting(&calls));
    fiber.settled().await.unwrap();
    // the provider does not wait for the fiber to unload, nor is the fiber reloaded
    tokio::select! {
        result = provider.restart() => result.unwrap(),
        _ = sleep(1000) => panic!("not settled"),
    }
    assert_eq!(fiber.state(), FiberState::Active);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn reloads_a_consumer_of_a_replaced_service() {
    let root = Context::new();
    let provider = |value: &'static str| {
        PluginDef::new(move |ctx, _| async move {
            ctx.provide_as("replaced", value)?;
            Ok(())
        })
    };
    let (old, next) = (provider("old"), provider("next"));
    let values = Seq::new();
    let v = values.clone();
    root.plugin_def(&old, Value::Null).unwrap().settled().await.unwrap();
    let consumer = root
        .plugin_def(
            &PluginDef::new(move |ctx, _| {
                let values = v.clone();
                async move {
                    values.push(*ctx.get_as::<&str>("replaced")?);
                    Ok(())
                }
            })
            .inject(["replaced"]),
            Value::Null,
        )
        .unwrap();
    consumer.settled().await.unwrap();
    let [fiber]: [_; 1] = root.registry().replace(&old, &next).try_into().unwrap();
    fiber.settled().await.unwrap();
    consumer.settled().await.unwrap();
    assert_eq!(values.get(), vec!["old", "next"]);
    assert_eq!(consumer.state(), FiberState::Active);
}

#[tokio::test]
async fn merges_the_fibers_into_a_registered_plugin() {
    let root = Context::new();
    let (old_applied, next_applied) = (Seq::new(), Seq::new());
    let (old, next) = (recording(&old_applied), recording(&next_applied));
    let a = root.plugin_def(&old, json!("a")).unwrap();
    let b = root.plugin_def(&next, json!("b")).unwrap();
    a.settled().await.unwrap();
    b.settled().await.unwrap();
    root.registry().replace(&old, &next);
    a.settled().await.unwrap();
    assert_eq!(root.registry().size(), 1);
    assert_eq!(a.runtime(), b.runtime());
    let mut configs: Vec<_> = root.registry().fibers(&next).iter().map(|f| f.config()).collect();
    configs.sort_by_key(|c| c.to_string());
    assert_eq!(configs, vec![json!("a"), json!("b")]);
    // the fiber of `next` is not restarted
    assert_eq!(next_applied.get(), vec![json!("b"), json!("a")]);
}

#[tokio::test]
async fn restarts_a_failed_fiber() {
    let root = Context::new();
    let old = PluginDef::new(|_, _| async { Err(Error::msg("failed")) });
    let calls = Arc::new(AtomicUsize::new(0));
    let fiber = root.plugin_def(&old, Value::Null).unwrap();
    assert!(fiber.settled().await.is_err());
    assert_eq!(fiber.state(), FiberState::Failed);
    root.registry().replace(&old, &counting(&calls));
    fiber.settled().await.unwrap();
    assert_eq!(fiber.state(), FiberState::Active);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn leaves_a_fiber_failed_when_the_new_plugin_fails_to_apply() {
    let root = Context::new();
    let old = PluginDef::new(|_, _| async { Ok(()) });
    let fiber = root.plugin_def(&old, Value::Null).unwrap();
    fiber.settled().await.unwrap();
    root.registry().replace(&old, &PluginDef::new(|_, _| async { Err(Error::msg("failed")) }));
    assert_eq!(fiber.settled().await.unwrap_err().to_string(), "failed");
    assert_eq!(fiber.state(), FiberState::Failed);
}

#[tokio::test]
async fn forwards_the_old_plugin_to_the_new_one() {
    let root = Context::new();
    let (old_calls, next_calls) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let old = counting(&old_calls);
    let next = counting(&next_calls).inject(["dep"]);
    root.plugin_def(&old, Value::Null).unwrap().settled().await.unwrap();
    root.registry().replace(&old, &next);
    let fiber = root.plugin_def(&old, Value::Null).unwrap();
    assert_eq!(fiber.inject().keys().map(|k| k.to_string()).collect::<Vec<_>>(), vec!["dep"]);
    assert_eq!(fiber.runtime(), root.registry().get(&next));
    assert_eq!(root.registry().get(&old), root.registry().get(&next));
    // the alias is not a registered plugin
    assert_eq!(root.registry().keys(), vec![next.key()]);
    root.provide_as("dep", ()).unwrap();
    settle().await;
    assert_eq!(old_calls.load(Ordering::SeqCst), 1);
    assert_eq!(next_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn forwards_a_plugin_that_is_not_registered() {
    let root = Context::new();
    let (old_calls, next_calls) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (old, next) = (counting(&old_calls), counting(&next_calls));
    assert!(root.registry().replace(&old, &next).is_empty());
    assert_eq!(root.registry().size(), 0);
    root.plugin_def(&old, Value::Null).unwrap().settled().await.unwrap();
    assert_eq!(old_calls.load(Ordering::SeqCst), 0);
    assert_eq!(next_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn follows_a_chain_of_replacements_and_switches_back() {
    let root = Context::new();
    let calls: Vec<_> = (0..3).map(|_| Arc::new(AtomicUsize::new(0))).collect();
    let [a, b, c]: [PluginDef; 3] = calls.iter().map(counting).collect::<Vec<_>>().try_into().unwrap();
    let fiber = root.plugin_def(&a, Value::Null).unwrap();
    fiber.settled().await.unwrap();
    root.registry().replace(&a, &b);
    root.registry().replace(&a, &c);
    fiber.settled().await.unwrap();
    assert_eq!(fiber.runtime(), Some(c.key()));
    assert_eq!(root.registry().keys(), vec![c.key()]);

    root.registry().replace(&c, &a);
    fiber.settled().await.unwrap();
    assert_eq!(fiber.runtime(), Some(a.key()));
    assert_eq!(root.registry().keys(), vec![a.key()]);
    assert_eq!(calls[0].load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn unregisters_the_new_plugin_when_its_last_fiber_is_disposed() {
    let root = Context::new();
    let old = PluginDef::new(|_, _| async { Ok(()) });
    let next = PluginDef::new(|_, _| async { Ok(()) });
    let fiber = root.plugin_def(&old, Value::Null).unwrap();
    fiber.settled().await.unwrap();
    root.registry().replace(&old, &next);
    fiber.settled().await.unwrap();
    fiber.dispose().await.unwrap();
    assert_eq!(root.registry().size(), 0);
    assert!(!root.registry().has(&next));
}
