//! Port of `intercept.spec.ts`, `logger.spec.ts`, the caller-attribution
//! parts of `service.spec.ts` and `shadow.spec.ts` (re-expressed on `Svc`).

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use common::{Seq, settle};
use cordis_core::{
    Context, EffectHandle, Exporter, Fiber, Inject, Level, Message, Plugin, PluginDef, RealmId, Service, Svc,
};
use serde_json::{Value, json};

struct Foo;

impl Service for Foo {
    const NAME: &'static str = "foo";
}

trait FooExt {
    fn config(&self) -> Value;
}

impl FooExt for Svc<Foo> {
    fn config(&self) -> Value {
        self.meta()
    }
}

struct FooPlugin;

impl Plugin for FooPlugin {
    const NAME: &'static str = "foo";
    type Config = ();

    async fn apply(ctx: Context, _: Arc<()>) -> cordis_core::Result<()> {
        ctx.provide(Foo)?;
        Ok(())
    }
}

async fn start<P: Plugin<Config = ()>>(ctx: &Context) -> Fiber {
    let fiber = ctx.plugin_value::<P>(Value::Null).unwrap();
    fiber.settled().await.unwrap();
    fiber
}

async fn inject_run(ctx: &Context, inject: Inject, f: impl Fn(Context) + Send + Sync + 'static) {
    let f = Arc::new(f);
    let def = PluginDef::new(move |ctx, _| {
        f(ctx);
        async { Ok(()) }
    })
    .inject(inject);
    ctx.plugin_def(&def, Value::Null).unwrap().settled().await.unwrap();
}

#[tokio::test]
async fn merges_context_intercepts_from_outer_to_inner() {
    let root = Context::new();
    start::<FooPlugin>(&root).await;
    assert_eq!(root.get::<Foo>().unwrap().config(), json!({}));
    let ctx = root.intercept("foo", json!({ "a": 1, "b": 1 })).intercept("foo", json!({ "b": 2 }));
    assert_eq!(ctx.get::<Foo>().unwrap().config(), json!({ "a": 1, "b": 2 }));
}

#[tokio::test]
async fn merges_injected_config_under_context_intercepts() {
    let root = Context::new();
    start::<FooPlugin>(&root).await;
    let config = Seq::new();
    let c = config.clone();
    inject_run(
        &root.intercept("foo", json!({ "b": 2 })),
        Inject::new().with_meta("foo", json!({ "a": 1, "b": 1 })),
        move |ctx| c.push(ctx.intercept("foo", json!({ "c": 3 })).get::<Foo>().unwrap().config()),
    )
    .await;
    assert_eq!(config.get(), vec![json!({ "a": 1, "b": 2, "c": 3 })]);
}

#[tokio::test]
async fn takes_injected_config_from_the_declaring_fiber() {
    let root = Context::new();
    start::<FooPlugin>(&root).await;
    let configs = Seq::new();
    let c = configs.clone();
    let def = PluginDef::new(move |ctx, _| {
        let configs = c.clone();
        async move {
            // no declaration of its own, so the access is admitted by the outer fiber
            let c1 = configs.clone();
            inject_run(&ctx, Inject::new(), move |ctx| c1.push(ctx.get::<Foo>().unwrap().config())).await;
            // the nearest declaration wins, and the outer one is not merged in
            let c2 = configs.clone();
            inject_run(&ctx, Inject::new().with_meta("foo", json!({ "b": 2 })), move |ctx| {
                c2.push(ctx.get::<Foo>().unwrap().config())
            })
            .await;
            Ok(())
        }
    })
    .inject(Inject::new().with_meta("foo", json!({ "a": 1 })));
    root.plugin_def(&def, Value::Null).unwrap().settled().await.unwrap();
    assert_eq!(configs.get(), vec![json!({ "a": 1 }), json!({ "b": 2 })]);
}

#[tokio::test]
async fn stops_looking_for_injected_config_at_an_isolation_boundary() {
    let root = Context::new();
    start::<FooPlugin>(&root).await;
    let label = RealmId::new();
    start::<FooPlugin>(&root.isolate_in("foo", label)).await;
    let config = Seq::new();
    let c = config.clone();
    let def = PluginDef::new(move |ctx, _| {
        let config = c.clone();
        async move {
            inject_run(&ctx, Inject::new(), move |ctx| {
                config.push(ctx.isolate_in("foo", label).live::<Foo>().unwrap().config())
            })
            .await;
            Ok(())
        }
    })
    .inject(Inject::new().with_meta("foo", json!({ "a": 1 })));
    root.plugin_def(&def, Value::Null).unwrap().settled().await.unwrap();
    assert_eq!(config.get(), vec![json!({})]);
}

