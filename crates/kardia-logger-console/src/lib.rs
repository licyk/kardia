//! Logger exporter that writes formatted, coloured messages to the terminal.

mod config;
mod exporter;
mod format;
mod plugin;

pub use config::{Align, Colors, ConsoleConfig, LabelStyle};
pub use exporter::ConsoleExporter;
pub use format::{color, format_duration, name_color, time_template};
pub use plugin::ConsolePlugin;
