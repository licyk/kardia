use cordis_core::{Context, Result};
use cordis_macros::plugin;

#[plugin]
fn greeter(_ctx: Context) -> Result<()> {
    Ok(())
}

fn main() {}
