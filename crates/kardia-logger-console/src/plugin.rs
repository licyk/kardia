//! The plugin that registers the console exporter.

use std::sync::Arc;

use kardia_core::{Context, Plugin, Result};

use crate::config::ConsoleConfig;
use crate::exporter::ConsoleExporter;

/// Registers a [`ConsoleExporter`] for the lifetime of the plugin.
pub struct ConsolePlugin;

impl Plugin for ConsolePlugin {
    const NAME: &'static str = "logger-console";
    type Config = ConsoleConfig;

    async fn apply(ctx: Context, config: Arc<ConsoleConfig>) -> Result<()> {
        ctx.add_exporter(Arc::new(ConsoleExporter::new((*config).clone())))?;
        Ok(())
    }
}