#[tokio::test]
async fn takes_no_injected_config_for_own_provisions() {
    let root = Context::new();
    start::<FooPlugin>(&root).await;
    let configs = Seq::new();
    let c = configs.clone();
    inject_run(&root, Inject::new().with_meta("foo", json!({ "a": 1 })), move |ctx| {
        let isolated = ctx.isolate("foo");
        isolated.provide(Foo).unwrap();
        // the injected config belongs to the dependency, not to the service the fiber provides itself
        c.push(ctx.get::<Foo>().unwrap().config());
        c.push(isolated.get::<Foo>().unwrap().config());
        c.push(isolated.intercept("foo", json!({ "b": 2 })).get::<Foo>().unwrap().config());
    })
    .await;
    assert_eq!(configs.get(), vec![json!({ "a": 1 }), json!({}), json!({ "b": 2 })]);
}

struct Tagged;

impl Service for Tagged {
    const NAME: &'static str = "tagged";

    fn merge_intercept(values: Vec<Value>) -> Value {
        let tags: Vec<Value> =
            values.iter().flat_map(|value| value["tags"].as_array().cloned().unwrap_or_default()).collect();
        json!({ "tags": tags })
    }
}

#[tokio::test]
async fn custom_merge() {
    let root = Context::new();
    root.provide(Tagged).unwrap();
    let config = Seq::new();
    let c = config.clone();
    inject_run(
        &root.intercept("tagged", json!({ "tags": ["outer"] })),
        Inject::new().with_meta("tagged", json!({ "tags": ["inject"] })),
        move |ctx| c.push(ctx.intercept("tagged", json!({ "tags": ["inner"] })).get::<Tagged>().unwrap().meta()),
    )
    .await;
    assert_eq!(config.get(), vec![json!({ "tags": ["inject", "outer", "inner"] })]);
}

/// Accesses `foo` through its def-site context, admitted by its own `inject`.
struct Bar {
    ctx: Context,
}

impl Service for Bar {
    const NAME: &'static str = "bar";
}

#[tokio::test]
async fn takes_injected_config_from_the_def_site() {
    let root = Context::new();
    start::<FooPlugin>(&root).await;
    let def = PluginDef::new(|ctx, _| async move {
        ctx.provide(Bar { ctx: ctx.clone() })?;
        Ok(())
    })
    .inject(Inject::new().with_meta("foo", json!({ "a": 2 })));
    root.plugin_def(&def, Value::Null).unwrap().settled().await.unwrap();
    let bar = root.intercept("foo", json!({ "b": 1 })).get::<Bar>().unwrap();
    // Unlike the TS shadow, the caller's interception of `foo` stays with the
    // caller: the service's own access only sees its def site.
    assert_eq!(bar.ctx.get::<Foo>().unwrap().config(), json!({ "a": 2 }));
    assert_eq!(bar.caller().intercepts("foo"), vec![json!({ "b": 1 })]);
}

// --- caller attribution (service.spec.ts "traceable effect", shadow.spec.ts) ---

struct Counter {
    value: AtomicI64,
}

impl Service for Counter {
    const NAME: &'static str = "counter";
}

trait CounterExt {
    fn increase(&self) -> cordis_core::Result<EffectHandle>;
}

impl CounterExt for Svc<Counter> {
    /// The increment is an effect of the caller, reverted with the caller.
    fn increase(&self) -> cordis_core::Result<EffectHandle> {
        let counter = self.arc().clone();
        self.caller().effect_sync("counter.increase()", |scope| {
            counter.value.fetch_add(1, Ordering::SeqCst);
            scope.defer_fn(move || {
                counter.value.fetch_sub(1, Ordering::SeqCst);
            });
            Ok(())
        })
    }
}

#[tokio::test]
async fn caller_effects_land_in_the_calling_fiber() {
    let root = Context::new();
    root.provide(Counter { value: AtomicI64::new(0) }).unwrap();
    root.get::<Counter>().unwrap().increase().unwrap();
    assert_eq!(root.get::<Counter>().unwrap().value.load(Ordering::SeqCst), 1);

    let def = PluginDef::new(|ctx, _| async move {
        ctx.get::<Counter>()?.increase()?;
        assert_eq!(ctx.get::<Counter>()?.value.load(Ordering::SeqCst), 2);
        Ok(())
    })
    .inject(Inject::of::<(Counter,)>());
    let fiber = root.plugin_def(&def, Value::Null).unwrap();
    fiber.settled().await.unwrap();
    fiber.dispose().await.unwrap();
    assert_eq!(root.get::<Counter>().unwrap().value.load(Ordering::SeqCst), 1);
    root.get::<Counter>().unwrap().increase().unwrap();
    assert_eq!(root.get::<Counter>().unwrap().value.load(Ordering::SeqCst), 2);
}

