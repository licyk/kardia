use futures::StreamExt;
use kardia_core::Context;
use kardia_server::{Body, Bytes, Server, ServerConfig, ServerExt, ServerPlugin};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
struct Guard(Arc<AtomicUsize>);
impl Guard {
    fn new(active: &Arc<AtomicUsize>) -> Self {
        active.fetch_add(1, Ordering::SeqCst);
        Self(active.clone())
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
#[tokio::test]
async fn disposing_routes_cancels_running_callbacks_and_unfinished_body_sources() {
    let root = Context::new();
    root.plugin::<ServerPlugin>(ServerConfig::local(0)).unwrap().settled().await.unwrap();
    let server = root.get::<Server>().unwrap();
    server
        .get("/healthy", |_, res, _| async move {
            res.set_body("alive");
            Ok(None)
        })
        .unwrap();
    let active = Arc::new(AtomicUsize::new(0));
    let counter = active.clone();
    let pending = server
        .get("/handler", move |_, _, _| {
            let guard = Guard::new(&counter);
            async move {
                let _guard = guard;
                futures::future::pending().await
            }
        })
        .unwrap();
    let counter = active.clone();
    let streaming = server
        .get("/body", move |_, res, _| {
            let guard = Guard::new(&counter);
            async move {
                res.set_body(Body::stream(futures::stream::unfold((false, guard), |(sent, guard)| async move {
                    if sent {
                        futures::future::pending().await
                    } else {
                        Some((Ok(Bytes::from_static(b"first")), (true, guard)))
                    }
                })));
                Ok(None)
            }
        })
        .unwrap();
    let url = server.public_url();
    let request_url = url.clone();
    let request = tokio::spawn(async move { reqwest::get(format!("{request_url}/handler")).await });
    let mut body = reqwest::get(format!("{url}/body")).await.unwrap().bytes_stream();
    assert_eq!(body.next().await.unwrap().unwrap(), b"first"[..]);
    tokio::time::timeout(Duration::from_secs(2), async {
        while active.load(Ordering::SeqCst) != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    pending.dispose().await.unwrap();
    streaming.dispose().await.unwrap();
    let remaining = active.load(Ordering::SeqCst);
    assert_eq!(reqwest::get(format!("{url}/healthy")).await.unwrap().text().await.unwrap(), "alive");
    request.abort();
    let _ = request.await;
    drop(body);
    root.fiber().dispose().await.unwrap();
    assert_eq!(remaining, 0, "route disposal must await callback and source destruction");
}

#[tokio::test]
async fn disposed_middleware_and_hooks_drop_their_pending_calls() {
    for hook in [false, true] {
        let root = Context::new();
        root.plugin::<ServerPlugin>(ServerConfig::local(0)).unwrap().settled().await.unwrap();
        let server = root.get::<Server>().unwrap();
        server
            .get("/", |_, res, _| async move {
                res.set_body("alive");
                Ok(None)
            })
            .unwrap();
        let active = Arc::new(AtomicUsize::new(0));
        let counter = active.clone();
        let effect = if hook {
            server
                .on_route_request(Arc::new(move |_, _, _, _| {
                    let guard = Guard::new(&counter);
                    Box::pin(async move {
                        let _guard = guard;
                        futures::future::pending().await
                    })
                }))
                .unwrap()
        } else {
            server
                .use_middleware(kardia_server::handler(move |_, _, _| {
                    let guard = Guard::new(&counter);
                    async move {
                        let _guard = guard;
                        futures::future::pending().await
                    }
                }))
                .unwrap()
        };
        let url = server.public_url();
        let request_url = url.clone();
        let task = tokio::spawn(async move { reqwest::get(request_url).await });
        tokio::time::timeout(Duration::from_secs(2), async {
            while active.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        effect.dispose().await.unwrap();
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert_eq!(task.await.unwrap().unwrap().status(), 500);
        assert_eq!(reqwest::get(url).await.unwrap().text().await.unwrap(), "alive");
        root.fiber().dispose().await.unwrap();
    }
}
