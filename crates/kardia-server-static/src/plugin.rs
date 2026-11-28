use crate::{StaticConfig, serve::Files};
use kardia_core::{Context, Inject, Plugin, Result};
use kardia_server::{Server, ServerExt};
use std::sync::Arc;

pub struct StaticPlugin;
impl Plugin for StaticPlugin {
    const NAME: &'static str = "server:static";
    type Config = StaticConfig;
    fn inject() -> Inject {
        Inject::new().with("std:server").with("std:fs")
    }
    async fn apply(ctx: Context, config: Arc<StaticConfig>) -> Result<()> {
        let files = Arc::new(Files::new(&ctx, config)?);
        ctx.get::<Server>()?.get("{/*path}", move |req, _, next| {
            let files = files.clone();
            async move { files.serve(req, next).await }
        })?;
        Ok(())
    }
}