/// A service forwarding to `counter` through its def-site context.
struct Traced {
    ctx: Context,
}

impl Service for Traced {
    const NAME: &'static str = "traced";
}

impl Traced {
    fn value(&self) -> i64 {
        self.ctx.get::<Counter>().unwrap().value.load(Ordering::SeqCst)
    }

    fn increase(&self) -> cordis_core::Result<EffectHandle> {
        self.ctx.get::<Counter>()?.increase()
    }
}

async fn traceable_effect(inject: bool) {
    let root = Context::new();
    root.provide(Counter { value: AtomicI64::new(0) }).unwrap();
    let def = PluginDef::new(|ctx, _| async move {
        ctx.provide(Traced { ctx: ctx.clone() })?;
        Ok(())
    });
    let def = if inject { def.inject(["counter"]) } else { def };
    root.plugin_def(&def, Value::Null).unwrap().settled().await.unwrap();
    root.get::<Traced>().unwrap().increase().unwrap();
    assert_eq!(root.get::<Traced>().unwrap().value(), 1);

    let values = Seq::new();
    let (v, outer) = (values.clone(), root.clone());
    let fiber = root
        .inject(["traced"], move |ctx| {
            outer.get::<Traced>().unwrap().increase().unwrap();
            v.push(ctx.get::<Traced>().unwrap().value());
            async { Ok(()) }
        })
        .unwrap();
    fiber.settled().await.unwrap();
    fiber.dispose().await.unwrap();
    root.get::<Traced>().unwrap().increase().unwrap();
    assert_eq!(values.get(), vec![2]);
    assert_eq!(root.get::<Traced>().unwrap().value(), 3);
    assert!(root.log_buffer().is_empty());
}

#[tokio::test]
async fn traceable_effect_with_inject() {
    traceable_effect(true).await;
}

#[tokio::test]
async fn traceable_effect_without_inject() {
    traceable_effect(false).await;
}

/// A service that resolves its own dependency through its def-site context.
struct Outer {
    ctx: Context,
}

impl Service for Outer {
    const NAME: &'static str = "outer";
}

struct Inner;

impl Service for Inner {
    const NAME: &'static str = "inner";
}

#[tokio::test]
async fn services_resolve_dependencies_at_their_def_site() {
    let root = Context::new();
    root.provide(Inner).unwrap();
    let def = PluginDef::new(|ctx, _| async move {
        ctx.provide(Outer { ctx: ctx.clone() })?;
        Ok(())
    })
    .name("outer")
    .inject(Inject::of::<(Inner,)>());
    let provider = root.plugin_def(&def, Value::Null).unwrap();
    provider.settled().await.unwrap();

    let callers = Seq::new();
    let c = callers.clone();
    inject_run(&root, Inject::of::<(Outer,)>(), move |ctx| {
        let (outer,) = ctx.deps::<(Outer,)>().unwrap();
        // the use site is the consumer, the def site is the provider
        c.push(outer.caller().fiber() == ctx.fiber());
        c.push(outer.provider() == provider);
        let inner = outer.ctx.get::<Inner>().unwrap();
        c.push(inner.caller().fiber() == provider);
    })
    .await;
    assert_eq!(callers.get(), vec![true, true, true]);
}

// --- logger.spec.ts ---

#[derive(Clone, Default)]
struct Captured(Seq<Message>);

impl Exporter for Captured {
    fn export(&self, message: &Message) {
        self.0.push(message.clone());
    }

    fn level(&self, _name: &str) -> Option<Level> {
        Some(Level::Debug)
    }
}

fn setup() -> (Context, Seq<Message>) {
    let ctx = Context::new();
    let captured = Captured::default();
    ctx.add_exporter(Arc::new(captured.clone())).unwrap();
    (ctx, captured.0)
}

fn names(captured: &Seq<Message>) -> Vec<String> {
    captured.get().into_iter().map(|message| message.name).collect()
}

#[tokio::test]
async fn keeps_the_bounded_buffer_chronological() {
    let ctx = Context::new();
    ctx.set_log_buffer_size(2);
    for text in ["one", "two", "three"] {
        ctx.logger().info(text);
    }
    let texts = |ctx: &Context| ctx.log_buffer().into_iter().map(|m| m.text).collect::<Vec<_>>();
    assert_eq!(texts(&ctx), vec!["two", "three"]);
    ctx.set_log_buffer_size(1);
    ctx.logger().info("four");
    assert_eq!(texts(&ctx), vec!["four"]);
    ctx.set_log_buffer_size(0);
    ctx.logger().info("five");
    assert!(texts(&ctx).is_empty());
}

