//! Logger exporter that writes formatted, coloured messages to the terminal.

use std::collections::HashMap;
use std::io::IsTerminal;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use cordis_core::{Context, Exporter, Level, Message, Plugin, Result};
use parking_lot::Mutex;
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
    Off(bool),
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

pub struct ConsoleExporter {
    colors: u8,
    config: ConsoleConfig,
    timestamp: Mutex<u64>,
}

impl ConsoleExporter {
    pub fn new(config: ConsoleConfig) -> Self {
        let colors = match config.colors {
            Some(Colors::Level(level)) => level,
            Some(Colors::Off(_)) => 0,
            None => detect_colors(),
        };
        Self { colors, config, timestamp: Mutex::new(now()) }
    }

    pub fn render(&self, message: &Message) -> String {
        let prefix = format!("[{}]", message.level.as_str()[..1].to_ascii_uppercase());
        let space = " ".repeat(self.config.label.margin.unwrap_or(1));
        let mut indent = 3 + space.len();
        let mut output = String::new();
        if !self.config.show_time.is_empty() {
            indent += self.config.show_time.chars().count();
            output += &color(self.colors, 8, &time_template(&self.config.show_time), "");
        }
        let code = name_color(&message.name, self.colors);
        let label = color(self.colors, code, &message.name, ";1");
        let pad = self.config.label.width.saturating_sub(message.name.chars().count());
        if self.config.label.align == Align::Right {
            output += &format!("{}{label}{space}{prefix}{space}", " ".repeat(pad));
            indent += self.config.label.width + space.len();
        } else {
            output += &format!("{prefix}{space}{label}{}{space}", " ".repeat(pad));
        }
        output +=
            &truncate_lines(&message.text, self.config.max_length).replace('\n', &format!("\n{}", " ".repeat(indent)));
        let mut timestamp = self.timestamp.lock();
        if self.config.show_diff {
            let diff = message.ts as i64 - *timestamp as i64;
            output += &color(self.colors, code, &format!(" +{}", format_duration(diff)), "");
        }
        *timestamp = message.ts;
        output
    }
}

impl Exporter for ConsoleExporter {
    fn export(&self, message: &Message) {
        println!("{}", self.render(message));
    }

    fn level(&self, name: &str) -> Option<Level> {
        self.config.levels.get(name).or_else(|| self.config.levels.get("default")).copied()
    }
}

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

const C16: [u8; 6] = [6, 2, 3, 4, 5, 1];
const C256: [u8; 75] = [
    20, 21, 26, 27, 32, 33, 38, 39, 40, 41, 42, 43, 44, 45, 56, 57, 62, 63, 68, 69, 74, 75, 76, 77, 78, 79, 80, 81, 92,
    93, 98, 99, 112, 113, 129, 134, 135, 148, 149, 160, 161, 162, 163, 164, 165, 166, 167, 168, 169, 170, 171, 172,
    173, 178, 179, 184, 185, 196, 197, 198, 199, 200, 201, 202, 203, 204, 205, 206, 207, 208, 209, 214, 215, 220, 221,
];

/// A stable colour for a logger name (the hash of `Logger.code`).
pub fn name_color(name: &str, colors: u8) -> u8 {
    let mut hash: i32 = 0;
    for unit in name.encode_utf16() {
        hash = (hash << 3).wrapping_sub(hash).wrapping_add(i32::from(unit)).wrapping_add(13);
    }
    let palette: &[u8] = match colors {
        0 => return 0,
        1 => &C16,
        _ => &C256,
    };
    palette[(hash.unsigned_abs() as usize) % palette.len()]
}

pub fn color(colors: u8, code: u8, value: &str, decoration: &str) -> String {
    if colors == 0 {
        return value.to_string();
    }
    let code = if code < 8 { code.to_string() } else { format!("8;5;{code}") };
    let decoration = if colors >= 2 { decoration } else { "" };
    format!("\u{1b}[3{code}{decoration}m{value}\u{1b}[0m")
}

fn truncate_lines(text: &str, max_length: usize) -> String {
    text.split('\n')
        .map(|line| {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if line.chars().count() > max_length {
                format!("{}...", line.chars().take(max_length).collect::<String>())
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `1500` → `2s`, `61000` → `1m` (cosmokit's `Time.format`).
pub fn format_duration(ms: i64) -> String {
    const SECOND: i64 = 1000;
    const MINUTE: i64 = 60 * SECOND;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    let abs = ms.abs();
    let round = |unit: i64| (ms as f64 / unit as f64).round() as i64;
    if abs >= DAY - HOUR / 2 {
        format!("{}d", round(DAY))
    } else if abs >= HOUR - MINUTE / 2 {
        format!("{}h", round(HOUR))
    } else if abs >= MINUTE - SECOND / 2 {
        format!("{}m", round(MINUTE))
    } else if abs >= SECOND {
        format!("{}s", round(SECOND))
    } else {
        format!("{ms}ms")
    }
}

/// Expands `yyyy MM dd hh mm ss SSS` with the local time.
pub fn time_template(template: &str) -> String {
    let now = chrono::Local::now();
    [("yyyy", "%Y"), ("yy", "%y"), ("MM", "%m"), ("dd", "%d"), ("hh", "%H"), ("mm", "%M"), ("ss", "%S"), ("SSS", "%3f")]
        .iter()
        .fold(template.to_string(), |result, (token, spec)| result.replace(token, &now.format(spec).to_string()))
}

fn detect_colors() -> u8 {
    if std::env::var_os("NO_COLOR").is_some() || !std::io::stdout().is_terminal() {
        return 0;
    }
    let colorterm = std::env::var("COLORTERM").unwrap_or_default();
    if colorterm == "truecolor" || colorterm == "24bit" {
        return 3;
    }
    if std::env::var("TERM").is_ok_and(|term| term.contains("256")) { 2 } else { 1 }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}
