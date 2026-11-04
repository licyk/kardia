use std::sync::Arc;

use cordis_core::{Context, FiberState, Injected, Plugin, Result, Service};
use cordis_macros::{Service, plugin};
use serde::{Deserialize, Serialize};

#[derive(Service)]
#[service(name = "database")]
struct Database {
    url: String,
}

#[derive(Service)]
struct TaskQueue;

#[derive(Serialize, Deserialize)]
struct DbConfig {
    url: String,
}

#[plugin(provide = ["database"])]
async fn db_plugin(ctx: Context, config: Arc<DbConfig>) -> Result<()> {
    ctx.provide(Database { url: config.url.clone() })?;
    Ok(())
}

#[plugin(name = "greeter")]
async fn greeter(ctx: Injected<(Database, TaskQueue)>) -> Result<()> {
    let database = ctx.get::<Database, _>()?;
    let (_, queue) = ctx.deps()?;
    ctx.logger().info(format_args!("{} {}", database.url, queue.key()));
    Ok(())
}

#[test]
fn compile_errors() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/*.rs");
}

#[test]
fn derives_names_and_declarations() {
    assert_eq!(Database::NAME, "database");
    assert_eq!(TaskQueue::NAME, "task-queue");
    assert_eq!(DbPlugin::NAME, "db-plugin");
    assert_eq!(DbPlugin::provide(), vec!["database".into()]);
    assert_eq!(Greeter::NAME, "greeter");
    let keys: Vec<_> = Greeter::inject().keys().map(ToString::to_string).collect();
    assert_eq!(keys, vec!["database", "task-queue"]);
}

#[tokio::test]
async fn generated_plugins_run() {
    let root = Context::new();
    root.provide(TaskQueue).unwrap();
    let greeter = root.plugin::<Greeter>(()).unwrap();
    greeter.settled().await.unwrap();
    assert_eq!(greeter.state(), FiberState::Pending);
    root.plugin::<DbPlugin>(DbConfig { url: "postgres://".into() }).unwrap().settled().await.unwrap();
    greeter.settled().await.unwrap();
    assert_eq!(greeter.state(), FiberState::Active);
    let logged: Vec<_> = root.log_buffer().into_iter().map(|m| m.text).collect();
    assert_eq!(logged, vec!["postgres:// task-queue"]);
}
