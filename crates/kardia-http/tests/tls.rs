use kardia_core::Context;
use kardia_http::{Http, HttpConfig, HttpExt, HttpPlugin, Message, RequestConfig, WebSocketExt};
use kardia_server::{Server, ServerConfig, ServerExt, ServerPlugin, WsExt, ws_handler};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio::net::{TcpListener, TcpStream};

#[tokio::test]
async fn https_and_wss_verify_certificates_and_share_explicit_trust() {
    let target = Context::new();
    target.plugin::<ServerPlugin>(ServerConfig::local(0)).unwrap().settled().await.unwrap();
    let server = target.get::<Server>().unwrap();
    let address = server.public_url().strip_prefix("http://").unwrap().to_owned();
    server
        .get("/", |_, res, _| async move {
            res.set_body("encrypted HTTP");
            Ok(None)
        })
        .unwrap();
    server
        .ws(
            "/ws",
            Some(ws_handler(|_, accept| async move {
                let socket = accept.accept().await?;
                while let Some(message) = socket.recv().await {
                    let message = message?;
                    if message.is_close() {
                        break;
                    }
                    socket.send(message)?;
                }
                Ok(())
            })),
            json!({}),
        )
        .unwrap();
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let pem = certificate.cert.pem();
    let tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.cert.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der()).into(),
        )
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let tunnel = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted=listener.accept()=>{let(stream,_)=accepted.unwrap();let acceptor=acceptor.clone();let address=address.clone();connections.spawn(async move{if let Ok(mut tls)=acceptor.accept(stream).await{let mut plain=TcpStream::connect(address).await.unwrap();let _=tokio::io::copy_bidirectional(&mut tls,&mut plain).await;}});},
                _=connections.join_next(),if !connections.is_empty()=>{},
            }
        }
    });
    let trusted = Context::new();
    trusted
        .plugin::<HttpPlugin>(HttpConfig { root_certificates: vec![pem], ..Default::default() })
        .unwrap()
        .settled()
        .await
        .unwrap();
    let http = trusted.get::<Http>().unwrap();
    assert_eq!(
        http.request(&format!("https://localhost:{port}/"), RequestConfig::default())
            .await
            .unwrap()
            .body
            .bytes()
            .await
            .unwrap(),
        b"encrypted HTTP"[..]
    );
    let socket = http.ws(&format!("wss://localhost:{port}/ws"), RequestConfig::default()).await.unwrap();
    socket.send(Message::Text("encrypted WS".into())).unwrap();
    assert_eq!(socket.recv().await.unwrap().unwrap(), Message::Text("encrypted WS".into()));
    socket.close(None).unwrap();
    drop(socket);
    assert!(http.request(&format!("https://127.0.0.1:{port}/"), RequestConfig::default()).await.is_err());
    let untrusted = Context::new();
    untrusted.plugin::<HttpPlugin>(HttpConfig::default()).unwrap().settled().await.unwrap();
    let other = untrusted.get::<Http>().unwrap();
    assert!(other.request(&format!("https://localhost:{port}/"), RequestConfig::default()).await.is_err());
    assert!(other.ws(&format!("wss://localhost:{port}/ws"), RequestConfig::default()).await.is_err());
    tokio::time::timeout(Duration::from_secs(2), trusted.fiber().dispose()).await.unwrap().unwrap();
    untrusted.fiber().dispose().await.unwrap();
    tunnel.abort();
    let _ = tunnel.await;
    target.fiber().dispose().await.unwrap();
    assert_eq!(http.active_operations(), 0);
}
