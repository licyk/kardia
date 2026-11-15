use futures::StreamExt;
use kardia_core::{Context, Svc};
use kardia_server::{Server, ServerConfig, ServerExt, ServerPlugin, WsExt, handler, ws_handler};
use kardia_server_acl::{AclConfig, AclPlugin};
use serde_json::{Value, json};
use tokio_tungstenite::{connect_async, tungstenite::client::IntoClientRequest};

struct Fixture {
    root: Context,
    server: Svc<Server>,
}
impl Fixture {
    async fn new(config: Option<Value>) -> Self {
        let root = Context::new();
        root.plugin::<ServerPlugin>(ServerConfig::local(0)).unwrap().settled().await.unwrap();
        if let Some(config) = config {
            root.plugin::<AclPlugin>(serde_json::from_value::<AclConfig>(config).unwrap())
                .unwrap()
                .settled()
                .await
                .unwrap();
        }
        let server = root.get::<Server>().unwrap();
        Self { root, server }
    }
    fn route(&self, path: &str, options: Value, body: &'static str) {
        self.server
            .route(
                "get",
                path,
                handler(move |_, res, _| async move {
                    res.set_body(body);
                    Ok(None)
                }),
                options,
            )
            .unwrap();
    }
    async fn request(&self, path: &str, host: &str) -> (u16, String) {
        let res = reqwest::Client::new()
            .get(format!("{}{path}", self.server.public_url()))
            .header("host", host)
            .send()
            .await
            .unwrap();
        (res.status().as_u16(), res.text().await.unwrap())
    }
    async fn ws(&self, path: &str, host: &str) -> bool {
        let mut req =
            format!("{}{path}", self.server.public_url().replace("http:", "ws:")).into_client_request().unwrap();
        req.headers_mut().insert("host", host.parse().unwrap());
        match connect_async(req).await {
            Ok((mut socket, _)) => {
                socket.close(None).await.unwrap();
                let _ = socket.next().await;
                true
            }
            Err(_) => false,
        }
    }
    async fn close(self) {
        self.root.fiber().dispose().await.unwrap();
    }
}
macro_rules! case {
    ($name:ident,$config:expr,$f:ident,$body:block)=>{ #[tokio::test] async fn $name() { let $f=Fixture::new($config).await; $body $f.close().await; } };
}
case!(no_component_loaded_any_host_allowed, None, f, {
    f.route("/ping", json!({}), "pong");
    for host in ["localhost", "public.example.com"] {
        assert_eq!(f.request("/ping", host).await.0, 200);
    }
});
case!(undefined_true_any_host_allowed, Some(json!({})), f, {
    f.route("/ping", json!({}), "pong");
    for host in ["localhost", "public.example.com"] {
        assert_eq!(f.request("/ping", host).await.0, 200);
    }
});
case!(false_loopback_allowed_others_denied, Some(json!({"allowedHosts":false})), f, {
    f.route("/ping", json!({}), "pong");
    for host in ["127.0.0.1", "localhost:3000", "[::1]"] {
        assert_eq!(f.request("/ping", host).await.0, 200);
    }
    assert_eq!(f.request("/ping", "public.example.com").await.0, 404);
});
case!(string_allow_listed_loopback, Some(json!({"allowedHosts":["public.example.com"]})), f, {
    f.route("/ping", json!({}), "pong");
    for host in ["public.example.com", "localhost"] {
        assert_eq!(f.request("/ping", host).await.0, 200);
    }
    assert_eq!(f.request("/ping", "other.example.com").await.0, 404);
});
case!(subdomain_wildcard_example_com, Some(json!({"allowedHosts":[".example.com"]})), f, {
    f.route("/ping", json!({}), "pong");
    for host in ["example.com", "api.example.com", "a.b.example.com"] {
        assert_eq!(f.request("/ping", host).await.0, 200);
    }
    assert_eq!(f.request("/ping", "example.org").await.0, 404);
});
case!(host_header_with_port_is_stripped_before_matching, Some(json!({"allowedHosts":["api.example.com"]})), f, {
    f.route("/ping", json!({}), "pong");
    assert_eq!(f.request("/ping", "api.example.com:8443").await.0, 200);
});
case!(route_true_overrides_component_false, Some(json!({"allowedHosts":false})), f, {
    f.route("/local", json!({}), "local");
    f.route("/public", json!({"allowedHosts":true}), "public");
    assert_eq!(f.request("/local", "foo.com").await.0, 404);
    assert_eq!(f.request("/public", "foo.com").await.0, 200);
});
case!(route_false_overrides_component_true, Some(json!({})), f, {
    f.route("/open", json!({}), "open");
    f.route("/admin", json!({"allowedHosts":false}), "admin");
    assert_eq!(f.request("/open", "foo.com").await.0, 200);
    assert_eq!(f.request("/admin", "foo.com").await.0, 404);
    assert_eq!(f.request("/admin", "localhost").await.0, 200);
});
case!(route_list_adds_to_component_behavior, Some(json!({"allowedHosts":false})), f, {
    f.route("/partner", json!({"allowedHosts":["partner.com"]}), "partner");
    for host in ["partner.com", "localhost"] {
        assert_eq!(f.request("/partner", host).await.0, 200);
    }
    assert_eq!(f.request("/partner", "evil.com").await.0, 404);
});
case!(intercept_routes_merge_into_route_options, Some(json!({"allowedHosts":false})), f, {
    f.root
        .intercept("server", json!({"routes":{"GET /scoped":{"allowedHosts":true}}}))
        .get::<Server>()
        .unwrap()
        .get("/scoped", |_, res, _| async move {
            res.set_body("scoped");
            Ok(None)
        })
        .unwrap();
    assert_eq!(f.request("/scoped", "foo.com").await.0, 200);
});
case!(component_level_false_denies_remote_ws_allows_loopback, Some(json!({"allowedHosts":false})), f, {
    f.server.ws("/events", None, json!({})).unwrap();
    assert!(f.ws("/events", "localhost").await);
    assert!(!f.ws("/events", "evil.com").await);
});
case!(route_level_allowedhosts_for_ws, Some(json!({"allowedHosts":false})), f, {
    f.server.ws("/public-ws", None, json!({"allowedHosts":true})).unwrap();
    assert!(f.ws("/public-ws", "foo.com").await);
});
case!(vetoed_route_falls_through_to_the_next_matching_route, Some(json!({})), f, {
    f.route("/both", json!({"allowedHosts":false}), "local");
    f.route("/both", json!({}), "public");
    assert_eq!(f.request("/both", "evil.com").await, (200, "public".into()));
    assert_eq!(f.request("/both", "localhost").await, (200, "local".into()));
});
case!(vetoed_sole_route_yields_404_not_405_no_information_leak, Some(json!({"allowedHosts":false})), f, {
    f.route("/api", json!({}), "ok");
    assert_eq!(f.request("/api", "evil.com").await.0, 404);
    let res = reqwest::Client::new()
        .post(format!("{}/api", f.server.public_url()))
        .header("host", "evil.com")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);
    assert!(res.headers().get("allow").is_none());
});
case!(vetoed_ws_route_falls_through_to_next_matching_ws_route, Some(json!({})), f, {
    f.server
        .ws(
            "/both",
            Some(ws_handler(|_, accept| async move {
                accept.accept().await?.send(kardia_server::Message::Text("local".into()))?;
                Ok(())
            })),
            json!({"allowedHosts":false}),
        )
        .unwrap();
    f.server
        .ws(
            "/both",
            Some(ws_handler(|_, accept| async move {
                accept.accept().await?.send(kardia_server::Message::Text("public".into()))?;
                Ok(())
            })),
            json!({}),
        )
        .unwrap();
    assert!(f.ws("/both", "evil.com").await);
});

