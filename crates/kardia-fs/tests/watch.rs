//! Watch tests on the real file system (port of `fs-node/tests/watch.spec.ts`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use kardia_core::{Context, Fiber, Inject, PluginDef};
use kardia_fs::{Filter, Fs, FsExt, FsPlugin, Subscription, WatchOptions, Watcher};
use parking_lot::Mutex;
use serde_json::Value;

fn fixture(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("kardia-fs").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: PathBuf) -> PathBuf {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "").unwrap();
    path
}

async fn consumer() -> (Context, Fiber) {
    let root = Context::new();
    root.plugin::<FsPlugin>(()).unwrap().settled().await.unwrap();
    let fiber =
        root.plugin_def(&PluginDef::new(|_, _| async { Ok(()) }).inject(Inject::of::<(Fs,)>()), Value::Null).unwrap();
    fiber.settled().await.unwrap();
    (root, fiber)
}

fn watch(fiber: &Fiber, path: impl AsRef<Path>, filter: Option<Filter>) -> Watcher {
    fiber.context().get::<Fs>().unwrap().watch(path, WatchOptions { filter }).unwrap()
}

type Seen = Arc<Mutex<Vec<PathBuf>>>;

fn record(watcher: &Watcher) -> Seen {
    let seen = Seen::default();
    let s = seen.clone();
    watcher.on_change(move |path| s.lock().push(path.to_path_buf()));
    seen
}

fn distinct(seen: &Seen) -> Vec<PathBuf> {
    let mut paths = seen.lock().clone();
    paths.sort();
    paths.dedup();
    paths
}

fn counter() -> (Arc<AtomicUsize>, impl Fn() + Clone + Send + Sync + 'static) {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    (calls, move || {
        c.fetch_add(1, Ordering::SeqCst);
    })
}

