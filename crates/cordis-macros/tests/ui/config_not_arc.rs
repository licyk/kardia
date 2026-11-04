use cordis_core::{Context, Result};
use cordis_macros::plugin;

#[plugin]
async fn greeter(_ctx: Context, _config: String) -> Result<()> {
    Ok(())
}

fn main() {}
