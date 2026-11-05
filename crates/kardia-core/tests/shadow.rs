//! Port of `packages/core/tests/shadow.spec.ts`, re-expressed on `Svc`: a
//! service resolves its dependencies through the context it stored when it
//! was constructed (its def site), while `Svc::caller` is the context that
//! accessed it (its use site). See
//! <https://licyk.github.io/kardia/docs/development-design#caller-attribution>.

mod common;

use common::Seq;
use kardia_core::{AccessError, Context, Fiber, Level, PluginDef, RealmId, Service, Svc};
use serde_json::Value;

struct Inner {
    ctx: Context,
}

impl Service for Inner {
    const NAME: &'static str = "inner";
}

impl Inner {
    fn hello(&self) -> &'static str {
        "hello"
    }
}

struct Outer {
    ctx: Context,
}

impl Service for Outer {
    const NAME: &'static str = "outer";
}

impl Outer {
    fn tag(&self) -> &'static str {
        "outer"
    }

    fn create(&self) -> Holder {
        Holder { ctx: self.ctx.clone() }
    }
}

/// A plain value handed out by a service, capturing its def-site context.
struct Holder {
    ctx: Context,
}

impl Holder {
    fn probe(&self) -> [&'static str; 2] {
        [self.ctx.get::<Outer>().unwrap().tag(), self.ctx.get::<Inner>().unwrap().hello()]
    }
}

fn provider<S: Service>(make: impl Fn(&Context) -> S + Send + Sync + 'static) -> PluginDef {
    PluginDef::new(move |ctx, _| {
        let service = make(&ctx);
        async move {
            ctx.provide(service)?;
            Ok(())
        }
    })
}

fn inner() -> PluginDef {
    provider(|ctx| Inner { ctx: ctx.clone() })
}

fn outer() -> PluginDef {
    provider(|ctx| Outer { ctx: ctx.clone() })
}

async fn start(ctx: &Context, def: PluginDef) -> Fiber {
    let fiber = ctx.plugin_def(&def, Value::Null).unwrap();
    fiber.settled().await.unwrap();
    fiber
}

/// Runs `f` once in a consumer of `outer`.
async fn consume<T: Clone + Send + 'static>(root: &Context, f: impl Fn(&Context) -> T + Send + Sync + 'static) -> T {
    let result = Seq::new();
    let r = result.clone();
    root.inject(["outer"], move |ctx| {
        r.push(f(&ctx));
        async { Ok(()) }
    })
    .unwrap()
    .settled()
    .await
    .unwrap();
    result.get().remove(0)
}

/// The caller of the inner access, the def site of `inner`, and that of `outer`.
fn inspect(outer: &Svc<Outer>) -> [Context; 3] {
    let inner = outer.ctx.get::<Inner>().unwrap();
    [inner.caller().clone(), inner.ctx.clone(), outer.ctx.clone()]
}

fn same(left: &[Context; 3], right: &[Context; 3]) -> bool {
    left.iter().zip(right).all(|(a, b)| a.ptr_eq(b))
}

#[tokio::test]
async fn keeps_caller_metadata_separate_from_the_service_shadow() {
    let root = Context::new();
    let inner_origin = start(&root, inner()).await.context().clone();
    let outer_origin = start(&root, outer().inject(["inner"])).await.context().clone();
    let expected = [outer_origin.clone(), inner_origin, outer_origin];

    let (result, use_site) = consume(&root, |ctx| {
        let outer = ctx.get::<Outer>().unwrap();
        (inspect(&outer), outer.caller().ptr_eq(ctx))
    })
    .await;
    assert!(same(&result, &expected));
    assert!(use_site);

    // accessing the service from root does not change its def sites
    let direct = root.get::<Outer>().unwrap();
    assert!(same(&inspect(&direct), &expected));
    assert!(direct.caller().ptr_eq(&root));
}

#[tokio::test]
async fn applies_the_service_inject_when_entered_from_root() {
    let root = Context::new();
    start(&root, inner()).await;
    // deliberately does not inject `inner`
    start(&root, outer()).await;

    let probe = |outer: Svc<Outer>| outer.ctx.get::<Inner>().map(|inner| inner.hello());
    let undeclared = Err(AccessError::Undeclared { key: "inner".into() });
    assert_eq!(probe(root.get::<Outer>().unwrap()), undeclared);
    assert_eq!(consume(&root, move |ctx| probe(ctx.get::<Outer>().unwrap())).await, undeclared);
}

#[tokio::test]
async fn resolves_dependencies_through_a_captured_shadow() {
    let root = Context::new();
    start(&root, inner()).await;
    start(&root, outer().inject(["inner"])).await;

    assert_eq!(root.get::<Outer>().unwrap().create().probe(), ["outer", "hello"]);
    let probed = consume(&root, |ctx| ctx.get::<Outer>().unwrap().create().probe()).await;
    assert_eq!(probed, ["outer", "hello"]);
}

