//! Port of `packages/include/tests/parse.spec.ts`.

mod common;

use common::App;
use serde_json::json;

// a 1.1 directive would read these as `true`, 511, 5 and a merge
#[tokio::test(flavor = "multi_thread")]
async fn resolves_scalars_by_the_core_schema_whatever_version_the_file_declares() {
    let text = "%YAML 1.1\n---\n- id: a\n  name: config-plugin\n  config:\n    tag: a\n    value: [yes, 0777, 0b101]\n    <<: { merged: true }\n";
    let app = App::setup("parse-scalars.yml", text, json!({})).await;
    let config = app.config("a").unwrap();
    assert_eq!(config["value"], json!(["yes", 777, "0b101"]));
    assert_eq!(config["<<"], json!({ "merged": true }));
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_expressions_back_with_their_tag() {
    let text = "- id: a\n  name: config-plugin\n  config:\n    tag: a\n    value: !!js \"'#' + 1\"\n";
    let app = App::setup("parse-expr.yml", text, json!({})).await;
    assert_eq!(app.config("a").unwrap()["value"], json!("#1"));

    app.update("a", json!({ "config": { "tag": "a", "value": { "__jsExpr": "'#' + 2" } } })).await;
    app.include().flush().await;
    // the dumper quotes with single quotes, where the TS one picks double quotes
    assert!(app.text().contains("    value: !!js '''#'' + 2'\n"), "{}", app.text());
    assert_eq!(app.read()[0]["config"]["value"], json!({ "__jsExpr": "'#' + 2" }));

    // force a re-read of what was written
    app.write(app.text() + "# touched\n");
    app.settle().await;
    assert_eq!(app.config("a").unwrap()["value"], json!("#2"));
}
#[test]
fn an_alias_that_precedes_its_anchor_is_told_apart_from_a_missing_one() {
    // A forward reference: the anchor exists lower in the document.
    let error = kardia_include::yaml::parse("b: *later\na: &later 1\n").unwrap_err();
    assert_eq!(error.message, "alias *later must follow its anchor");

    // No anchor of that name exists at all.
    let error = kardia_include::yaml::parse("a: *missing\n").unwrap_err();
    assert_eq!(error.message, "unresolved alias: *missing");
}
