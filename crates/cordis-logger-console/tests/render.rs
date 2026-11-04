//! Port of `packages/logger-console/tests/index.spec.ts`.

use cordis_core::{FiberId, Level, Message};
use cordis_logger_console::{Align, Colors, ConsoleConfig, ConsoleExporter, LabelStyle, color, format_duration};

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
fn levels_by_logger_name() {
    use cordis_core::Exporter;
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
    let root = cordis_core::Context::new();
    let fiber =
        root.plugin_value::<cordis_logger_console::ConsolePlugin>(serde_json::json!({ "colors": false })).unwrap();
    fiber.settled().await.unwrap();
    assert_eq!(fiber.effect_count(), 1);
    fiber.dispose().await.unwrap();
}
