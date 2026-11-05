//! Port of `packages/logger-console/tests/index.spec.ts`.

use std::sync::{Arc, Mutex};

use kardia_core::{AggregateError, Context, Error, Exporter, FiberId, Level, Message};
use kardia_logger_console::{Align, Colors, ConsoleConfig, ConsoleExporter, LabelStyle, color, format_duration};

fn message(level: Level, text: &str, ts: u64) -> Message {
    Message { sn: 1, ts, name: "test".into(), level, text: text.into(), fiber: FiberId::ROOT }
}

fn exporter(label: LabelStyle) -> ConsoleExporter {
    ConsoleExporter::new(ConsoleConfig {
        colors: Some(Colors::Level(0)),
        show_diff: true,
        show_time: String::new(),
        label,
        ..ConsoleConfig::default()
    })
}

#[test]
fn renders_level_name_text_and_diff() {
    let exporter = exporter(LabelStyle::default());
    let first = message(Level::Error, "message", 1_000);
    let base = exporter.render(&first);
    assert!(base.starts_with("[E] test message +"));
    assert_eq!(exporter.render(&message(Level::Info, "{ foo: 'bar' }", 1_002)), "[I] test { foo: 'bar' } +2ms");
}

/// Collects what the console exporter would print, like the TS spec's `export` override.
struct Capture {
    console: ConsoleExporter,
    data: Mutex<String>,
}

impl Exporter for Capture {
    fn export(&self, message: &Message) {
        let mut data = self.data.lock().unwrap();
        *data += &self.console.render(message);
        *data += "\n";
    }

    fn level(&self, name: &str) -> Option<Level> {
        self.console.level(name)
    }
}

fn capture(ctx: &Context) -> Arc<Capture> {
    let capture = Arc::new(Capture { console: exporter(LabelStyle::default()), data: Mutex::default() });
    ctx.add_exporter(capture.clone()).unwrap();
    capture
}

/// The output with the trailing ` +<diff>`, which depends on the wall clock.
fn without_diff(data: &str) -> &str {
    let (head, diff) = data.rsplit_once(" +").unwrap();
    assert!(diff.ends_with("ms\n"), "{data:?}");
    head
}

#[test]
fn format_error() {
    let ctx = Context::new();
    let capture = capture(&ctx);
    let outer = Error::new(AggregateError { message: "outer".into(), errors: vec![Error::msg("message")] });
    ctx.logger_named("test").log_error(&outer);
    assert_eq!(without_diff(&capture.data.lock().unwrap()), "[E] test message");
}

#[test]
fn log_levels() {
    let ctx = Context::new();
    let capture = capture(&ctx);
    let logger = ctx.logger_named("test");
    logger.debug("foo bar");
    assert_eq!(*capture.data.lock().unwrap(), "");

    logger.with_level(Level::Debug).debug("foo bar");
    assert_eq!(without_diff(&capture.data.lock().unwrap()), "[D] test foo bar");
}

#[test]
fn label_style() {
    let exporter = exporter(LabelStyle { align: Align::Right, width: 10, margin: Some(2) });
    exporter.render(&message(Level::Info, "warm up", 5));
    assert_eq!(
        exporter.render(&message(Level::Info, "message\nmessage", 5)),
        "      test  [I]  message\n                 message +0ms"
    );
}

#[test]
fn colors_true_turns_colors_on_and_false_turns_them_off() {
    let render = |colors: &str| {
        let config: ConsoleConfig = serde_json_from(&format!(r#"{{ "colors": {colors}, "showTime": "" }}"#));
        ConsoleExporter::new(config).render(&message(Level::Info, "text", 1_000))
    };
    assert!(render("true").contains('\u{1b}'), "{:?}", render("true"));
    assert!(!render("false").contains('\u{1b}'));
    assert!(!render("0").contains('\u{1b}'));
}

#[test]
fn levels_by_logger_name() {
    let config: ConsoleConfig = serde_json_from(r#"{ "levels": { "default": 1, "test": 3 }, "colors": false }"#);
    let exporter = ConsoleExporter::new(config);
    assert_eq!(exporter.level("test"), Some(Level::Debug));
    assert_eq!(exporter.level("other"), Some(Level::Warn));
}

fn serde_json_from(source: &str) -> ConsoleConfig {
    serde_json::from_str(source).unwrap()
}

#[test]
fn colours_and_durations() {
    assert_eq!(color(0, 3, "x", ";1"), "x");
    assert_eq!(color(1, 3, "x", ";1"), "\u{1b}[33mx\u{1b}[0m");
    assert_eq!(color(2, 200, "x", ";1"), "\u{1b}[38;5;200;1mx\u{1b}[0m");
    assert_eq!(format_duration(999), "999ms");
    assert_eq!(format_duration(1500), "2s");
    assert_eq!(format_duration(90_000), "2m");
    assert_eq!(format_duration(3 * 3_600_000), "3h");
}

#[tokio::test]
async fn plugin_registers_the_exporter_as_an_effect() {
    let root = kardia_core::Context::new();
    let fiber =
        root.plugin_value::<kardia_logger_console::ConsolePlugin>(serde_json::json!({ "colors": false })).unwrap();
    fiber.settled().await.unwrap();
    assert_eq!(fiber.effect_count(), 1);
    fiber.dispose().await.unwrap();
}
