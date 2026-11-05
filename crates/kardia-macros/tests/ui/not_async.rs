use kardia_core::{Context, Result};
use kardia_macros::plugin;

#[plugin]
fn greeter(_ctx: Context) -> Result<()> {
    Ok(())
}

fn main() {}
