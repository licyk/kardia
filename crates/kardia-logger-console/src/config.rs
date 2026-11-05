//! The configuration of the console exporter.

use std::collections::HashMap;

use kardia_core::Level;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    #[default]
    Left,
    Right,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct LabelStyle {
    pub width: usize,
    pub margin: Option<usize>,
    pub align: Align,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum Colors {
    /// `false` turns colours off; `true` turns them on at the detected level
    /// (at least basic colours, also when output is not a terminal).
    Enabled(bool),
    Level(u8),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ConsoleConfig {
    /// Colour support level (0–3); detected from the terminal when absent.
    pub colors: Option<Colors>,
    pub max_length: usize,
    /// Most verbose level per logger name; `default` applies to the rest.
    pub levels: HashMap<String, Level>,
    pub show_diff: bool,
    /// A time template (`yyyy-MM-dd hh:mm:ss `); empty hides the time.
    pub show_time: String,
    pub label: LabelStyle,
}

impl Default for ConsoleConfig {
    fn default() -> Self {
        Self {
            colors: None,
            max_length: 10240,
            levels: HashMap::new(),
            show_diff: false,
            show_time: "yyyy-MM-dd hh:mm:ss ".into(),
            label: LabelStyle::default(),
        }
    }
}
