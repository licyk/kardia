use kardia_core::{Context, Result};
use kardia_macros::plugin;

#[plugin]
async fn greeter(_ctx: Context, _config: String) -> Result<()> {
    Ok(())
}

fn main() {}
