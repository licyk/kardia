//! The memory provider against the operations `kardia-fs` defines.

use kardia_core::Context;
use kardia_fs::{
    CopyOptions, EntryType, Fs, FsCode, FsConfig, FsError, FsExt, FsPlugin, MkdirOptions, Url, WriteOptions,
};
use kardia_fs_memory::{MemoryFsConfig, MemoryFsPlugin};
use serde_json::json;

async fn setup(files: &[(&str, &str)]) -> Context {
    let root = Context::new();
    root.plugin::<FsPlugin>(FsConfig { base_url: Some(Url::parse("mem://app/").unwrap()) })
        .unwrap()
        .settled()
        .await
        .unwrap();
    let files = files.iter().map(|(path, content)| ((*path).to_owned(), (*content).to_owned())).collect();
    root.plugin::<MemoryFsPlugin>(MemoryFsConfig { mount: "mem:".into(), files }).unwrap().settled().await.unwrap();
    root
}

fn code(error: &kardia_core::Error) -> Option<FsCode> {
    error.downcast_ref::<FsError>().map(|error| error.code)
}

#[tokio::test]
async fn reads_the_files_the_config_declares() {
    let root = setup(&[("a.txt", "hello")]).await;
    let fs = root.get::<Fs>().unwrap();
    assert_eq!(fs.file("a.txt").unwrap().text().await.unwrap(), "hello");
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn writes_and_reports_a_version() {
    let root = setup(&[]).await;
    let fs = root.get::<Fs>().unwrap();
    let version = fs.file("a.txt").unwrap().write("data", WriteOptions::default()).await.unwrap();
    assert!(version.is_some());
    assert_eq!(fs.file("a.txt").unwrap().text().await.unwrap(), "data");
    assert_eq!(fs.stat("a.txt").await.unwrap().kind, EntryType::File);
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn an_exclusive_write_fails_on_an_existing_file() {
    let root = setup(&[("a.txt", "old")]).await;
    let fs = root.get::<Fs>().unwrap();
    let error = fs
        .file("a.txt")
        .unwrap()
        .write("new", WriteOptions { exclusive: true, ..Default::default() })
        .await
        .unwrap_err();
    assert_eq!(code(&error), Some(FsCode::AlreadyExists));
    assert_eq!(fs.file("a.txt").unwrap().text().await.unwrap(), "old");
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn a_conditional_write_needs_the_current_version() {
    let root = setup(&[("a.txt", "old")]).await;
    let fs = root.get::<Fs>().unwrap();
    let version = fs.stat("a.txt").await.unwrap().version.unwrap();
    fs.file("a.txt")
        .unwrap()
        .write("new", WriteOptions { if_match: Some(version.clone()), ..Default::default() })
        .await
        .unwrap();
    let error = fs
        .file("a.txt")
        .unwrap()
        .write("stale", WriteOptions { if_match: Some(version), ..Default::default() })
        .await
        .unwrap_err();
    assert_eq!(code(&error), Some(FsCode::VersionMismatch));
    assert_eq!(fs.file("a.txt").unwrap().text().await.unwrap(), "new");
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn reads_a_range_with_the_semantics_of_a_slice() {
    let root = setup(&[("a.txt", "0123456789")]).await;
    let fs = root.get::<Fs>().unwrap();
    let file = fs.file("a.txt").unwrap();
    assert_eq!(file.slice(2, Some(5)).text().await.unwrap(), "234");
    // A negative bound counts from the end.
    assert_eq!(file.slice(-3, None).text().await.unwrap(), "789");
    // A range past the end is clamped.
    assert_eq!(file.slice(8, Some(100)).text().await.unwrap(), "89");
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn creates_moves_and_removes_directories() {
    let root = setup(&[]).await;
    let fs = root.get::<Fs>().unwrap();
    fs.mkdir("a/b", MkdirOptions { recursive: true }).await.unwrap();
    fs.file("a/b/c.txt").unwrap().write("x", WriteOptions::default()).await.unwrap();
    assert_eq!(fs.readdir("a/b").await.unwrap().len(), 1);
    // A directory is not removed while it has entries.
    let error = fs.remove("a/b", kardia_fs::RemoveOptions::default()).await.unwrap_err();
    assert_eq!(code(&error), Some(FsCode::DirectoryNotEmpty));
    fs.rename("a/b", "a/c").await.unwrap();
    assert_eq!(fs.file("a/c/c.txt").unwrap().text().await.unwrap(), "x");
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn lists_a_directory_with_the_type_of_each_entry() {
    let root = setup(&[("d/a.txt", "a")]).await;
    let fs = root.get::<Fs>().unwrap();
    fs.mkdir("d/sub", MkdirOptions::default()).await.unwrap();
    let mut entries: Vec<_> =
        fs.readdir("d").await.unwrap().into_iter().map(|entry| (entry.name, entry.kind)).collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(entries, vec![("a.txt".to_owned(), EntryType::File), ("sub".to_owned(), EntryType::Directory)]);
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn copies_a_file_and_a_tree() {
    let root = setup(&[("a.txt", "data"), ("d/x.txt", "x")]).await;
    let fs = root.get::<Fs>().unwrap();
    fs.copy("a.txt", "b.txt", CopyOptions::default()).await.unwrap();
    assert_eq!(fs.file("b.txt").unwrap().text().await.unwrap(), "data");
    fs.copy("d", "e", CopyOptions { recursive: true, ..Default::default() }).await.unwrap();
    assert_eq!(fs.file("e/x.txt").unwrap().text().await.unwrap(), "x");
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn a_symbolic_link_stores_its_target() {
    let root = setup(&[("a.txt", "x")]).await;
    let fs = root.get::<Fs>().unwrap();
    fs.symlink("a.txt", "link").await.unwrap();
    assert_eq!(fs.readlink("link").await.unwrap(), "a.txt");
    assert_eq!(fs.stat("link").await.unwrap().kind, EntryType::Symlink);
    let error = fs.readlink("a.txt").await.unwrap_err();
    assert_eq!(code(&error), Some(FsCode::NotALink));
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn a_provider_at_the_empty_prefix_serves_every_location() {
    let root = Context::new();
    // The empty prefix serves every location, so the entry needs a base URL to
    // resolve a path against.
    root.plugin::<FsPlugin>(FsConfig { base_url: Some(Url::parse("mem://app/").unwrap()) })
        .unwrap()
        .settled()
        .await
        .unwrap();
    root.plugin_value::<MemoryFsPlugin>(json!({ "mount": "", "files": { "any/thing.txt": "ok" } }))
        .unwrap()
        .settled()
        .await
        .unwrap();
    let fs = root.get::<Fs>().unwrap();
    assert_eq!(fs.file("any/thing.txt").unwrap().text().await.unwrap(), "ok");
    root.fiber().dispose().await.unwrap();
}
