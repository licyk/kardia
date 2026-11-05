//! Port of `packages/include/tests/location.spec.ts`.
//!
//! Errors the include reports itself (read, parse, file shape) carry
//! `file:line:col`. Errors the loader reports for an entry (option and
//! config validation, plugin resolution, expressions) are not relocated to
//! file positions yet: the cases that check those positions are ignored
//! until the loader can ask a tree's backend where a value is written.
//! JavaScript stack-trace mechanics (function frames, module evaluation)
//! have no counterpart and are not ported.

mod common;

use std::path::Path;

use common::{App, write_fixture};
use cordis_core::Level;
use serde_json::json;

const RELOCATION: &str = "needs cordis-loader: relocate entry errors through the tree backend";

/// `file:line:column` of the first occurrence of `needle`.
fn at(filename: &Path, source: &str, needle: &str) -> String {
    let before = &source[..source.find(needle).expect("needle in source")];
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().unwrap_or_default().chars().count() + 1;
    format!("{}:{line}:{column}", filename.display())
}

struct Located {
    app: App,
    source: String,
}

impl Located {
    async fn start(name: &str, source: &str) -> Self {
        Self { app: App::setup(name, source, json!({})).await, source: source.into() }
    }

    fn at(&self, needle: &str) -> String {
        at(&self.app.filename, &self.source, needle)
    }

    fn file(&self) -> String {
        self.app.filename.display().to_string()
    }

    fn errors(&self) -> Vec<String> {
        self.app.logs(Level::Error)
    }