#[tokio::test]
async fn disposes_the_exporter_that_registered_the_disposer() {
    let ctx = Context::new();
    let (first, second) = (Captured::default(), Captured::default());
    let dispose_first = ctx.add_exporter(Arc::new(first.clone())).unwrap();
    let dispose_second = ctx.add_exporter(Arc::new(second.clone())).unwrap();
    dispose_first.dispose().await.unwrap();
    ctx.logger().info("test");
    assert_eq!((first.0.len(), second.0.len()), (0, 1));
    dispose_second.dispose().await.unwrap();
    ctx.logger().info("test");
    assert_eq!(second.0.len(), 1);
}

#[tokio::test]
async fn logger_names() {
    let (ctx, captured) = setup();
    ctx.logger().debug("hello");
    ctx.logger_named("custom").debug("hello");
    ctx.intercept("logger", json!({ "name": "intercepted" })).logger().debug("hello");
    assert_eq!(names(&captured), vec!["root", "custom", "intercepted"]);
}

#[tokio::test]
async fn intercepted_level_filters_messages() {
    let ctx = Context::new();
    let exporter = common::capture(&ctx, Level::Debug);
    ctx.logger().debug("hidden");
    let silent = Captured::default();
    struct Defer(Captured);
    impl Exporter for Defer {
        fn export(&self, message: &Message) {
            self.0.export(message);
        }
    }
    ctx.add_exporter(Arc::new(Defer(silent.clone()))).unwrap();
    ctx.logger().debug("default level");
    ctx.intercept("logger", json!({ "level": 3 })).logger().debug("verbose");
    assert_eq!(silent.0.get().into_iter().map(|m| m.text).collect::<Vec<_>>(), vec!["verbose"]);
    assert_eq!(exporter.len(), 3);
}

/// `foo:driver` logs on behalf of its callers.
struct Driver {
    ctx: Context,
}

impl Service for Driver {
    const NAME: &'static str = "foo";
}

trait DriverExt {
    fn action(&self);
}

impl DriverExt for Svc<Driver> {
    fn action(&self) {
        self.logger().debug("from action");
    }
}

#[tokio::test]
async fn uses_the_service_name_and_lets_the_caller_intercept_override_it() {
    let (ctx, captured) = setup();
    let def = PluginDef::new(|ctx, _| async move {
        ctx.provide(Driver { ctx: ctx.clone() })?;
        ctx.logger().debug("from init");
        Ok(())
    })
    .name("foo:driver");
    ctx.plugin_def(&def, Value::Null).unwrap().settled().await.unwrap();
    ctx.get::<Driver>().unwrap().action();
    ctx.intercept("logger", json!({ "name": "caller-override" })).get::<Driver>().unwrap().action();
    settle().await;
    assert_eq!(names(&captured), vec!["foo:driver", "foo:driver", "caller-override"]);
    let _ = &ctx.get::<Driver>().unwrap().ctx;
}

#[tokio::test]
async fn uses_the_innermost_service_name() {
    let (ctx, captured) = setup();
    struct Bar;
    impl Service for Bar {
        const NAME: &'static str = "bar";
    }
    ctx.plugin_def(
        &PluginDef::new(|ctx, _| async move {
            ctx.provide(Bar)?;
            Ok(())
        })
        .name("bar:driver"),
        Value::Null,
    )
    .unwrap()
    .settled()
    .await
    .unwrap();
    ctx.plugin_def(
        &PluginDef::new(|ctx, _| async move {
            ctx.provide(Driver { ctx: ctx.clone() })?;
            Ok(())
        })
        .name("foo:driver")
        .inject(["bar"]),
        Value::Null,
    )
    .unwrap()
    .settled()
    .await
    .unwrap();
    inject_run(&ctx, Inject::new().with("foo"), |ctx| {
        let foo = ctx.get::<Driver>().unwrap();
        foo.ctx.get::<Bar>().unwrap().logger().debug("from bar");
        foo.logger().debug("from foo");
    })
    .await;
    let logged: Vec<_> = captured.get().into_iter().map(|m| (m.name, m.text)).collect();
    assert_eq!(
        logged,
        vec![("bar:driver".to_string(), "from bar".to_string()), ("foo:driver".into(), "from foo".into())]
    );
}

#[tokio::test]
async fn flattens_an_aggregate_error_into_its_members() {
    let (ctx, captured) = setup();
    let empty = cordis_core::AggregateError { message: "empty aggregate".into(), errors: vec![] };
    ctx.logger().log_error_at(Level::Warn, &empty.into());
    let outer = cordis_core::AggregateError { message: "outer".into(), errors: vec![cordis_core::Error::msg("inner")] };
    ctx.logger().log_error_at(Level::Warn, &outer.into());
    let texts: Vec<_> = captured.get().into_iter().map(|m| m.text).collect();
    assert_eq!(texts, vec!["empty aggregate", "inner"]);
}
