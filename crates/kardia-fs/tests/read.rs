use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::{StreamExt, stream};
use kardia_core::{Context, Error, PluginDef, RecoveryAction, RecoveryPolicy};
use kardia_fs::{
    Bytes, FileStream, Fs, FsCode, FsConfig, FsError, FsExt, FsPlugin, FsProvider, LocalFsConfig, LocalFsPlugin, Url,
};
use serde_json::{Value, json};

struct Text(&'static str);
impl FsProvider for Text {
    fn read(&self, _: Url) -> FileStream {
        stream::iter([Ok(Bytes::from_static(self.0.as_bytes()))]).boxed()
    }
}
async fn setup() -> Context {
    let root = Context::new();
    root.plugin::<FsPlugin>(FsConfig::default()).unwrap().settled().await.unwrap();
    root
}
async fn until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn resolves_base_urls_in_config_and_outer_to_inner_intercepts() {
    let root = Context::new();
    root.plugin::<FsPlugin>(FsConfig { base_url: Some(Url::parse("memory:///base/").unwrap()) })
        .unwrap()
        .settled()
        .await
        .unwrap();
    let ctx = root.intercept("fs", json!({"baseUrl":"outer/"})).intercept("fs", json!({"baseUrl":"../inner/"}));
    let fs = ctx.get::<Fs>().unwrap();
    assert_eq!(fs.base_url().unwrap().as_str(), "memory:///base/inner/");
    assert_eq!(fs.resolve("a%20b").unwrap().as_str(), "memory:///base/inner/a%20b");
    assert_eq!(fs.resolve("https://example.com/file").unwrap().as_str(), "https://example.com/file");
    root.fiber().dispose().await.unwrap();
}
#[tokio::test]
async fn longest_prefix_routes_each_new_read_and_mounts_are_owned_effects() {
    let root = setup().await;
    let fs = root.get::<Fs>().unwrap();
    let file = fs.file("memory:///special/file").unwrap();
    fs.mount("memory:", Arc::new(Text("root"))).unwrap();
    let special = fs.mount("memory:///special", Arc::new(Text("special"))).unwrap();
    assert_eq!(file.text().await.unwrap(), "special");
    assert!(fs.mount("memory:///special/", Arc::new(Text("duplicate"))).is_err());
    special.dispose().await.unwrap();
    assert_eq!(file.text().await.unwrap(), "root");
    root.fiber().dispose().await.unwrap();
}
#[tokio::test]
async fn local_provider_reads_real_files_and_maps_read_failures() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("fs-read-files");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("测试.txt"), "\u{feff}hello").unwrap();
    let root = setup().await;
    root.plugin::<LocalFsPlugin>(LocalFsConfig::default()).unwrap().settled().await.unwrap();
    let fs = root.get::<Fs>().unwrap();
    let base = Url::from_directory_path(&dir).unwrap();
    assert_eq!(fs.file(base.join("测试.txt").unwrap().as_str()).unwrap().text().await.unwrap(), "hello");
    for (path, code) in [
        ("missing", FsCode::NotFound),
        ("./", FsCode::IsADirectory),
        ("测试.txt/child", if cfg!(windows) { FsCode::NotFound } else { FsCode::NotADirectory }),
        ("%2Fbad", FsCode::InvalidPath),
    ] {
        let error = fs.file(base.join(path).unwrap().as_str()).unwrap().bytes().await.unwrap_err();
        assert_eq!(error.downcast_ref::<FsError>().unwrap().code, code);
    }
    root.fiber().dispose().await.unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

