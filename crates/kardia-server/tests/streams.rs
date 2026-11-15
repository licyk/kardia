use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::{StreamExt, stream};
use kardia_core::Context;
use kardia_server::{Blob, Body, Bytes, FormData, FormEntry, FormValue, Server, ServerConfig, ServerExt, ServerPlugin};

struct Closed(Arc<AtomicBool>);
impl Drop for Closed {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn delivers_first_chunk_without_waiting_for_the_rest_and_drops_cancelled_source() {
    let root = Context::new();
    root.plugin::<ServerPlugin>(ServerConfig::local(0)).unwrap().settled().await.unwrap();
    let server = root.get::<Server>().unwrap();
    let released = Arc::new(tokio::sync::Notify::new());
    let closed = Arc::new(AtomicBool::new(false));
    let release = released.clone();
    let close = closed.clone();
    server
        .get("/stream", move |_, res, _| {
            let release = release.clone();
            let guard = Closed(close.clone());
            async move {
                let source = stream::unfold((false, guard, release), |(sent, guard, release)| async move {
                    if sent {
                        release.notified().await;
                    }
                    Some((Ok(Bytes::from_static(b"chunk")), (true, guard, release)))
                });
                res.set_body(Body::stream(source));
                Ok(None)
            }
        })
        .unwrap();
    let client = reqwest::Client::builder().timeout(Duration::from_secs(2)).build().unwrap();
    let response = client.get(format!("{}/stream", server.public_url())).send().await.unwrap();
    let mut stream = response.bytes_stream();
    assert_eq!(tokio::time::timeout(Duration::from_secs(2), stream.next()).await.unwrap().unwrap().unwrap(), "chunk");
    assert!(!closed.load(Ordering::SeqCst));
    drop(stream);
    tokio::time::timeout(Duration::from_secs(2), async {
        while !closed.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    root.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn response_form_and_blob_readers_keep_the_sendable_body() {
    let form = FormData(vec![
        FormEntry { name: "text".into(), value: FormValue::Text("两行\ntext".into()) },
        FormEntry {
            name: "file".into(),
            value: FormValue::File {
                filename: "测试.bin".into(),
                blob: Blob { bytes: Bytes::from_static(&[0, 1, 255]), content_type: "application/octet-stream".into() },
            },
        },
    ]);
    let res = kardia_server::Response::new();
    res.set_form_data(form);
    let read = res.form_data().await.unwrap();
    assert_eq!(read.0[0].value, FormValue::Text("两行\r\ntext".into()));
    assert_eq!(read.0[1].name, "file");
    let sent = res.bytes().await.unwrap();
    assert!(sent.ends_with(b"--\r\n"));
    res.set_blob(Blob { bytes: Bytes::from_static(&[0, 255]), content_type: "IMAGE/PNG".into() });
    assert_eq!(res.blob().await.unwrap().content_type, "image/png");
    assert_eq!(res.bytes().await.unwrap(), Bytes::from_static(&[0, 255]));
}

#[tokio::test]
async fn dropping_both_tee_readers_releases_source_without_draining() {
    let closed = Arc::new(AtomicBool::new(false));
    let guard = Closed(closed.clone());
    let mut body = Body::stream(stream::unfold(guard, |guard| async move {
        std::future::pending::<()>().await;
        Some((Ok(Bytes::new()), guard))
    }));
    let second = body.fork();
    drop(body);
    assert!(!closed.load(Ordering::SeqCst));
    drop(second);
    assert!(closed.load(Ordering::SeqCst));
}
