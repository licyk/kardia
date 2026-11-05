//! Port of `packages/core/tests/associate.spec.ts`. An associated service
//! (`foo.bar`) is an ordinary key (<https://licyk.github.io/kardia/docs/development-design#mixins>); mixins and accessors are not ported.

mod common;

use common::Seq;
use kardia_core::{AccessError, Context, Fiber, PluginDef, Service};
use serde_json::Value;

struct Foo {
    qux: i32,
}

impl Service for Foo {
    const NAME: &'static str = "foo";
}

struct FooBar;

impl Service for FooBar {
    const NAME: &'static str = "foo.bar";
}

impl FooBar {
    fn hello(&self) -> &'static str {
        "bar"
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

async fn start(ctx: &Context, def: PluginDef) -> Fiber {
    let fiber = ctx.plugin_def(&def, Value::Null).unwrap();
    fiber.settled().await.unwrap();
    fiber
}

#[tokio::test]
async fn service_injection() {
    let root = Context::new();
    start(&root, provider(|_| Foo { qux: 1 })).await;
    let fiber = start(&root, provider(|_| FooBar)).await;
    assert_eq!(root.get::<Foo>().unwrap().qux, 1);
    assert_eq!(root.get::<FooBar>().unwrap().hello(), "bar");
    fiber.dispose().await.unwrap();
    assert_eq!(root.get::<FooBar>().err(), Some(AccessError::Inactive { key: "foo.bar".into() }));
    assert_eq!(root.get::<Foo>().unwrap().qux, 1);
}

struct Baz {
    ctx: Context,
}

impl Service for Baz {
    const NAME: &'static str = "baz";
}

struct Qux {
    ctx: Context,
}

impl Service for Qux {
    const NAME: &'static str = "qux";
}

fn probe(ctx: &Context) -> Result<&'static str, AccessError> {
    Ok(ctx.get::<FooBar>()?.hello())
}

#[tokio::test]
async fn associated_access_follows_the_service_fiber() {
    let root = Context::new();
    start(&root, provider(|_| Foo { qux: 1 })).await;
    start(&root, provider(|_| FooBar)).await;
    start(&root, provider(|ctx| Baz { ctx: ctx.clone() }).inject(["foo", "foo.bar"])).await;
    // injects `foo` but not `foo.bar`
    start(&root, provider(|ctx| Qux { ctx: ctx.clone() }).inject(["foo"])).await;

    // `foo.bar` is governed by the fiber of the service that runs the access,
    // not by whoever called the service
    let undeclared = Err(AccessError::Undeclared { key: "foo.bar".into() });
    assert_eq!(probe(&root.get::<Baz>().unwrap().ctx), Ok("bar"));
    assert_eq!(probe(&root.get::<Qux>().unwrap().ctx), undeclared);
    let results = Seq::new();
    let r = results.clone();
    root.inject(["baz", "qux"], move |ctx| {
        r.push(probe(&ctx.get::<Baz>().unwrap().ctx));
        r.push(probe(&ctx.get::<Qux>().unwrap().ctx));
        async { Ok(()) }
    })
    .unwrap()
    .settled()
    .await
    .unwrap();
    assert_eq!(results.get(), vec![Ok("bar"), undeclared]);
}
