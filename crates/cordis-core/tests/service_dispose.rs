//! Port of `packages/core/tests/service-dispose.spec.ts`.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use common::{Custom, Gate, Seq, errors, settle};
use cordis_core::{Context, Disposer, Error, Fiber, PluginDef, RealmId};
use serde_json::Value;

#[derive(Clone)]
struct Fixture {
    root: Context,
    timeline: Seq<String>,
    gate: Gate,
    closed: Arc<AtomicBool>,
}

/// The service provided by [`Fixture::provider`].
struct Foo {
    timeline: Seq<String>,
    closed: Arc<AtomicBool>,
}

impl Foo {
    fn read(&self) {
        let state = if self.closed.load(Ordering::SeqCst) { "closed" } else { "open" };
        self.timeline.push(format!("consumer read: {state}"));
    }
}

impl Fixture {
    fn new() -> Self {
        Self { root: Context::new(), timeline: Seq::new(), gate: Gate::new(), closed: Arc::new(AtomicBool::new(false)) }
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Provides `foo`, then opens a resource closed by its last inverse.
    fn provider(&self) -> PluginDef {
        let fx = self.clone();
        PluginDef::new(move |ctx, _| {
            let fx = fx.clone();
            async move {
                ctx.provide_as("foo", Foo { timeline: fx.timeline.clone(), closed: fx.closed.clone() })?;
                fx.closed.store(false, Ordering::SeqCst);
                ctx.on_dispose(move || {
                    fx.closed.store(true, Ordering::SeqCst);
                    fx.timeline.push("provider closed".into());
                })?;
                Ok(())
            }
        })
        .name("provider")
    }

    /// Injects `foo` and reads it after the gate opens during cleanup.
    fn gated_reader(&self) -> PluginDef {
        let gate = self.gate.clone();
        PluginDef::new(move |ctx, _| {
            let gate = gate.clone();
            async move {
                let reader = ctx.clone();
                ctx.defer(
                    "cleanup",
                    Disposer::future(move || async move {
                        gate.wait().await;
                        reader.get_as::<Foo>("foo").unwrap().read();
                    }),
                )?;
                Ok(())
            }
        })
        .inject(["foo"])
    }
}

fn noop() -> PluginDef {
    PluginDef::new(|_, _| async { Ok(()) })
}

fn start(ctx: &Context, def: &PluginDef) -> Fiber {
    ctx.plugin_def(def, Value::Null).unwrap()
}

async fn started(ctx: &Context, def: &PluginDef) -> Fiber {
    let fiber = start(ctx, def);
    fiber.settled().await.unwrap();
    fiber
}

async fn releases_binding_after_providing_in_another_realm(action: &str) {
    let root = Context::new();
    let provider = started(
        &root,
        &PluginDef::new(|ctx, _| async move {
            ctx.provide_as("foo", 1i32)?;
            Ok(())
        }),
    )
    .await;
    let consumer = started(
        &root,
        &PluginDef::new(|ctx, _| async move {
            let outer = *ctx.get_as::<i32>("foo")?;
            ctx.isolate("foo").provide_as("foo", outer + 1)?;
            Ok(())
        })
        .inject(["foo"]),
    )
    .await;
    let disposed = Seq::new();
    let task = {
        let (root, provider, consumer, disposed) = (root.clone(), provider.clone(), consumer.clone(), disposed.clone());
        let action = action.to_string();
        tokio::spawn(async move {
            match action.as_str() {
                "root" => root.fiber().dispose().await.unwrap(),
                "provider" => provider.dispose().await.unwrap(),
                _ => {
                    consumer.dispose().await.unwrap();
                    provider.dispose().await.unwrap();
                }
            }
            disposed.push(true);
        })
    };
    settle().await;
    assert_eq!(disposed.get(), vec![true], "{action}");
    task.await.unwrap();
}

#[tokio::test]
async fn releases_an_injected_binding_after_providing_the_same_name_in_another_realm() {
    for action in ["consumer", "provider", "root"] {
        releases_binding_after_providing_in_another_realm(action).await;
    }
}

#[tokio::test]
async fn waits_for_a_disposing_consumer_that_has_already_left_the_registry() {
    let root = Context::new();
    let (gate, timeline) = (Gate::new(), Seq::new());
    let provider = started(&root, &noop()).await;
    let withdraw = provider.context().provide_as("foo", ()).unwrap();
    let (g, t) = (gate.clone(), timeline.clone());
    let consumer = started(
        &root,
        &PluginDef::new(move |ctx, _| {
            let (gate, timeline) = (g.clone(), t.clone());
            async move {
                ctx.defer(
                    "cleanup",
                    Disposer::future(move || async move {
                        gate.wait().await;
                        timeline.push("consumer finished".to_string());
                    }),
                )?;
                Ok(())
            }
        })
        .inject(["foo"]),
    )
    .await;
    let disposing = consumer.dispose();
    let withdrawn = Arc::new(AtomicBool::new(false));
    let w = withdrawn.clone();
    let t = timeline.clone();
    let withdrawal = withdraw.dispose();
    let withdrawing = tokio::spawn(async move {
        withdrawal.await.unwrap();
        w.store(true, Ordering::SeqCst);
        t.push("withdraw completed".to_string());
    });
    settle().await;
    let withdrawn_during_cleanup = withdrawn.load(Ordering::SeqCst);
    gate.open();
    let (a, b) = tokio::join!(disposing, withdrawing);
    a.unwrap();
    b.unwrap();
    assert!(!withdrawn_during_cleanup);
    assert_eq!(timeline.get(), vec!["consumer finished", "withdraw completed"]);
}

#[tokio::test]
async fn withdraws_one_binding_without_waiting_for_another_binding_from_the_same_provider() {
    let root = Context::new();
    let gate = Gate::new();
    let provider = started(&root, &noop()).await;
    let withdraw = provider.context().provide_as("foo", ()).unwrap();
    provider.context().provide_as("bar", ()).unwrap();
    let first = started(&root, &noop().inject(["foo"])).await;
    let g = gate.clone();
    let second = started(
        &root,
        &PluginDef::new(move |ctx, _| {
            let gate = g.clone();
            async move {
                ctx.defer("cleanup", Disposer::future(move || async move { gate.wait().await }))?;
                Ok(())
            }
        })
        .inject(["bar"]),
    )
    .await;
    let disposing = second.dispose();
    let withdrawal = withdraw.dispose();
    let withdrawn = Seq::new();
    let w = withdrawn.clone();
    let withdrawing = tokio::spawn(async move {
        withdrawal.await.unwrap();
        w.push(true);
    });
    settle().await;
    let withdrawn_during_other_cleanup = withdrawn.len() == 1;
    gate.open();
    let (a, b) = tokio::join!(disposing, withdrawing);
    a.unwrap();
    b.unwrap();
    assert!(withdrawn_during_other_cleanup);
    first.dispose().await.unwrap();
}

async fn retains_resources_during_consumer_cleanup(action: &str) {
    let fx = Fixture::new();
    let root = fx.root.clone();
    let provider = started(&root, &fx.provider()).await;
    let consumer = started(&root, &fx.gated_reader()).await;
    let task = {
        let (root, provider, consumer, action) = (root.clone(), provider.clone(), consumer.clone(), action.to_string());
        tokio::spawn(async move {
            match action.as_str() {
                "root" => root.fiber().dispose().await.unwrap(),
                "restart" => provider.restart().await.unwrap(),
                "consumer first" => {
                    let (a, b) = tokio::join!(consumer.dispose(), provider.dispose());
                    a.unwrap();
                    b.unwrap();
                }
                "provider first" => {
                    let (a, b) = tokio::join!(provider.dispose(), consumer.dispose());
                    a.unwrap();
                    b.unwrap();
                }
                _ => provider.dispose().await.unwrap(),
            }
        })
    };
    settle().await;
    let closed_during_drain = fx.is_closed();
    fx.gate.open();
    task.await.unwrap();
    assert!(!closed_during_drain, "{action}");
    assert_eq!(fx.timeline.get(), vec!["consumer read: open", "provider closed"], "{action}");
    if action == "restart" {
        consumer.settled().await.unwrap();
        assert!(consumer.context().get_as::<Foo>("foo").is_ok());
    }
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn retains_resources_during_consumer_cleanup_all_actions() {
    for action in ["provider", "root", "provider first", "consumer first", "restart"] {
        retains_resources_during_consumer_cleanup(action).await;
    }
}

#[tokio::test]
async fn retains_transitive_dependencies_while_a_consumer_providing_another_service_drains() {
    let fx = Fixture::new();
    let fibers = Seq::new();
    let (f, provider, timeline, gate) = (fibers.clone(), fx.provider(), fx.timeline.clone(), fx.gate.clone());
    let owner = started(
        &fx.root,
        &PluginDef::new(move |ctx, _| {
            let (fibers, provider, timeline, gate) = (f.clone(), provider.clone(), timeline.clone(), gate.clone());
            async move {
                fibers.push(ctx.plugin_def(&provider, Value::Null)?);
                let middle = PluginDef::new(|ctx, _| async move {
                    ctx.provide_as("bar", ())?;
                    let reader = ctx.clone();
                    ctx.on_dispose(move || reader.get_as::<Foo>("foo").unwrap().read())?;
                    Ok(())
                })
                .inject(["foo"]);
                fibers.push(ctx.plugin_def(&middle, Value::Null)?);
                let last = PluginDef::new(move |ctx, _| {
                    let (timeline, gate) = (timeline.clone(), gate.clone());
                    async move {
                        ctx.defer(
                            "cleanup",
                            Disposer::future(move || async move {
                                timeline.push("consumer draining".into());
                                gate.wait().await;
                                timeline.push("consumer finished".into());
                            }),
                        )?;
                        Ok(())
                    }
                })
                .inject(["bar"]);
                fibers.push(ctx.plugin_def(&last, Value::Null)?);
                Ok(())
            }
        }),
    )
    .await;
    for fiber in fibers.get() {
        fiber.settled().await.unwrap();
    }
    let task = owner.dispose();
    settle().await;
    let closed_during_drain = fx.is_closed();
    fx.gate.open();
    task.await.unwrap();
    assert!(!closed_during_drain);
    assert_eq!(
        fx.timeline.get(),
        vec!["consumer draining", "consumer finished", "consumer read: open", "provider closed"]
    );
}

async fn retains_dependencies_through_in_flight_setup(fail: bool) {
    let fx = Fixture::new();
    let logged = errors(&fx.root);
    let provider = started(&fx.root, &fx.provider()).await;
    let (timeline, gate) = (fx.timeline.clone(), fx.gate.clone());
    let consumer = start(
        &fx.root,
        &PluginDef::new(move |ctx, _| {
            let (timeline, gate) = (timeline.clone(), gate.clone());
            async move {
                timeline.push("setup started".into());
                let reader = ctx.clone();
                let t = timeline.clone();
                ctx.on_dispose(move || {
                    reader.get_as::<Foo>("foo").unwrap().read();
                    t.push("setup rolled back".into());
                })?;
                gate.wait().await;
                ctx.get_as::<Foo>("foo")?.read();
                if fail { Err(Error::msg("setup failed")) } else { Ok(()) }
            }
        })
        .inject(["foo"]),
    );
    settle().await;
    let task = provider.dispose();
    settle().await;
    let closed_during_setup = fx.is_closed();
    fx.gate.open();
    task.await.unwrap();
    consumer.dispose().await.unwrap();
    assert!(!closed_during_setup);
    assert_eq!(
        fx.timeline.get(),
        vec!["setup started", "consumer read: open", "consumer read: open", "setup rolled back", "provider closed"]
    );
    assert_eq!(logged.len(), usize::from(fail));
}

#[tokio::test]
async fn retains_dependencies_through_in_flight_setup_and_rollback() {
    retains_dependencies_through_in_flight_setup(false).await;
    retains_dependencies_through_in_flight_setup(true).await;
}

#[tokio::test]
async fn releases_dependencies_after_a_cleanup_error() {
    let fx = Fixture::new();
    started(&fx.root, &fx.provider()).await;
    started(
        &fx.root,
        &PluginDef::new(|ctx, _| async move {
            let reader = ctx.clone();
            ctx.defer(
                "cleanup",
                Disposer::fallible(move || {
                    reader.get_as::<Foo>("foo")?.read();
                    Err(Error::msg("cleanup failed"))
                }),
            )?;
            Ok(())
        })
        .inject(["foo"]),
    )
    .await;
    fx.root.fiber().dispose().await.unwrap();
    assert_eq!(fx.timeline.get(), vec!["consumer read: open", "provider closed"]);
    // exporters registered on the root are reverted with it; the buffer is built in
    let logged: Vec<_> = fx.root.log_buffer().into_iter().map(|message| message.text).collect();
    assert_eq!(logged, vec!["cleanup failed"]);
}

#[tokio::test]
async fn keeps_provider_listeners_alive_through_the_final_consumer_event() {
    let root = Context::new();
    let received = Arc::new(AtomicUsize::new(0));
    let r = received.clone();
    started(
        &root,
        &PluginDef::new(move |ctx, _| {
            let received = r.clone();
            async move {
                ctx.provide_as("foo", ())?;
                ctx.on(move |_: &Custom| {
                    received.fetch_add(1, Ordering::SeqCst);
                })?;
                Ok(())
            }
        }),
    )
    .await;
    started(
        &root,
        &PluginDef::new(|ctx, _| async move {
            let emitter = ctx.clone();
            ctx.defer(
                "cleanup",
                Disposer::future(move || async move {
                    settle().await;
                    emitter.emit(&Custom);
                }),
            )?;
            Ok(())
        })
        .inject(["foo"]),
    )
    .await;
    root.fiber().dispose().await.unwrap();
    assert_eq!(received.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn allows_unrelated_providers_and_sibling_effects_to_clean_up_concurrently() {
    let fx = Fixture::new();
    let (independent, sibling) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    started(&fx.root, &fx.provider()).await;
    let i = independent.clone();
    started(
        &fx.root,
        &PluginDef::new(move |ctx, _| {
            let independent = i.clone();
            async move {
                ctx.provide_as("bar", ())?;
                ctx.on_dispose(move || {
                    independent.fetch_add(1, Ordering::SeqCst);
                })?;
                Ok(())
            }
        }),
    )
    .await;
    let (s, gate) = (sibling.clone(), fx.gate.clone());
    started(
        &fx.root,
        &PluginDef::new(move |ctx, _| {
            let (sibling, gate) = (s.clone(), gate.clone());
            async move {
                let reader = ctx.clone();
                ctx.defer(
                    "cleanup",
                    Disposer::future(move || async move {
                        gate.wait().await;
                        reader.get_as::<Foo>("foo").unwrap().read();
                    }),
                )?;
                ctx.on_dispose(move || {
                    sibling.fetch_add(1, Ordering::SeqCst);
                })?;
                Ok(())
            }
        })
        .inject(["foo"]),
    )
    .await;
    let task = fx.root.fiber().dispose();
    settle().await;
    let during_drain = (fx.is_closed(), independent.load(Ordering::SeqCst), sibling.load(Ordering::SeqCst));
    fx.gate.open();
    task.await.unwrap();
    assert_eq!(during_drain, (false, 1, 1));
}

async fn composed_provider_effects(owner_shutdown: bool) {
    let root = Context::new();
    let (gate, timeline) = (Gate::new(), Seq::<String>::new());
    let closed = Arc::new(AtomicBool::new(false));
    let fibers = Seq::new();
    let (g, t, c, f) = (gate.clone(), timeline.clone(), closed.clone(), fibers.clone());
    let owner = started(
        &root,
        &PluginDef::new(move |ctx, _| {
            let (gate, timeline, closed, fibers) = (g.clone(), t.clone(), c.clone(), f.clone());
            async move {
                let (t2, c2) = (timeline.clone(), closed.clone());
                let provider = PluginDef::new(move |ctx, _| {
                    let (timeline, closed) = (t2.clone(), c2.clone());
                    async move {
                        ctx.effect_sync("composed", |scope| {
                            let (t3, closed2) = (timeline.clone(), closed.clone());
                            scope.adopt(ctx.on_dispose(move || {
                                closed2.store(true, Ordering::SeqCst);
                                t3.push("provider closed".into());
                            })?);
                            scope.adopt(
                                ctx.provide_as("foo", Foo { timeline: timeline.clone(), closed: closed.clone() })?,
                            );
                            Ok(())
                        })?;
                        Ok(())
                    }
                });
                fibers.push(ctx.plugin_def(&provider, Value::Null)?);
                let consumer = PluginDef::new(move |ctx, _| {
                    let gate = gate.clone();
                    async move {
                        let reader = ctx.clone();
                        ctx.defer(
                            "cleanup",
                            Disposer::future(move || async move {
                                gate.wait().await;
                                reader.get_as::<Foo>("foo").unwrap().read();
                            }),
                        )?;
                        Ok(())
                    }
                })
                .inject(["foo"]);
                fibers.push(ctx.plugin_def(&consumer, Value::Null)?);
                Ok(())
            }
        }),
    )
    .await;
    let [provider, consumer]: [Fiber; 2] = fibers.get().try_into().unwrap();
    provider.settled().await.unwrap();
    consumer.settled().await.unwrap();
    let task = if owner_shutdown { owner.dispose() } else { provider.dispose() };
    settle().await;
    let closed_during_drain = closed.load(Ordering::SeqCst);
    gate.open();
    task.await.unwrap();
    assert!(!closed_during_drain);
    assert_eq!(timeline.get(), vec!["consumer read: open", "provider closed"]);
    owner.dispose().await.unwrap();
}

#[tokio::test]
async fn supports_composed_provider_effects() {
    composed_provider_effects(false).await;
    composed_provider_effects(true).await;
}

#[tokio::test]
async fn keeps_an_inherited_service_accessible_to_a_child_in_a_composed_effect() {
    let root = Context::new();
    let read = Arc::new(AtomicUsize::new(0));
    let children = Seq::new();
    let (r, c) = (read.clone(), children.clone());
    let owner = started(
        &root,
        &PluginDef::new(move |ctx, _| {
            let (read, children) = (r.clone(), c.clone());
            async move {
                ctx.provide_as("foo", read)?;
                ctx.effect_sync("composed", |scope| {
                    let child = ctx.plugin_def(
                        &PluginDef::new(|ctx, _| async move {
                            let reader = ctx.clone();
                            ctx.defer(
                                "read",
                                Disposer::fallible(move || {
                                    reader.get_as::<Arc<AtomicUsize>>("foo")?.fetch_add(1, Ordering::SeqCst);
                                    Ok(())
                                }),
                            )?;
                            Ok(())
                        }),
                        Value::Null,
                    )?;
                    scope.adopt(&child);
                    children.push(child);
                    Ok(())
                })?;
                Ok(())
            }
        }),
    )
    .await;
    children.get()[0].settled().await.unwrap();
    let logged = errors(&root);
    owner.dispose().await.unwrap();
    assert_eq!(read.load(Ordering::SeqCst), 1);
    assert_eq!(logged.len(), 0);
}

#[tokio::test]
async fn retains_an_ancestor_injection_until_its_child_finishes_cleanup() {
    let fx = Fixture::new();
    started(&fx.root, &fx.provider()).await;
    let children = Seq::new();
    let (c, gate) = (children.clone(), fx.gate.clone());
    started(
        &fx.root,
        &PluginDef::new(move |ctx, _| {
            let (children, gate) = (c.clone(), gate.clone());
            async move {
                let child = PluginDef::new(move |ctx, _| {
                    let gate = gate.clone();
                    async move {
                        let reader = ctx.clone();
                        ctx.defer(
                            "cleanup",
                            Disposer::future(move || async move {
                                gate.wait().await;
                                reader.get_as::<Foo>("foo").unwrap().read();
                            }),
                        )?;
                        Ok(())
                    }
                });
                children.push(ctx.plugin_def(&child, Value::Null)?);
                Ok(())
            }
        })
        .inject(["foo"]),
    )
    .await;
    children.get()[0].settled().await.unwrap();
    let task = fx.root.fiber().dispose();
    settle().await;
    let closed_during_drain = fx.is_closed();
    fx.gate.open();
    task.await.unwrap();
    assert!(!closed_during_drain);
    assert_eq!(fx.timeline.get(), vec!["consumer read: open", "provider closed"]);
}

#[tokio::test]
async fn waits_for_a_child_that_explicitly_injects_its_parent_service() {
    let fx = Fixture::new();
    let provider = started(&fx.root, &fx.provider()).await;
    started(provider.context(), &fx.gated_reader()).await;
    let task = provider.dispose();
    settle().await;
    let closed_during_drain = fx.is_closed();
    fx.gate.open();
    task.await.unwrap();
    assert!(!closed_during_drain);
    assert_eq!(fx.timeline.get(), vec!["consumer read: open", "provider closed"]);
}

async fn same_name_services_in_separate_realms(root_shutdown: bool) {
    let root = Context::new();
    let gate = Gate::new();
    let (first, second) = (RealmId::new(), RealmId::new());
    let (closed, read) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (c, r) = (closed.clone(), read.clone());
    let provider = started(
        &root,
        &PluginDef::new(move |ctx, _| {
            let (closed, read) = (c.clone(), r.clone());
            async move {
                ctx.isolate_in("foo", first).provide_as("foo", read)?;
                ctx.isolate_in("foo", second).provide_as("foo", Arc::new(AtomicUsize::new(0)))?;
                ctx.on_dispose(move || {
                    closed.fetch_add(1, Ordering::SeqCst);
                })?;
                Ok(())
            }
        }),
    )
    .await;
    let draining = Arc::new(AtomicBool::new(false));
    let (d, g) = (draining.clone(), gate.clone());
    let consumer = started(
        &root.isolate_in("foo", first),
        &PluginDef::new(move |ctx, _| {
            let (draining, gate) = (d.clone(), g.clone());
            async move {
                let reader = ctx.clone();
                ctx.defer(
                    "cleanup",
                    Disposer::future(move || async move {
                        draining.store(true, Ordering::SeqCst);
                        gate.wait().await;
                        reader.get_as::<Arc<AtomicUsize>>("foo").unwrap().fetch_add(1, Ordering::SeqCst);
                    }),
                )?;
                Ok(())
            }
        })
        .inject(["foo"]),
    )
    .await;
    let task = if root_shutdown { root.fiber().dispose() } else { provider.dispose() };
    settle().await;
    let closed_during_drain = closed.load(Ordering::SeqCst);
    let notified = draining.load(Ordering::SeqCst);
    gate.open();
    consumer.dispose().await.unwrap();
    task.await.unwrap();
    assert_eq!(closed_during_drain, 0);
    assert!(notified);
    assert_eq!(read.load(Ordering::SeqCst), 1);
    assert_eq!(closed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn tracks_same_name_services_in_separate_isolation_realms() {
    same_name_services_in_separate_realms(false).await;
    same_name_services_in_separate_realms(true).await;
}

#[tokio::test]
async fn withdraws_only_the_old_binding_without_waiting_for_the_next_load() {
    let root = Context::new();
    let (old_cleanup, new_setup) = (Gate::new(), Gate::new());
    let versions = Seq::new();
    let provider = started(&root, &noop()).await;
    let withdraw = provider.context().provide_as("foo", 1i32).unwrap();
    let (v, o, n) = (versions.clone(), old_cleanup.clone(), new_setup.clone());
    let consumer = started(
        &root,
        &PluginDef::new(move |ctx, _| {
            let (versions, old_cleanup, new_setup) = (v.clone(), o.clone(), n.clone());
            async move {
                let version = *ctx.get_as::<i32>("foo")?;
                versions.push(version);
                if version == 2 {
                    new_setup.wait().await;
                }
                let reader = ctx.clone();
                ctx.defer(
                    "cleanup",
                    Disposer::future(move || async move {
                        old_cleanup.wait().await;
                        versions.push(*reader.get_as::<i32>("foo").unwrap());
                    }),
                )?;
                Ok(())
            }
        })
        .inject(["foo"]),
    )
    .await;
    let withdrawal = withdraw.dispose();
    let withdrawn = Seq::new();
    let w = withdrawn.clone();
    let task = tokio::spawn(async move {
        withdrawal.await.unwrap();
        w.push(true);
    });
    provider.context().provide_as("foo", 2i32).unwrap();
    old_cleanup.open();
    settle().await;
    let withdrawn_before_setup = withdrawn.len() == 1;
    let current = provider.context().live_as::<i32>("foo").map(|v| *v);
    new_setup.open();
    task.await.unwrap();
    consumer.settled().await.unwrap();
    assert!(withdrawn_before_setup);
    assert_eq!(current, Some(2));
    assert_eq!(versions.get(), vec![1, 1, 2]);
}

async fn current_and_withdrawn_bindings(withdrawn_first: bool) {
    let fx = Fixture::new();
    let current = Gate::new();
    let provider = started(&fx.root, &fx.provider()).await;
    let withdraw = provider.context().provide_as("bar", ()).unwrap();
    let c = current.clone();
    started(
        &fx.root,
        &PluginDef::new(move |ctx, _| {
            let current = c.clone();
            async move {
                let reader = ctx.clone();
                ctx.defer(
                    "cleanup",
                    Disposer::future(move || async move {
                        current.wait().await;
                        reader.get_as::<Foo>("foo").unwrap().read();
                    }),
                )?;
                Ok(())
            }
        })
        .inject(["foo"]),
    )
    .await;
    let (g, t) = (fx.gate.clone(), fx.timeline.clone());
    started(
        &fx.root,
        &PluginDef::new(move |ctx, _| {
            let (gate, timeline) = (g.clone(), t.clone());
            async move {
                ctx.defer(
                    "cleanup",
                    Disposer::future(move || async move {
                        gate.wait().await;
                        timeline.push("consumer finished".into());
                    }),
                )?;
                Ok(())
            }
        })
        .inject(["bar"]),
    )
    .await;
    let withdrawal = withdraw.dispose();
    provider.context().provide_as("bar", ()).unwrap();
    let task = provider.dispose();
    settle().await;
    let closed_during_drain = fx.is_closed();
    let (first, second) = if withdrawn_first { (&fx.gate, &current) } else { (&current, &fx.gate) };
    first.open();
    settle().await;
    let closed_after_first = fx.is_closed();
    second.open();
    let (a, b) = tokio::join!(withdrawal, task);
    a.unwrap();
    b.unwrap();
    assert!(!closed_during_drain);
    assert!(!closed_after_first);
    let expected = if withdrawn_first {
        vec!["consumer finished", "consumer read: open", "provider closed"]
    } else {
        vec!["consumer read: open", "consumer finished", "provider closed"]
    };
    assert_eq!(fx.timeline.get(), expected);
}

#[tokio::test]
async fn waits_for_current_and_withdrawn_bindings_during_provider_disposal() {
    current_and_withdrawn_bindings(false).await;
    current_and_withdrawn_bindings(true).await;
}

#[tokio::test]
async fn replaces_a_binding_from_the_same_provider_during_consumer_initialization() {
    let root = Context::new();
    let setup = Gate::new();
    let versions = Seq::new();
    let withdraw = root.provide_as("foo", 1i32).unwrap();
    let (v, s) = (versions.clone(), setup.clone());
    let consumer = start(
        &root,
        &PluginDef::new(move |ctx, _| {
            let (versions, setup) = (v.clone(), s.clone());
            async move {
                let version = *ctx.get_as::<i32>("foo")?;
                versions.push(version);
                setup.wait().await;
                ctx.on_dispose(move || versions.push(-version))?;
                Ok(())
            }
        })
        .inject(["foo"]),
    );
    settle().await;
    let withdrawal = withdraw.dispose();
    let withdrawn = Seq::new();
    let w = withdrawn.clone();
    let task = tokio::spawn(async move {
        withdrawal.await.unwrap();
        w.push(true);
    });
    root.provide_as("foo", 2i32).unwrap();
    setup.open();
    consumer.settled().await.unwrap();
    settle().await;
    let withdrawn_after_setup = withdrawn.len() == 1;
    let current = *consumer.context().get_as::<i32>("foo").unwrap();
    consumer.dispose().await.unwrap();
    task.await.unwrap();
    assert!(withdrawn_after_setup);
    assert_eq!(current, 2);
    assert_eq!(versions.get(), vec![1, -1, 2, -2]);
}