#[tokio::test]
async fn acl_failure_and_each_recovery_policy_leave_public_routes_running() {
    use kardia_core::{Error, FiberState, RecoveryAction, RecoveryPolicy};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    for mode in 0..3 {
        let f = Fixture::new(None).await;
        f.route("/healthy", json!({"allowedHosts":true}), "healthy");
        let fiber = f.root.plugin_value::<AclPlugin>(json!({"allowedHosts":false})).unwrap();
        fiber.settled().await.unwrap();
        let baseline = fiber.effect_count();
        let provider = f.server.impl_id();
        fiber
            .set_recovery(match mode {
                0 => RecoveryPolicy::Manual,
                1 => RecoveryPolicy::Retry { delay: Duration::from_millis(20) },
                _ => RecoveryPolicy::host(|_| async { Ok(RecoveryAction::RetryAfter(Duration::from_millis(20))) }),
            })
            .unwrap();
        let progress = Arc::new(AtomicUsize::new(0));
        let observed = progress.clone();
        let url = f.server.public_url();
        let monitor = tokio::spawn(async move {
            let client = reqwest::Client::builder().timeout(Duration::from_secs(2)).build().unwrap();
            loop {
                let response =
                    client.get(format!("{url}/healthy")).header("host", "external.com").send().await.unwrap();
                assert_eq!(response.text().await.unwrap(), "healthy");
                observed.fetch_add(1, Ordering::SeqCst);
            }
        });
        for _ in 0..3 {
            let before = progress.load(Ordering::SeqCst);
            fiber.fail(Error::msg("ACL failure")).await.unwrap_err();
            assert_eq!(fiber.effect_count(), 0);
            if mode == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
                assert_eq!(fiber.state(), FiberState::Failed);
                fiber.update(json!({"allowedHosts":false})).unwrap().await.unwrap();
            } else {
                tokio::time::timeout(Duration::from_secs(2), async {
                    while fiber.state() != FiberState::Active {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
            }
            assert!(progress.load(Ordering::SeqCst) > before);
            assert_eq!(fiber.effect_count(), baseline);
            assert_eq!(f.root.get::<Server>().unwrap().impl_id(), provider);
        }
        monitor.abort();
        let _ = monitor.await;
        f.close().await;
    }
}