fn count(calls: &AtomicUsize) -> usize {
    calls.load(Ordering::SeqCst)
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

async fn eventually(check: impl Fn() -> bool) {
    for _ in 0..250 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out");
}

#[tokio::test(flavor = "multi_thread")]
async fn watches_a_file_through_atomic_replacements() {
    let dir = fixture("file");
    let file = dir.join("kardia.yml");
    std::fs::write(&file, "a").unwrap();
    let (_, fiber) = consumer().await;
    let seen = record(&watch(&fiber, &file, None));

    let other = dir.join("other.txt");
    touch(&[&file, &other], || !seen.lock().is_empty()).await;
    assert!(seen.lock().iter().all(|path| path == &file));

    // replaced through a rename, as by an atomic write
    seen.lock().clear();
    let tmp = dir.join(".kardia.yml.tmp");
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
    let seen = record(&watch(&fiber, &dir, Some(Filter::Patterns(vec!["src/**".into()]))));
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

#[tokio::test]
async fn resolves_a_relative_path_against_the_working_directory_without_a_context_directory() {
    let path = write(fixture("cwd").join("cwd.txt"));
    let (_, fiber) = consumer().await;
    let cwd = std::env::current_dir().unwrap();
    let up = cwd.ancestors().position(|dir| path.starts_with(dir)).unwrap();
    let base = cwd.ancestors().nth(up).unwrap();
    let relative = Path::new(&"../".repeat(up)).join(path.strip_prefix(base).unwrap());
    let seen = record(&watch(&fiber, &relative, None));

    touch(&[&path], || !seen.lock().is_empty()).await;
    assert_eq!(distinct(&seen), [path]);
}

// adapted from the file URL cases: a path takes the place of a URL
#[tokio::test]
async fn accepts_a_directory_path_with_a_trailing_slash() {
    let dir = fixture("trailing-slash");
    let path = write(dir.join("file.txt"));
    let (_, fiber) = consumer().await;
    let filter = Filter::Predicate(Arc::new(|path, _| path == "file.txt"));
    let seen = record(&watch(&fiber, format!("{}/", dir.display()), Some(filter)));

    touch(&[&path], || !seen.lock().is_empty()).await;
    assert_eq!(distinct(&seen), [path]);
}

#[tokio::test]
async fn keeps_watching_a_file_replaced_through_renames() {
    let dir = fixture("atomic");
    let (path, sibling) = (write(dir.join("config.yml")), write(dir.join("sibling.yml")));
    let (_, fiber) = consumer().await;
    let seen = record(&watch(&fiber, &path, None));

    let tmp = dir.join("config.yml.tmp");
    for value in ["v1", "v2"] {
        std::fs::write(&tmp, value).unwrap();
        std::fs::rename(&tmp, &path).unwrap();
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    seen.lock().clear();

    touch(&[&path, &sibling], || !seen.lock().is_empty()).await;
    assert_eq!(distinct(&seen), [path]);
}

#[tokio::test]
async fn reports_the_changed_file_of_a_watched_directory() {
    let dir = fixture("watched-dir");
    let path = write(dir.join("nested/file.txt"));
    let (_, fiber) = consumer().await;
    let seen = record(&watch(&fiber, &dir, None));

    touch(&[&path], || !seen.lock().is_empty()).await;
    assert_eq!(distinct(&seen), [path]);
}

#[tokio::test]
async fn applies_filter_to_each_watch_on_its_own() {
    let dir = fixture("filtered");
    let (kept, claimed) = (write(dir.join("kept.txt")), write(dir.join(".claimed.yml")));
    let (_, fiber) = consumer().await;
    let filter = Filter::Predicate(Arc::new(|path, _| !path.starts_with('.')));
    let from_dir = record(&watch(&fiber, &dir, Some(filter)));
    let from_file = record(&watch(&fiber, &claimed, None));

    touch(&[&claimed, &kept], || !from_dir.lock().is_empty() && !from_file.lock().is_empty()).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(distinct(&from_dir), [kept]);
    assert_eq!(distinct(&from_file), [claimed]);
}

#[tokio::test]
async fn passes_filter_the_paths_relative_to_the_watched_directory_and_their_types() {
    let dir = fixture("relative");
    let path = write(dir.join("nested/file.txt"));
    let (_, fiber) = consumer().await;
    let tested = Arc::new(Mutex::new(Vec::new()));
    let t = tested.clone();
    let filter = Filter::Predicate(Arc::new(move |path, is_dir| {
        t.lock().push((path.to_string(), is_dir));
        true
    }));
    let seen = record(&watch(&fiber, &dir, Some(filter)));

    touch(&[&path], || !seen.lock().is_empty()).await;
    let tested = tested.lock();
    assert!(tested.contains(&("nested".into(), true)));
    assert!(tested.contains(&("nested/file.txt".into(), false)));
    assert!(tested.iter().all(|(path, _)| !path.is_empty() && !path.starts_with('/') && !path.contains('\\')));
}

#[cfg(unix)]
#[tokio::test]
async fn treats_a_symbolic_link_to_a_directory_as_a_directory() {
    let dir = fixture("symlink");
    let target = write(dir.join("target/file.ts"));
    let link = dir.join("root/linked");
    std::fs::create_dir_all(dir.join("root")).unwrap();
    std::os::unix::fs::symlink(target.parent().unwrap(), &link).unwrap();
    let (_, fiber) = consumer().await;
    let seen = record(&watch(&fiber, dir.join("root"), Some(Filter::Patterns(vec!["linked/**/*.ts".into()]))));

    touch(&[&target], || !seen.lock().is_empty()).await;
    assert_eq!(distinct(&seen), [link.join("file.ts")]);
}

#[cfg(unix)]
#[tokio::test]
async fn treats_a_file_replaced_with_a_symbolic_link_to_a_directory_as_a_directory() {
    let dir = fixture("relinked");
    let target = write(dir.join("target/file.ts"));
    let link = write(dir.join("root/linked"));
    let (_, fiber) = consumer().await;
    let seen = record(&watch(&fiber, dir.join("root"), Some(Filter::Patterns(vec!["linked/**/*.ts".into()]))));

    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(target.parent().unwrap(), &link).unwrap();
    touch(&[&target], || !seen.lock().is_empty()).await;
    assert_eq!(distinct(&seen), [link.join("file.ts")]);
}

#[tokio::test]
async fn rejects_a_filter_entry_leading_outside_the_watched_directory() {
    let dir = fixture("outside");
    let (_, fiber) = consumer().await;
    let fs = fiber.context().get::<Fs>().unwrap();
    for entry in ["../outside", "!../outside", "a/../../outside", "/absolute"] {
        let options = WatchOptions { filter: Some(Filter::Patterns(vec![entry.into()])) };
        assert!(fs.watch(&dir, options).is_err(), "{entry} is rejected");
    }
    assert_eq!(fiber.effect_count(), 0);
}

#[tokio::test]
async fn does_not_apply_filter_to_the_watched_directory() {
    let dir = fixture(".root");
    let path = write(dir.join("file.txt"));
    let (_, fiber) = consumer().await;
    let seen = record(&watch(&fiber, &dir, Some(Filter::Patterns(vec![".".into(), "!**/.*".into()]))));

    touch(&[&path], || !seen.lock().is_empty()).await;
    assert_eq!(distinct(&seen), [path]);
}

#[tokio::test]
async fn does_not_apply_filter_to_a_watched_file() {
    let path = write(fixture("watched-file").join(".parent/.file"));
    let (_, fiber) = consumer().await;
    let seen = record(&watch(&fiber, &path, Some(Filter::Patterns(vec![]))));

    touch(&[&path], || !seen.lock().is_empty()).await;
    assert_eq!(distinct(&seen), [path]);
}

#[tokio::test]
async fn runs_every_callback_registered_for_the_same_path() {
    let path = write(fixture("shared").join("shared.txt"));
    let (_, fiber) = consumer().await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    for name in ["a", "b"] {
        let s = seen.clone();
        watch(&fiber, &path, None).on_change(move |_| s.lock().push(name));
    }

    touch(&[&path], || seen.lock().contains(&"a") && seen.lock().contains(&"b")).await;
}

#[tokio::test]
async fn stops_calling_a_listener_once_the_watch_is_disposed() {
    let path = write(fixture("dispose").join("dispose.txt"));
    let (_, fiber) = consumer().await;
    let (calls, call) = counter();
    let (others, other) = counter();
    let watcher = watch(&fiber, &path, None);
    watcher.on_change(move |_| call());
    watch(&fiber, &path, None).on_change(move |_| other());
    watcher.dispose().await.unwrap();
    let before = count(&calls);

    touch(&[&path], || count(&others) > 0).await;
    assert_eq!(count(&calls), before);
}

#[tokio::test]
async fn stops_calling_a_listener_once_the_calling_fiber_is_disposed() {
    let path = write(fixture("fiber").join("fiber.txt"));
    let (root, fiber) = consumer().await;
    let (calls, call) = counter();
    let (others, other) = counter();
    let target = path.clone();
    let def = PluginDef::new(move |ctx, _| {
        let (path, call) = (target.clone(), call.clone());
        async move {
            ctx.get::<Fs>()?.watch(&path, WatchOptions::default())?.on_change(move |_| call());
            Ok(())
        }
    })
    .inject(Inject::of::<(Fs,)>());
    let inner = root.plugin_def(&def, Value::Null).unwrap();
    inner.settled().await.unwrap();
    watch(&fiber, &path, None).on_change(move |_| other());
    inner.dispose().await.unwrap();
    let before = count(&calls);

    touch(&[&path], || count(&others) > 0).await;
    assert_eq!(count(&calls), before);
}

// The ready tests run on a current-thread runtime, where the dispatch cannot
// run before the test yields.

#[tokio::test]
async fn calls_a_ready_listener_added_after_the_initial_scan_asynchronously() {
    let path = write(fixture("ready").join("ready.txt"));
    let (_, fiber) = consumer().await;
    let (calls, call) = counter();
    watch(&fiber, &path, None).on_ready(call);

    assert_eq!(count(&calls), 0);
    eventually(|| count(&calls) == 1).await;
}

#[tokio::test]
async fn calls_a_ready_listener_added_by_another_one_once() {
    let path = write(fixture("ready-nested").join("ready-nested.txt"));
    let (_, fiber) = consumer().await;
    let watcher = watch(&fiber, &path, None);
    let (calls, call) = counter();
    let w = watcher.clone();
    watcher.on_ready(move || drop(w.on_ready(call.clone())));

    eventually(|| count(&calls) == 1).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(count(&calls), 1);
}

#[tokio::test]
async fn does_not_call_a_listener_removed_by_another_one_in_the_same_dispatch() {
    let path = write(fixture("removed").join("removed.txt"));
    let (_, fiber) = consumer().await;
    let watcher = watch(&fiber, &path, None);
    let removal: Arc<Mutex<Option<Subscription>>> = Default::default();
    let r = removal.clone();
    watcher.on_ready(move || r.lock().take().unwrap().remove());
    let (calls, call) = counter();
    *removal.lock() = Some(watcher.on_ready(call));
    let (others, other) = counter();
    watcher.on_ready(other);

    eventually(|| count(&others) == 1).await;
    assert_eq!(count(&calls), 0);
}

#[tokio::test]
async fn calls_a_function_added_again_after_its_removal_once_for_ready() {
    let path = write(fixture("readded").join("readded.txt"));
    let (_, fiber) = consumer().await;
    let watcher = watch(&fiber, &path, None);
    let (calls, listener) = counter();
    watcher.on_ready(listener.clone()).remove();
    watcher.on_ready(listener);

    eventually(|| count(&calls) == 1).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(count(&calls), 1);
}

#[tokio::test]
async fn calls_a_function_added_again_in_the_same_dispatch_once_for_ready() {
    let path = write(fixture("readded-dispatch").join("readded-dispatch.txt"));
    let (_, fiber) = consumer().await;
    let watcher = watch(&fiber, &path, None);
    let (calls, listener) = counter();
    let removal: Arc<Mutex<Option<Subscription>>> = Default::default();
    let (r, w, l) = (removal.clone(), watcher.clone(), listener.clone());
    watcher.on_ready(move || {
        r.lock().take().unwrap().remove();
        drop(w.on_ready(l.clone()));
    });
    *removal.lock() = Some(watcher.on_ready(listener));

    eventually(|| count(&calls) == 1).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(count(&calls), 1);
}