    fn error_of(&self, id: &str) -> String {
        let frame = format!("at {id} (");
        self.errors().into_iter().find(|error| error.contains(&frame)).unwrap_or_default()
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs cordis-loader: relocate entry errors through the tree backend"]
async fn points_a_validation_issue_at_the_value_in_the_file() {
    let app =
        Located::start("location-value.yml", "- id: a\n  name: schema-plugin\n  config:\n    port: \"80\"\n").await;
    let expected = format!(
        "invalid config:\n  - port must be a number (at port, {})\n    at a ({}:1:3)",
        app.at("\"80\""),
        app.file()
    );
    assert_eq!(app.errors(), vec![expected], "{RELOCATION}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs cordis-loader: relocate entry errors through the tree backend"]
async fn points_a_missing_value_at_the_key_that_should_hold_it() {
    let source =
        "- id: a\n  name: schema-plugin\n  config:\n    hosts: [localhost, 1]\n- id: b\n  name: schema-plugin\n";
    let app = Located::start("location-missing.yml", source).await;
    let expected = format!(
        "  - port must be a number (at port, {})\n  - host must be a string (at hosts.1, {})",
        app.at("config:"),
        app.at("1]")
    );
    assert!(app.error_of("a").contains(&expected), "{:?}", app.errors());
    assert!(app.error_of("b").contains(&format!("  - config is required (at {})", app.at("id: b"))));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs cordis-loader: relocate entry errors through the tree backend"]
async fn locates_entries_nested_in_groups() {
    let source = "- id: g\n  name: group\n  group: true\n  config:\n    - id: a\n      name: schema-plugin\n      config:\n        port: !!js \"'80'\"\n";
    let app = Located::start("location-group.yml", source).await;
    let [error] = &app.errors()[..] else { panic!("{:?}", app.errors()) };
    assert!(error.contains(&format!("  - port must be a number (at port, {})", app.at("\"'80'\""))));
    assert!(error.contains(&format!("    at a ({0}:5:7)\n    at g ({0}:1:3)", app.file())));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs cordis-loader: relocate entry errors through the tree backend"]
async fn locates_patched_values_in_the_file_that_declares_the_patch() {
    write_fixture("location-inner.yml", "- id: a\n  name: schema-plugin\n  config:\n    port: 80\n");
    let source = "- id: inc\n  name: include\n  config:\n    path: ./location-inner.yml\n    patches:\n      - id: a\n        config: { port: \"80\" }\n      - insert:\n          - id: b\n            name: schema-plugin\n            config: { port: 80, hosts: [1] }\n";
    let app = Located::start("location-outer.yml", source).await;
    assert!(app.error_of("a").contains(&format!("(at port, {})", app.at("\"80\""))));
    assert!(app.error_of("b").contains(&format!("(at hosts.0, {})", app.at("1]"))));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs cordis-loader: relocate entry errors through the tree backend"]
async fn locates_overrides_of_inserted_entries_in_the_override() {
    write_fixture("location-insert-inner.yml", "[]\n");
    let source = "- id: inc\n  name: include\n  config:\n    path: ./location-insert-inner.yml\n    patches:\n      - insert:\n          - id: a\n            name: schema-plugin\n            config: { port: 80 }\n      - id: a\n        config: { port: \"80\" }\n";
    let app = Located::start("location-insert-outer.yml", source).await;
    let [error] = &app.errors()[..] else { panic!("{:?}", app.errors()) };
    assert!(error.contains(&format!("(at port, {})", app.at("\"80\""))));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs cordis-loader: relocate entry errors through the tree backend"]
async fn aliases_locate_a_config_value_at_its_anchor() {
    let source = "- id: a\n  name: schema-plugin\n  config: &c\n    port: &p \"80\"\n- id: b\n  name: schema-plugin\n  config: *c\n- id: c\n  name: schema-plugin\n  config:\n    port: *p\n";
    let app = Located::start("location-alias-config.yml", source).await;
    assert_eq!(app.errors().len(), 3);
    for id in ["a", "b", "c"] {
        assert!(app.error_of(id).contains(&format!("(at port, {})", app.at("\"80\""))), "{id}");
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs cordis-loader: relocate entry errors through the tree backend"]
async fn aliases_point_a_missing_value_under_an_alias_at_the_key_that_holds_it() {
    let source = "- id: a\n  name: config-plugin\n  intercept: &c {}\n- id: b\n  name: schema-plugin\n  config: *c\n";
    let app = Located::start("location-alias-missing.yml", source).await;
    let [error] = &app.errors()[..] else { panic!("{:?}", app.errors()) };
    assert!(error.contains(&format!("(at port, {})", app.at("config: *c"))));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs cordis-loader: relocate entry errors through the tree backend"]
async fn aliases_locate_an_entry_option_at_its_anchor() {
    let source =
        "- id: a\n  name: config-plugin\n  isolate: { x: &g 1 }\n- id: b\n  name: config-plugin\n  group: *g\n";
    let app = Located::start("location-alias-fields.yml", source).await;
    assert!(app.error_of("b").contains(&format!("(at group, {})", app.at("1 }"))));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs cordis-loader: relocate entry errors through the tree backend"]
async fn aliases_locate_an_expression_at_its_anchor() {
    let source = "- id: a\n  name: config-plugin\n  intercept: { x: &x !!js \"1 + foo\" }\n- id: b\n  name: config-plugin\n  disabled: *x\n";
    let app = Located::start("location-alias-expr.yml", source).await;
    assert!(app.error_of("b").contains(&format!("    at disabled ({})\n    at b ({}:4:3)", app.at("foo"), app.file())));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs cordis-loader: relocate entry errors through the tree backend"]
async fn locates_entries_of_a_group_config_replaced_by_a_patch() {
    let inner = write_fixture("location-group-inner.yml", "- id: g\n  name: group\n  group: true\n  config: []\n");
    let source = "- id: inc\n  name: include\n  config:\n    path: ./location-group-inner.yml\n    patches:\n      - id: g\n        config:\n          - id: a\n            name: schema-plugin\n            config: { port: \"80\" }\n";
    let app = Located::start("location-group-outer.yml", source).await;
    let [error] = &app.errors()[..] else { panic!("{:?}", app.errors()) };
    assert!(error.contains(&format!("(at port, {})", app.at("\"80\""))));
    assert!(error.contains(&format!("    at a ({}:8:13)\n    at g ({}:1:3)", app.file(), inner.display())));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs cordis-loader: relocate entry errors through the tree backend"]
async fn ignores_patches_that_do_not_apply() {
    let inner =
        write_fixture("location-skip-inner.yml", "- id: a\n  name: schema-plugin\n  config: { port: \"80\" }\n");
    let source = "- id: inc\n  name: include\n  config:\n    path: ./location-skip-inner.yml\n    patches:\n      - id: a\n        name: other-plugin\n        config: { port: 80 }\n";
    let app = Located::start("location-skip-outer.yml", source).await;
    let [error] = &app.errors()[..] else { panic!("{:?}", app.errors()) };
    assert!(error.contains(&format!("(at port, {}:3:19)", inner.display())));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs cordis-loader: relocate entry errors through the tree backend"]
async fn follows_patches_across_nested_includes() {
    let inner = write_fixture("location-nested-inner.yml", "- id: a\n  name: schema-plugin\n  config: { port: 80 }\n");
    let middle = write_fixture(
        "location-nested-middle.yml",
        "- id: inc\n  name: include\n  config:\n    path: ./location-nested-inner.yml\n    patches:\n      - id: a\n        config: { port: \"middle\" }\n",
    );
    // the outer patch replaces the whole config of `inc`, its patches included
    let source = "- id: mid\n  name: include\n  config:\n    path: ./location-nested-middle.yml\n    patches:\n      - id: inc\n        config:\n          path: ./location-nested-inner.yml\n          patches:\n            - id: a\n              config: { port: \"outer\" }\n";
    let app = Located::start("location-nested-outer.yml", source).await;
    let [error] = &app.errors()[..] else { panic!("{:?}", app.errors()) };
    assert!(error.contains(&format!("(at port, {})", app.at("\"outer\""))));
    let frames = format!(
        "    at a ({}:1:3)\n    at inc ({}:1:3)\n    at mid ({}:1:3)",
        inner.display(),
        middle.display(),
        app.file()
    );
    assert!(error.contains(&frames));
}

#[tokio::test(flavor = "multi_thread")]
async fn leaves_a_config_passed_at_runtime_unlocated() {
    let app = Located::start("location-runtime.yml", "- id: a\n  name: schema-plugin\n  config:\n    port: 80\n").await;
    let fiber = app.app.tree().resolve("a").unwrap().inner_fiber().unwrap();
    let error = fiber.update(json!({ "port": "runtime" })).err().expect("rejected");
    assert_eq!(error.to_string(), "invalid config:\n  - port must be a number (at port)");
}

#[tokio::test(flavor = "multi_thread")]
async fn leaves_entries_outside_any_file_untraced() {
    let app = Located::start("location-memory.yml", "[]\n").await;
    app.app.loader.create(json!({ "name": "schema-plugin", "config": { "port": "80" } }), None, None).await.unwrap();
    app.app.loader.settled().await;
    assert_eq!(app.errors(), vec!["invalid config:\n  - port must be a number (at port)"]);
}

// --- entry fields ---

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs cordis-loader: relocate entry errors through the tree backend"]
async fn entry_fields_point_invalid_fields_at_their_values() {
    let source = "- id: a\n  name: config-plugin\n  disabled: \"no\"\n  inject: [loader, 1]\n- id: b\n  group: true\n";
    let app = Located::start("location-fields.yml", source).await;
    assert_eq!(app.errors().len(), 2);
    let expected = format!(
        "invalid entry options:\n  - disabled must be a boolean (at disabled, {})\n  - inject.1 must be a string (at inject.1, {})\n    at a ({}:1:3)",
        app.at("\"no\""),
        app.at("1]"),
        app.file()
    );
    assert_eq!(app.error_of("a"), expected);
    // a missing field falls back to the entry
    let missing =
        format!("  - name is required (at name, {0})\n  - config is required (at config, {0})", app.at("id: b"));
    assert!(app.error_of("b").contains(&missing));
    assert!(app.app.tree().resolve("a").is_err());
}

/// The location of the rejected `name` is pending ([`RELOCATION`]).
#[tokio::test(flavor = "multi_thread")]
async fn entry_fields_accept_an_expression_for_disabled_only() {
    let source =
        "- id: a\n  name: config-plugin\n  disabled: !!js \"false\"\n- id: b\n  name: !!js \"'config-plugin'\"\n";
    let app = Located::start("location-expr.yml", source).await;
    let [error] = &app.errors()[..] else { panic!("{:?}", app.errors()) };
    assert!(error.contains("  - name must be a string (at name"), "{error}");
    assert!(app.app.tree().resolve("a").unwrap().inner_fiber().is_some_and(|fiber| !fiber.is_removed()));
}

/// The location of the rejected `group` is pending ([`RELOCATION`]).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs cordis-loader: `group_update` reports an invalid entry that keeps its id twice"]
async fn entry_fields_unload_an_entry_that_turns_invalid_without_writing_to_the_file() {
    let source = "- id: a\n  name: config-plugin\n";
    let app = Located::start("location-turn.yml", source).await;
    let fiber = app.app.tree().resolve("a").unwrap().inner_fiber().unwrap();

    let invalid = "- id: a\n  name: config-plugin\n  group: 1\n";
    app.app.write(invalid);
    app.app.settle().await;
    let [error] = &app.errors()[..] else { panic!("{:?}", app.errors()) };
    assert!(error.contains("group must be a boolean (at group"), "{error}");
    assert!(app.app.tree().resolve("a").is_err());
    assert!(fiber.is_removed());
    assert_eq!(app.app.text(), invalid);

    app.app.write(source);
    app.app.settle().await;
    assert!(app.app.tree().resolve("a").unwrap().inner_fiber().is_some_and(|fiber| !fiber.is_removed()));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs cordis-loader: relocate entry errors through the tree backend"]
async fn entry_fields_locate_entries_nested_in_groups() {
    let source = "- id: g\n  name: group\n  group: true\n  config:\n    - id: a\n      name: [config-plugin]\n";
    let app = Located::start("location-fields-group.yml", source).await;
    let expected = format!(
        "invalid entry options:\n  - name must be a string (at name, {})\n    at a ({1}:5:7)\n    at g ({1}:1:3)",
        app.at("name: ["),
        app.file()
    );
    assert_eq!(app.errors(), vec![expected]);
}

#[tokio::test(flavor = "multi_thread")]
async fn entry_fields_reject_a_file_with_an_entry_that_is_not_an_object() {
    let app = Located::start("location-entry.yml", "- id: a\n  name: config-plugin\n").await;
    let invalid =
        "- id: a\n  name: config-plugin\n- config-plugin\n- id: g\n  name: group\n  group: true\n  config: [null]\n";
    app.app.write(invalid);
    app.app.settle().await;

    let expected = format!(
        "  - entry must be an object (at {}:3:3)\n  - entry must be an object (at {})",
        app.file(),
        at(&app.app.filename, invalid, "null")
    );
    let warnings = app.app.logs(Level::Warn);
    assert!(warnings.len() == 1 && warnings[0].contains(&expected), "{warnings:?}");
    // the previous tree stays as it was
    assert!(app.app.tree().resolve("a").unwrap().inner_fiber().is_some_and(|fiber| !fiber.is_removed()));
    assert!(app.app.tree().resolve("g").is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn entry_fields_leave_invalid_options_passed_at_runtime_unlocated() {
    let app = Located::start("location-fields-runtime.yml", "- id: a\n  name: config-plugin\n").await;
    let id = format!("{}:a", app.app.id);
    let error = app.app.loader.update(&id, json!({ "disabled": "yes" }), None, None).await.unwrap_err();
    assert_eq!(error.to_string(), "invalid entry options:\n  - disabled must be a boolean (at disabled)");
}

// --- plugin import ---

/// A catalog name is either registered or not, so the TS distinction
/// between a missing module and a missing package collapses into one case.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs cordis-loader: relocate entry errors through the tree backend"]
async fn plugin_import_points_a_plugin_that_cannot_be_found_at_its_name() {
    let source = "- id: a\n  name: ./missing-plugin\n- id: b\n  name: missing-package\n";
    let app = Located::start("location-import.yml", source).await;
    assert_eq!(app.errors().len(), 2);
    let expected = format!(
        "Cannot find plugin 'missing-package' (at name, {})\n    at b ({}:3:3)",
        app.at("missing-package"),
        app.file()
    );
    assert_eq!(app.error_of("b"), expected);
    assert!(app.error_of("a").contains(&format!("(at name, {})", app.at("./missing"))));
}

#[tokio::test(flavor = "multi_thread")]
async fn plugin_import_leaves_entries_outside_any_file_untraced() {
    let app = Located::start("location-import-memory.yml", "[]\n").await;
    app.app.loader.create(json!({ "name": "missing-plugin" }), None, None).await.unwrap();
    app.app.loader.settled().await;
    assert_eq!(app.errors(), vec!["Cannot find plugin 'missing-plugin'"]);
}

// --- expressions ---

async fn points_a_failure_at_the_expression(field: &str, lines: &str, label: &str) {
    let source = format!("- id: a\n  name: schema-plugin\n{lines}\n");
    let app = Located::start(&format!("location-expr-{field}.yml"), &source).await;
    let [error] = &app.errors()[..] else { panic!("{:?}", app.errors()) };
    assert!(
        error.ends_with(&format!("\n    at {label} ({})\n    at a ({}:1:3)", app.at("foo"), app.file())),
        "{error}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs cordis-loader: relocate entry errors through the tree backend"]
async fn expressions_point_a_failure_in_inject_at_the_expression() {
    points_a_failure_at_the_expression("inject", "  inject:\n    loader: !!js \"1 + foo\"", "inject.loader").await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs cordis-loader: relocate entry errors through the tree backend"]
async fn expressions_point_a_failure_in_disabled_at_the_expression() {
    points_a_failure_at_the_expression("disabled", "  disabled: !!js \"1 + foo\"", "disabled").await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs cordis-loader: relocate entry errors through the tree backend"]
async fn expressions_point_a_failure_in_intercept_at_the_expression() {
    points_a_failure_at_the_expression(
        "intercept",
        "  intercept:\n    loader: { await: !!js \"1 + foo\" }",
        "intercept.loader.await",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs cordis-loader: relocate entry errors through the tree backend"]
async fn expressions_point_a_failure_in_config_at_the_expression() {
    points_a_failure_at_the_expression("config", "  config:\n    port: !!js \"1 + foo\"", "config.port").await;
}

/// Only the escapes part of the TS case: the Rust expression language has
/// no functions to throw from, so there are no frames inside an expression.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs cordis-loader: relocate entry errors through the tree backend"]
async fn expressions_map_positions_across_lines_and_escapes() {
    let source = "- id: b\n  name: config-plugin\n  disabled: !!js \"\\\"it's\\\" + \\u0020 foo\"\n- id: c\n  name: config-plugin\n  disabled: !!js '\"it''s\" + bar'\n";
    let app = Located::start("location-expr-block.yml", source).await;
    assert_eq!(app.errors().len(), 2);
    assert!(app.error_of("b").contains(&format!("    at disabled ({})\n", app.at("foo"))));
    assert!(app.error_of("c").contains(&format!("    at disabled ({})\n", app.at("bar"))));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs cordis-loader: relocate entry errors through the tree backend"]
async fn expressions_point_an_expression_that_does_not_parse_at_its_start() {
    let source = "- id: a\n  name: config-plugin\n  disabled: !!js \"foo bar\"\n";
    let app = Located::start("location-expr-syntax.yml", source).await;
    let [error] = &app.errors()[..] else { panic!("{:?}", app.errors()) };
    assert!(error.ends_with(&format!("\n    at disabled ({})\n    at a ({}:1:3)", app.at("foo bar"), app.file())));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs cordis-loader: relocate entry errors through the tree backend"]
async fn expressions_locate_entries_nested_in_groups() {
    let source = "- id: g\n  name: group\n  group: true\n  config:\n    - id: a\n      name: config-plugin\n      disabled: !!js \"foo\"\n";
    let app = Located::start("location-expr-group.yml", source).await;
    let [error] = &app.errors()[..] else { panic!("{:?}", app.errors()) };
    let frames = format!("\n    at disabled ({})\n    at a ({1}:5:7)\n    at g ({1}:1:3)", app.at("foo"), app.file());
    assert!(error.ends_with(&frames), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs cordis-loader: relocate entry errors through the tree backend"]
async fn expressions_locate_an_expression_written_as_an_object_in_json() {
    let source = serde_json::to_string_pretty(&json!([
        { "id": "a", "name": "config-plugin", "disabled": { "__jsExpr": "1 + foo" } },
    ]))
    .unwrap();
    let app = Located::start("location-expr.json", &source).await;
    let [error] = &app.errors()[..] else { panic!("{:?}", app.errors()) };
    let frames = format!("\n    at disabled ({})\n    at a ({})", app.at("foo"), app.at("{\n    \"id\""));
    assert!(error.ends_with(&frames), "{error}");
}

/// The rejection of the update and the running plugin are checked; the
/// location of the failure is pending ([`RELOCATION`]).
#[tokio::test(flavor = "multi_thread")]
async fn expressions_point_a_failure_on_update_at_the_new_expression() {
    let app =
        Located::start("location-expr-update.yml", "- id: a\n  name: schema-plugin\n  config:\n    port: 80\n").await;
    let fiber = app.app.tree().resolve("a").unwrap().inner_fiber().unwrap();
    app.app.write("- id: a\n  name: schema-plugin\n  config:\n    port: !!js \"1 + foo\"\n");
    app.app.settle().await;

    let [error] = &app.errors()[..] else { panic!("{:?}", app.errors()) };
    assert!(error.contains("foo"), "{error}");
    // the update is rejected, and the plugin keeps running
    assert_eq!(app.app.tree().resolve("a").unwrap().inner_fiber(), Some(fiber.clone()));
    assert!(!fiber.is_removed());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs cordis-loader: relocate entry errors through the tree backend"]
async fn expressions_leave_an_expression_changed_at_runtime_unlocated() {
    let app = Located::start("location-expr-runtime.yml", "- id: a\n  name: config-plugin\n").await;
    app.app.update("a", json!({ "disabled": { "__jsExpr": "foo" } })).await;
    let [error] = &app.errors()[..] else { panic!("{:?}", app.errors()) };
    // the expression has no position, but the entry still has one
    assert!(error.ends_with(&format!("\n    at disabled\n    at a ({}:1:3)", app.file())), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn expressions_leave_entries_outside_any_file_untraced() {
    let app = Located::start("location-expr-memory.yml", "[]\n").await;
    let options = json!({ "name": "config-plugin", "disabled": { "__jsExpr": "foo" } });
    app.app.loader.create(options, None, None).await.unwrap();
    app.app.loader.settled().await;
    let [error] = &app.errors()[..] else { panic!("{:?}", app.errors()) };
    assert!(error.contains("foo") && !error.contains(" (at ") && !error.contains("\n    at "), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn points_parse_errors_at_the_source() {
    let app = Located::start("location-parse.yml", "[]\n").await;
    let source = "- id: a\n  name: !foo schema-plugin\n";
    app.app.write(source);
    app.app.settle().await;

    let expected = format!("  - unresolved tag: !foo (at {})", at(&app.app.filename, source, "!foo"));
    let warnings = app.app.logs(Level::Warn);
    assert!(warnings.len() == 1 && warnings[0].contains(&expected), "{warnings:?}");
}

// --- JSON ---

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs cordis-loader: relocate entry errors through the tree backend"]
async fn json_points_a_validation_issue_at_the_value_in_the_file() {
    let source =
        serde_json::to_string_pretty(&json!([{ "id": "a", "name": "schema-plugin", "config": { "port": "80" } }]))
            .unwrap();
    let app = Located::start("location-value.json", &source).await;
    let expected = format!(
        "invalid config:\n  - port must be a number (at port, {})\n    at a ({})",
        app.at("\"80\""),
        app.at("{\n    \"id\"")
    );
    assert_eq!(app.errors(), vec![expected]);
}

#[tokio::test(flavor = "multi_thread")]
async fn json_points_syntax_errors_at_the_source() {
    let app = Located::start("location-syntax.json", "[]\n").await;
    let source = "[\n  { \"id\": \"a\", }\n]\n";
    app.app.write(source);
    app.app.settle().await;

    let warnings = app.app.logs(Level::Warn);
    let [warning] = &warnings[..] else { panic!("{warnings:?}") };
    assert!(warning.contains(&format!("(at {})", at(&app.app.filename, source, "}\n]"))), "{warning}");
    assert!(!warning.contains("at position") && !warning.contains("at line"), "{warning}");
}

#[tokio::test(flavor = "multi_thread")]
async fn json_points_a_non_array_file_at_its_value() {
    let app = Located::start("location-shape.json", "[]\n").await;
    let source = "\n{ \"id\": \"a\" }\n";
    app.app.write(source);
    app.app.settle().await;

    let expected = format!("  - config file must be a top-level array (at {})", at(&app.app.filename, source, "{"));
    let warnings = app.app.logs(Level::Warn);
    assert!(warnings.len() == 1 && warnings[0].contains(&expected), "{warnings:?}");
}
