//! Port of `packages/include/tests/path.spec.ts`.

mod common;

use common::{App, plugin, write_fixture};
use serde_json::json;

// `path` can be an absolute filesystem path (from a command line or from
// another plugin). The TS spec runs only on Windows, where a drive letter
// once read as a URL scheme; an absolute path is checked everywhere here.
#[tokio::test(flavor = "multi_thread")]
async fn accepts_a_drive_letter_path() {
    let filename = write_fixture("path-abs.yml", vec![plugin("inner", 1)]);
    assert!(filename.is_absolute());
    let app = App::boot(filename.clone(), json!({ "path": filename.to_string_lossy() })).await;

    assert_eq!(app.include().filename(), filename);
    assert_eq!(app.config("inner").unwrap()["tag"], json!("inner"));
}
