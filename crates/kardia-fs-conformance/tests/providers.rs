//! The suite run against the two providers that ship with kardia.

use std::sync::Arc;

use kardia_fs::LocalFs;
use kardia_fs_conformance::{Outcome, Suite};
use kardia_fs_memory::MemoryFs;

async fn report(name: &str, provider: Arc<dyn kardia_fs::FsProvider>) {
    let suite = Suite::boot(provider).await.expect("the suite starts");
    let results = suite.run().await;
    let passed = results.iter().filter(|(_, outcome)| matches!(outcome, Outcome::Passed)).count();
    let skipped = results.iter().filter(|(_, outcome)| matches!(outcome, Outcome::Skipped)).count();
    let failures = suite.failures().await;
    assert!(failures.is_empty(), "{name} fails the suite:\n{}", failures.join("\n"));
    // Every case either passes or is skipped, and the suite has 11 of them.
    assert_eq!(passed + skipped, 11, "{name}: {results:?}");
}

#[tokio::test]
async fn the_memory_provider_satisfies_the_suite() {
    report("memory", Arc::new(MemoryFs::new())).await;
}

#[tokio::test]
async fn the_local_provider_satisfies_the_suite() {
    // `LocalFs` serves `file:`, so the base URL names a real temporary directory.
    let directory = tempfile::tempdir().expect("a temporary directory");
    let base = kardia_fs::Url::from_directory_path(directory.path()).expect("a file URL");
    let suite = Suite::boot_at(Arc::new(LocalFs), "file:", base.as_str()).await.expect("the suite starts");
    let failures = suite.failures().await;
    assert!(failures.is_empty(), "local fails the suite:\n{}", failures.join("\n"));
}
