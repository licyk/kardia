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
