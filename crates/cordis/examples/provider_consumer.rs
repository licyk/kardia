//! A provider / consumer pair: the consumer waits for the provider, follows
//! its reconfiguration, and survives a hot replacement of its implementation.

use std::sync::Arc;
use std::time::Duration;

use cordis::logger_console::{ConsoleConfig, ConsoleExporter};
use cordis::prelude::*;

#[derive(Service)]
#[service(crate = ::cordis)]
struct Database {
    url: String,
    version: u32,
}

#[derive(Deserialize)]
struct DbConfig {
    url: String,
}

/// Opens a "connection", provides it, and closes it when reverted.
fn db_plugin(version: u32) -> PluginDef {
    PluginDef::typed(move |ctx: Context, config: Arc<DbConfig>| async move {
        let logger = ctx.logger();
        logger.info(format_args!("v{version}: connecting to {}", config.url));
        let url = config.url.clone();
        ctx.on_dispose(move || logger.info(format_args!("v{version}: closing {url}")))?;
        ctx.checkpoint()?;
        ctx.provide(Database { url: config.url.clone(), version })?;
        Ok(())
    })
    .name("db")
    .provides([Database::NAME])
}

/// Declares `database` and `timer`; reaching any other service through
/// `ctx.get` would not compile.
#[plugin(crate = ::cordis)]
async fn greeter(ctx: Injected<(Database, Timer)>) -> Result<()> {
    let (db, timer) = ctx.deps()?;
    ctx.logger().info(format_args!("using {} (v{})", db.url, db.version));
    let logger = ctx.logger();
    timer.interval(Duration::from_millis(40), move || logger.debug("tick"))?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let root = Context::new();
    let config = ConsoleConfig { show_time: String::new(), ..Default::default() };
    root.add_exporter(Arc::new(ConsoleExporter::new(config)))?;
    root.plugin::<TimerPlugin>(())?;

    let greeter = root.plugin::<Greeter>(())?;
    greeter.settled().await?;
    println!("greeter without a database: {:?}", greeter.state());

    let v1 = db_plugin(1);
    let db = root.plugin_def(&v1, json!({ "url": "postgres://primary" }))?;
    db.settled().await?;
    greeter.settled().await?;
    println!("greeter with a database: {:?}", greeter.state());

    // reconfiguration: the greeter unloads first, the database reloads, the greeter follows
    db.update(json!({ "url": "postgres://replica" }))?.await?;
    greeter.settled().await?;

    // hot replacement: the same fiber now runs the v2 implementation
    for fiber in root.registry().replace(&v1, &db_plugin(2)) {
        fiber.settled().await?;
    }
    greeter.settled().await?;

    db.dispose().await?;
    println!("greeter after the database is gone: {:?}", greeter.state());
    root.fiber().dispose().await?;
    Ok(())
}
