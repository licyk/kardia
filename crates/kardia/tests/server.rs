use futures::{SinkExt, StreamExt};
use kardia::{
    Context, json,
    loader::Loader,
    server::{Server, ServerExt},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[tokio::test]
async fn catalog_loads_all_server_packages_and_rebuilds_proxy_after_provider_replacement() {
    let root = Context::new();
    let catalog = kardia::catalog();
    for name in [
        "@cordisjs/server",
        "@cordisjs/server-acl",
        "@cordisjs/server-echo",
        "@cordisjs/server-static",
        "@cordisjs/server-proxy",
        "@cordisjs/http",
        "@cordisjs/fs",
        "@cordisjs/fs-node",
    ] {
        assert!(catalog.get(name).is_some(), "{name}");
    }
    root.plugin_def(&Loader::plugin(catalog), json!(null)).unwrap().settled().await.unwrap();
    let loader = root.get::<Loader>().unwrap();
    for entry in [
        json!({"id":"fs","name":"@cordisjs/fs"}),
        json!({"id":"local","name":"@cordisjs/fs-node"}),
        json!({"id":"http","name":"@cordisjs/http"}),
        json!({"id":"server","name":"@cordisjs/server","config":{"port":0}}),
        json!({"id":"acl","name":"@cordisjs/server-acl","config":{"allowedHosts":false}}),
        json!({"id":"echo","name":"@cordisjs/server-echo","intercept":{"std:server":{"path":"/echo"}}}),
    ] {
        loader.create(entry, None, None).await.unwrap();
    }
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(folder.path().join("index.html"), "<h1>static</h1>").unwrap();
    loader.create(json!({"id":"static","name":"@cordisjs/server-static","config":{"root":kardia::fs::Url::from_directory_path(folder.path()).unwrap()},"intercept":{"std:server":{"path":"/static"}}}),None,None).await.unwrap();
    let server = root.get::<Server>().unwrap();
    let url = server.public_url();
    // A target is a prefix that ends with `/`.
    loader.create(json!({"id":"proxy","name":"@cordisjs/server-proxy","config":{"target":format!("{url}/")},"intercept":{"std:server":{"path":"/proxy"}},"recovery":{"mode":"manual"}}),None,None).await.unwrap();
    loader.settled().await;
    // The proxy forwards under its intercept; it has no public URL of its own.
    assert_eq!(root.get::<Server>().unwrap().public_url(), url);
    let client = reqwest::Client::new();
    assert_eq!(client.get(format!("{url}/static/")).send().await.unwrap().text().await.unwrap(), "<h1>static</h1>");
    assert_eq!(
        client.post(format!("{url}/proxy/echo")).body("echo").send().await.unwrap().text().await.unwrap(),
        "echo"
    );
    assert_eq!(client.post(format!("{url}/echo")).header("host", "denied.example").send().await.unwrap().status(), 404);
    let (mut ws, _) = connect_async(format!("{}/proxy/echo", url.replace("http:", "ws:"))).await.unwrap();
    ws.send(Message::Binary(vec![0, 255].into())).await.unwrap();
    assert_eq!(ws.next().await.unwrap().unwrap(), Message::Binary(vec![0, 255].into()));
    ws.close(None).await.unwrap();
    drop(ws);
    let http = loader.resolve("http").unwrap().inner_fiber().unwrap();
    http.update_with(json!({}), true).unwrap().await.unwrap();
    loader.resolve("proxy").unwrap().inner_fiber().unwrap().settled().await.unwrap();
    assert_eq!(
        client.post(format!("{url}/proxy/echo")).body("again").send().await.unwrap().text().await.unwrap(),
        "again"
    );
    loader
        .resolve("server")
        .unwrap()
        .inner_fiber()
        .unwrap()
        .update_with(json!({"port":server.address().port()}), true)
        .unwrap()
        .await
        .unwrap();
    loader.resolve("proxy").unwrap().inner_fiber().unwrap().settled().await.unwrap();
    assert_ne!(root.get::<Server>().unwrap().impl_id(), server.impl_id());
    assert_eq!(
        client.post(format!("{url}/proxy/echo")).body("rebuilt").send().await.unwrap().text().await.unwrap(),
        "rebuilt"
    );
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn cli_runs_the_documented_server_configuration() {
    let reservation = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = reservation.local_addr().unwrap().port();
    drop(reservation);
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("server.yml");
    std::fs::create_dir(directory.path().join("server-public")).unwrap();
    std::fs::write(
        directory.path().join("server-public/index.html"),
        include_str!("../../../examples/server-public/index.html"),
    )
    .unwrap();
    std::fs::write(&config, include_str!("../../../examples/server.yml").replace("5140", &port.to_string())).unwrap();
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_kardia"))
        .arg(config)
        .current_dir(workspace)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap();
    let mut process = Process(child);
    let url = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(1)).build().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            assert!(process.0.try_wait().unwrap().is_none(), "CLI exited during startup");
            if let Ok(response) = client.get(format!("{url}/static/")).send().await {
                if response.status() == 200 && response.text().await.unwrap().contains("Kardia Server") {
                    let proxy = client.post(format!("{url}/proxy/echo")).body("ready").send().await.unwrap();
                    if proxy.status() == 200 && proxy.text().await.unwrap() == "ready" {
                        break;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(client.post(format!("{url}/proxy/echo")).body("CLI").send().await.unwrap().text().await.unwrap(), "CLI");
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{port}/proxy/echo")).await.unwrap();
    ws.send(Message::Text("CLI".into())).await.unwrap();
    assert_eq!(ws.next().await.unwrap().unwrap(), Message::Text("CLI".into()));
    ws.close(None).await.unwrap();
}
struct Process(std::process::Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
