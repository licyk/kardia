use super::*;

#[tokio::test]
async fn nested_includes_scope_guest_resolution_and_hmr_to_their_own_directory() {
    let directory = tempfile::tempdir().unwrap();
    let child_dir = directory.path().join("child # %");
    std::fs::create_dir(&child_dir).unwrap();
    std::fs::write(child_dir.join("hello.wat"), include_str!("../../../examples/plugins/hello.wat")).unwrap();
    std::fs::write(child_dir.join("child.json"), r#"[{"id":"hmr","name":"hmr"},{"id":"hello","name":"./hello.wat"}]"#)
        .unwrap();
    std::fs::write(
        directory.path().join("main.json"),
        // A path carries literal names: `%`, `?` and `#` are not escapes here, so
        // the directory is written as it is on disk.
        r#"[{"id":"child","name":"include","config":{"path":"child # %/child.json"}}]"#,
    )
    .unwrap();
    let root = Context::new();
    root.set_log_buffer_size(100);
    root.plugin::<FsPlugin>(Default::default()).unwrap().settled().await.unwrap();
    let catalog = kardia::catalog();
    let graph = ModuleGraph::new(GuestFiles { wasm: WasmSource::new() });
    catalog.register_def("hmr", Hmr::plugin(graph.clone()));
    root.plugin_def(
        &Loader::plugin(Plugins { catalog, graph: graph.clone() }).inject(Inject::new().with("std:fs")),
        Value::Null,
    )
    .unwrap()
    .settled()
    .await
    .unwrap();
    let loader = root.get::<Loader>().unwrap();
    let path = kardia::fs::Url::from_file_path(directory.path().join("main.json")).unwrap();
    loader.create(json!({"id":"main","name":"include","config":{"path":path}}), None, None).await.unwrap();
    loader.settled().await;
    let guest = loader.resolve("main:child:hello").unwrap().inner_fiber().unwrap();
    guest.settled().await.unwrap();
    assert_eq!(guest.state(), FiberState::Active);
    assert_eq!(root.get::<Hmr>().unwrap().base_dir(), child_dir);
    let module = child_dir.join("hello.wat").to_string_lossy().into_owned();
    assert!(graph.loaded().contains(&module));
    assert!(root.log_buffer().iter().any(|message| message.text == "hello from wasm"));
    root.fiber().dispose().await.unwrap();
    assert!(root.log_buffer().iter().any(|message| message.text == "goodbye from wasm"));
}