struct Guard(Arc<AtomicUsize>);
impl Drop for Guard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
struct Pending(Arc<AtomicUsize>);
impl FsProvider for Pending {
    fn read(&self, _: Url) -> FileStream {
        self.0.fetch_add(1, Ordering::SeqCst);
        let guard = Guard(self.0.clone());
        stream::unfold(guard, |guard| async {
            std::future::pending::<()>().await;
            Some((Ok(Bytes::new()), guard))
        })
        .boxed()
    }
}
#[tokio::test]
async fn cancelled_and_disposed_readers_release_provider_resources() {
    let root = setup().await;
    let open = Arc::new(AtomicUsize::new(0));
    let mount = root.get::<Fs>().unwrap().mount("pending:", Arc::new(Pending(open.clone()))).unwrap();
    let consumer = root.plugin_def(&PluginDef::new(|_, _| async { Ok(()) }).inject(["fs"]), Value::Null).unwrap();
    consumer.settled().await.unwrap();
    let file = consumer.context().get::<Fs>().unwrap().file("pending:///file").unwrap();
    let reader = file.stream().unwrap();
    assert_eq!(open.load(Ordering::SeqCst), 1);
    drop(reader);
    until(|| open.load(Ordering::SeqCst) == 0).await;
    let mut reader = file.stream().unwrap();
    consumer.dispose().await.unwrap();
    assert_eq!(open.load(Ordering::SeqCst), 0);
    assert!(reader.next().await.is_none());
    let mut reader = root.get::<Fs>().unwrap().file("pending:///file").unwrap().stream().unwrap();
    mount.dispose().await.unwrap();
    let error = reader.next().await.unwrap().unwrap_err();
    assert_eq!(error.downcast_ref::<FsError>().unwrap().code, FsCode::NoProvider);
    until(|| open.load(Ordering::SeqCst) == 0).await;
    root.fiber().dispose().await.unwrap();
}

struct Panic(bool);
impl FsProvider for Panic {
    fn read(&self, _: Url) -> FileStream {
        assert!(!self.0, "provider invocation panic");
        stream::once(async {
            tokio::task::yield_now().await;
            panic!("provider poll panic");
            #[allow(unreachable_code)]
            Ok(Bytes::new())
        })
        .boxed()
    }
}
#[tokio::test]
async fn provider_panics_are_errors_and_other_mounts_keep_working() {
    let root = setup().await;
    let fs = root.get::<Fs>().unwrap();
    fs.mount("safe:", Arc::new(Text("healthy"))).unwrap();
    for (scheme, sync) in [("sync:", true), ("async:", false)] {
        fs.mount(scheme, Arc::new(Panic(sync))).unwrap();
        let error = fs.file(&format!("{scheme}///file")).unwrap().bytes().await.unwrap_err();
        assert!(error.downcast_ref::<kardia_core::Panicked>().is_some());
        assert_eq!(fs.file("safe:///file").unwrap().text().await.unwrap(), "healthy");
    }
    root.fiber().dispose().await.unwrap();
}
#[tokio::test]
async fn mount_owner_recovery_leaves_an_independent_provider_available() {
    for mode in 0..3 {
        let root = setup().await;
        let fs = root.get::<Fs>().unwrap();
        fs.mount("safe:", Arc::new(Text("healthy"))).unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let count = attempts.clone();
        let def = PluginDef::new(move |ctx, _| {
            let n = count.fetch_add(1, Ordering::SeqCst);
            async move {
                ctx.get::<Fs>()?.mount("recover:", Arc::new(Text("recovered")))?;
                if n == 0 { Err(Error::msg("provider failed")) } else { Ok(()) }
            }
        })
        .inject(["fs"]);
        let fiber = root.plugin_def(&def, Value::Null).unwrap();
        let _ = fiber.settled().await;
        fiber
            .set_recovery(match mode {
                0 => RecoveryPolicy::Manual,
                1 => RecoveryPolicy::Retry { delay: Duration::from_millis(5) },
                _ => RecoveryPolicy::host(|_| async { Ok(RecoveryAction::RetryAfter(Duration::from_millis(5))) }),
            })
            .unwrap();
        if mode == 0 {
            assert_eq!(fs.file("safe:///file").unwrap().text().await.unwrap(), "healthy");
            assert_eq!(attempts.load(Ordering::SeqCst), 1);
            fiber.update(Value::Null).unwrap().await.unwrap();
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while fiber.state() != kardia_core::FiberState::Active {
                assert_eq!(fs.file("safe:///file").unwrap().text().await.unwrap(), "healthy");
            }
        })
        .await
        .unwrap();
        assert_eq!(fs.file("recover:///file").unwrap().text().await.unwrap(), "recovered");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        root.fiber().dispose().await.unwrap();
    }
}
