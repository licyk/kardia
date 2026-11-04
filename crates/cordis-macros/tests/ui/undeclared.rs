use cordis_core::{Injected, Result};
use cordis_macros::{Service, plugin};

#[derive(Service)]
struct Database;

#[derive(Service)]
struct Timer;

#[plugin]
async fn greeter(ctx: Injected<(Database,)>) -> Result<()> {
    let _database = ctx.get::<Database, _>()?;
    // `Timer` is not declared
    let _timer = ctx.get::<Timer, _>()?;
    Ok(())
}

fn main() {}
