//! The exporter that renders messages and prints them to stdout.

use std::io::IsTerminal;
use std::time::{SystemTime, UNIX_EPOCH};

use kardia_core::{Exporter, Level, Message};
use parking_lot::Mutex;

use crate::config::{Align, Colors, ConsoleConfig};
use crate::format::{color, format_duration, name_color, time_template};

pub struct ConsoleExporter {
    colors: u8,
    config: ConsoleConfig,
    timestamp: Mutex<u64>,
}

impl ConsoleExporter {
    pub fn new(config: ConsoleConfig) -> Self {
        let colors = match config.colors {
            Some(Colors::Level(level)) => level,
            Some(Colors::Enabled(false)) => 0,
            Some(Colors::Enabled(true)) => detect_colors().max(1),
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
