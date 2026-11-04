//! Watch tests on the real file system (port of the core cases of `watch.spec.ts`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use cordis_core::{Context, Fiber, Inject, PluginDef};
use cordis_fs::{Filter, Fs, FsExt, FsPlugin, WatchOptions};
use parking_lot::Mutex;
use serde_json::Value;

fn fixture(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cordis-fs-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn consumer() -> (Context, Fiber) {
    let root = Context::new();
    root.plugin::<FsPlugin>(()).unwrap().settled().await.unwrap();
    let fiber =
        root.plugin_def(&PluginDef::new(|_, _| async { Ok(()) }).inject(Inject::of::<(Fs,)>()), Value::Null).unwrap();
    fiber.settled().await.unwrap();
    (root, fiber)
}

/// Writes the files until `done` holds (the OS may deliver the first events late).
async fn touch(paths: &[&Path], done: impl Fn() -> bool) {
    for attempt in 0..80 {
        if done() {
            return;
        }
        for path in paths {
            std::fs::write(path, format!("v{attempt}")).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("touch timed out");
}

#[tokio::test(flavor = "multi_thread")]
async fn watches_a_file_through_atomic_replacements() {
    let dir = fixture("file");
    let file = dir.join("cordis.yml");
    std::fs::write(&file, "a").unwrap();
    let (_, fiber) = consumer().await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    let watcher = fiber.context().get::<Fs>().unwrap().watch(&file, WatchOptions::default()).unwrap();
    watcher.on_change(move |path| s.lock().push(path.to_path_buf()));
    let ready = Arc::new(Mutex::new(false));
    let r = ready.clone();
    watcher.on_ready(move || *r.lock() = true);
    assert!(*ready.lock());

    let other = dir.join("other.txt");
    touch(&[&file, &other], || !seen.lock().is_empty()).await;
    assert!(seen.lock().iter().all(|path| path == &file));

    // replaced through a rename, as by an atomic write
    seen.lock().clear();
    let tmp = dir.join(".cordis.yml.tmp");
    for attempt in 0..80 {
        if !seen.lock().is_empty() {
            break;
        }
        std::fs::write(&tmp, format!("r{attempt}")).unwrap();
        std::fs::rename(&tmp, &file).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(seen.lock().contains(&file));
}

#[tokio::test(flavor = "multi_thread")]
async fn watches_a_directory_with_a_filter_and_ends_with_its_fiber() {
    let dir = fixture("dir");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let (kept, ignored) = (dir.join("src/a.rs"), dir.join("notes.txt"));
    let (_, fiber) = consumer().await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    let options = WatchOptions { filter: Some(Filter::Patterns(vec!["src/**".into()])) };
    let watcher = fiber.context().get::<Fs>().unwrap().watch(&dir, options).unwrap();
    watcher.on_change(move |path| s.lock().push(path.to_path_buf()));
    assert_eq!(fiber.effect_count(), 1);

    touch(&[&kept, &ignored], || seen.lock().contains(&kept)).await;
    assert!(!seen.lock().contains(&ignored));

    fiber.dispose().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    seen.lock().clear();
    std::fs::write(&kept, "after").unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(seen.lock().is_empty());
}