struct Probe {
    ctx: Context,
    tag: &'static str,
}

impl Service for Probe {
    const NAME: &'static str = "foo";
}

impl Probe {
    fn inspect(&self) -> &'static str {
        self.ctx.get::<Probe>().unwrap().tag
    }
}

struct Relay {
    ctx: Context,
}

impl Service for Relay {
    const NAME: &'static str = "relay";
}

#[tokio::test]
async fn resolves_same_name_own_provisions_at_their_definition_sites() {
    let root = Context::new();
    let (first, second) = (RealmId::new(), RealmId::new());
    let def = PluginDef::new(move |ctx, _| async move {
        let (a, b) = (ctx.isolate_in("foo", first), ctx.isolate_in("foo", second));
        a.provide(Probe { ctx: a.clone(), tag: "first" })?;
        b.provide(Probe { ctx: b.clone(), tag: "second" })?;
        Ok(())
    });
    start(&root, def).await;
    start(&root.isolate_in("foo", first), provider(|ctx| Relay { ctx: ctx.clone() }).inject(["foo"])).await;

    // The use site changes, while the def sites of the relay and of the probe
    // it injects stay in the first realm.
    let use_site = root.isolate_in("foo", second);
    let relay = use_site.get::<Relay>().unwrap();
    assert_eq!(relay.ctx.get::<Probe>().unwrap().inspect(), "first");
    assert_eq!(relay.caller().realm("foo"), Some(second));
    assert_eq!(use_site.get::<Probe>().unwrap().inspect(), "second");
    root.fiber().dispose().await.unwrap();
}

/// Provided outside the fiber system, so it can never declare `inject`.
struct Rooted {
    ctx: Context,
}

impl Service for Rooted {
    const NAME: &'static str = "rooted";
}

#[tokio::test]
async fn keeps_unchecked_access_for_services_provided_on_the_root_ctx() {
    let root = Context::new();
    start(&root, inner()).await;
    root.provide(Rooted { ctx: root.clone() }).unwrap();
    assert_eq!(root.get::<Rooted>().unwrap().ctx.get::<Inner>().unwrap().hello(), "hello");
}

struct Plain;

impl Service for Plain {
    const NAME: &'static str = "probe";
}

#[tokio::test]
async fn exposes_the_caller_without_preserving_shadow_for_no_shadow_services() {
    let root = Context::new();
    root.provide(Plain).unwrap();
    let outer_origin = start(&root, outer().inject(["probe"])).await.context().clone();
    let caller = consume(&root, |ctx| ctx.get::<Outer>().unwrap().ctx.get::<Plain>().unwrap().caller().clone()).await;
    assert!(caller.ptr_eq(&outer_origin));
}

struct Loader;

impl Service for Loader {
    const NAME: &'static str = "loader";
}

struct Server;

impl Service for Server {
    const NAME: &'static str = "server";
}

trait LoaderExt {
    fn load(&self, def: &PluginDef) -> kardia_core::Result<Fiber>;
}

impl LoaderExt for Svc<Loader> {
    /// Instantiating a plugin is an effect, so it belongs to the use site.
    fn load(&self, def: &PluginDef) -> kardia_core::Result<Fiber> {
        self.caller().plugin_def(def, Value::Null)
    }
}

#[tokio::test]
async fn strips_service_shadow_before_creating_plugins() {
    let root = Context::new();
    start(&root, provider(|_| Loader)).await;
    let server = provider(|_| Server);
    let injected = Seq::new();
    let i = injected.clone();
    let consumer = PluginDef::new(move |ctx, _| {
        let injected = i.clone();
        async move {
            let fiber = ctx.inject(["server"], move |ctx| {
                injected.push(ctx.get::<Server>().is_ok());
                async { Ok(()) }
            })?;
            fiber.settled().await
        }
    });
    let parents = Seq::new();
    let p = parents.clone();
    root.inject(["loader"], move |ctx| {
        let (server, consumer, parents) = (server.clone(), consumer.clone(), p.clone());
        async move {
            let loader = ctx.get::<Loader>()?;
            let server = loader.load(&server)?;
            server.settled().await?;
            let consumer = loader.load(&consumer)?;
            consumer.settled().await?;
            parents.push([server.parent(), consumer.parent()] == [Some(ctx.fiber()), Some(ctx.fiber())]);
            Ok(())
        }
    })
    .unwrap()
    .settled()
    .await
    .unwrap();
    assert_eq!(injected.get(), vec![true]);
    assert_eq!(parents.get(), vec![true]);
    assert!(root.log_buffer().iter().all(|message| message.level != Level::Error));
}
