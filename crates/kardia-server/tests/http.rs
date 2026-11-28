use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::stream;
use kardia_core::{Context, Error, Svc};
use kardia_server::{
    Body, Bytes, HeaderName, HeaderValue, Path, Response, Server, ServerConfig, ServerExt, ServerPlugin, StatusCode,
    ValidationError, handler,
};
use serde_json::{Value, json};

struct Fixture {
    root: Context,
    server: Svc<Server>,
    client: reqwest::Client,
}
impl Fixture {
    async fn new(config: ServerConfig) -> Self {
        Self::try_new(config).await.expect("the server starts")
    }
    async fn try_new(config: ServerConfig) -> kardia_core::Result<Self> {
        let root = Context::new();
        root.plugin::<ServerPlugin>(config)?.settled().await?;
        let server = root.get::<Server>()?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .unwrap();
        Ok(Self { root, server, client })
    }
    async fn get(&self, path: &str) -> reqwest::Response {
        self.client.get(format!("http://{}{}", self.server.address(), path)).send().await.unwrap()
    }
    async fn close(self) {
        self.root.fiber().dispose().await.unwrap();
    }
}

macro_rules! case {
    ($name:ident, $f:ident, $body:block) => {
        #[tokio::test] async fn $name() {
            let $f = Fixture::new(ServerConfig::local(0)).await;
            $body
            $f.close().await;
        }
    };
}

case!(should_start_and_listen, f, {
    assert_eq!(f.get("/").await.status(), 404);
});
case!(should_handle_get_route, f, {
    f.server
        .get("/hello", |_, res, _| async move {
            res.set_body("world");
            Ok(None)
        })
        .unwrap();
    assert_eq!(f.get("/hello").await.text().await.unwrap(), "world");
});
case!(should_handle_post_route, f, {
    f.server
        .post("/echo", |req, res, _| async move {
            res.set_body(req.text().await?);
            Ok(None)
        })
        .unwrap();
    let res = f.client.post(format!("{}/echo", f.server.public_url())).body("hello").send().await.unwrap();
    assert_eq!(res.text().await.unwrap(), "hello");
});
case!(should_handle_route_returning_response_object, f, {
    f.server
        .get("/response", |_, _, _| async {
            let res = Response::new();
            res.set_json(&json!({"ok":true}))?;
            Ok(Some(res))
        })
        .unwrap();
    assert_eq!(f.get("/response").await.json::<Value>().await.unwrap(), json!({"ok":true}));
});
case!(should_extract_typed_path_params, f, {
    f.server
        .get("/users/:id", |req, res, _| async move {
            res.set_body(req.params["id"].as_str().unwrap().to_owned());
            Ok(None)
        })
        .unwrap();
    assert_eq!(f.get("/users/42").await.text().await.unwrap(), "42");
});
case!(should_decode_path_params, f, {
    f.server
        .get("/files/:name", |req, res, _| async move {
            res.set_json(&req.params)?;
            Ok(None)
        })
        .unwrap();
    assert_eq!(f.get("/files/hello%20world").await.json::<Value>().await.unwrap(), json!({"name":"hello world"}));
    assert_eq!(f.get("/files/a+b%2Fc").await.json::<Value>().await.unwrap(), json!({"name":"a+b/c"}));
    assert_eq!(f.get("/files/%ZZ").await.status(), 400);
});
case!(should_handle_wildcard_path_params, f, {
    f.server
        .get("{/*path}", |req, res, _| async move {
            res.set_json(&req.params)?;
            Ok(None)
        })
        .unwrap();
    assert_eq!(f.get("/a/b/c").await.json::<Value>().await.unwrap(), json!({"path":"a/b/c"}));
    assert_eq!(f.get("/").await.json::<Value>().await.unwrap(), json!({}));
});
case!(should_match_route_with_query_string, f, {
    f.server
        .get("/search", |req, res, _| async move {
            res.set_json(&req.query)?;
            Ok(None)
        })
        .unwrap();
    assert_eq!(
        f.get("/search?q=hello&q=again").await.json::<Value>().await.unwrap(),
        json!([["q", "hello"], ["q", "again"]])
    );
});
case!(should_not_leak_query_string_into_path_params, f, {
    f.server
        .get("{/*path}", |req, res, _| async move {
            res.set_json(&req.params)?;
            Ok(None)
        })
        .unwrap();
    assert_eq!(f.get("/a/b?foo=bar").await.json::<Value>().await.unwrap(), json!({"path":"a/b"}));
});
case!(should_keep_a_question_mark_inside_the_query_string, f, {
    f.server
        .get("/search", |req, res, _| async move {
            res.set_json(&json!({ "path": req.path, "query": req.query }))?;
            Ok(None)
        })
        .unwrap();
    assert_eq!(
        f.get("/search?a=1?b=2").await.json::<Value>().await.unwrap(),
        json!({ "path": "/search", "query": [["a", "1?b=2"]] })
    );
});
case!(should_handle_regex_routes, f, {
    f.server
        .get(Path::regex(r"^/re/(\d+)$", "").unwrap(), |req, res, _| async move {
            res.set_json(&req.params)?;
            Ok(None)
        })
        .unwrap();
    assert_eq!(f.get("/re/123").await.json::<Value>().await.unwrap(), json!(["/re/123", "123"]));
});
case!(should_support_all_to_match_any_method, f, {
    f.server
        .all("/any", |req, res, _| async move {
            res.set_body(req.method.to_string());
            Ok(None)
        })
        .unwrap();
    assert_eq!(f.get("/any").await.text().await.unwrap(), "GET");
    assert_eq!(
        f.client.post(format!("{}/any", f.server.public_url())).send().await.unwrap().text().await.unwrap(),
        "POST"
    );
});
case!(should_return_404_for_unmatched_path, f, {
    assert_eq!(f.get("/nonexistent").await.status(), 404);
});
case!(should_return_405_with_allow_header_for_mismatched_method, f, {
    f.server
        .get("/only-get", |_, res, _| async move {
            res.set_body("ok");
            Ok(None)
        })
        .unwrap();
    let res = f.client.post(format!("{}/only-get", f.server.public_url())).send().await.unwrap();
    assert_eq!(res.status(), 405);
    assert_eq!(res.headers()["allow"], "get");
});
case!(should_treat_all_as_wildcard_method_in_allow_header, f, {
    f.server.all("/any", |_, _, next| next).unwrap();
    let res =
        f.client.request(reqwest::Method::OPTIONS, format!("{}/any", f.server.public_url())).send().await.unwrap();
    assert_eq!(res.status(), 204);
    assert_eq!(res.headers()["allow"], "*");
});
case!(should_handle_options_with_allow_header, f, {
    f.server
        .get("/resource", |_, res, _| async move {
            res.set_body("ok");
            Ok(None)
        })
        .unwrap();
    f.server
        .post("/resource", |_, res, _| async move {
            res.set_body("ok");
            Ok(None)
        })
        .unwrap();
    let res =
        f.client.request(reqwest::Method::OPTIONS, format!("{}/resource", f.server.public_url())).send().await.unwrap();
    assert_eq!(res.status(), 204);
    assert_eq!(res.headers()["allow"], "get, post");
});
case!(should_allow_setting_custom_status_code, f, {
    f.server
        .get("/created", |_, res, _| async move {
            res.set_status(StatusCode::CREATED);
            res.set_body("created");
            Ok(None)
        })
        .unwrap();
    assert_eq!(f.get("/created").await.status(), 201);
});
case!(should_preserve_status_only_response_redirect, f, {
    f.server
        .get("/redirect", |_, res, _| async move {
            res.set_status(StatusCode::FOUND);
            res.set_header(http::header::LOCATION, HeaderValue::from_static("/new"));
            Ok(None)
        })
        .unwrap();
    let res = f.get("/redirect").await;
    assert_eq!(res.status(), 302);
    assert_eq!(res.headers()["location"], "/new");
});
case!(should_preserve_204_empty_response, f, {
    f.server
        .get("/no-content", |_, res, _| async move {
            res.set_status(StatusCode::NO_CONTENT);
            Ok(None)
        })
        .unwrap();
    assert_eq!(f.get("/no-content").await.status(), 204);
});
case!(should_preserve_custom_404_body, f, {
    f.server
        .get("/nf", |_, res, _| async move {
            res.set_status(StatusCode::NOT_FOUND);
            res.set_text("custom not found");
            Ok(None)
        })
        .unwrap();
    let res = f.get("/nf").await;
    assert_eq!(res.status(), 404);
    assert_eq!(res.text().await.unwrap(), "custom not found");
});
case!(should_run_middleware_before_routes, f, {
    let order = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let first = order.clone();
    f.server
        .use_middleware(handler(move |_, _, next| {
            first.lock().push("middleware");
            next
        }))
        .unwrap();
    let second = order.clone();
    f.server
        .get("/test", move |_, res, _| {
            second.lock().push("route");
            async move {
                res.set_body("ok");
                Ok(None)
            }
        })
        .unwrap();
    f.get("/test").await;
    assert_eq!(*order.lock(), ["middleware", "route"]);
});
case!(should_allow_middleware_to_short_circuit, f, {
    f.server
        .use_middleware(handler(|_, res, _| async move {
            res.set_status(StatusCode::FORBIDDEN);
            res.set_body("forbidden");
            Ok(None)
        }))
        .unwrap();
    f.server.get("/test", |_, _, _| async { panic!("unreachable") }).unwrap();
    assert_eq!(f.get("/test").await.status(), 403);
});
case!(should_propagate_a_response_returned_via_next_so_catch_alls_can_forward_it, f, {
    f.server
        .get("{/*path}", |_, res, next| async move {
            let result = next.await?;
            if result.is_none() && !res.claimed() {
                res.set_text("<fallback/>");
            }
            Ok(result)
        })
        .unwrap();
    f.server
        .get("/api/thing", |_, _, _| async {
            let res = Response::new();
            res.set_json(&json!({"ok":true}))?;
            Ok(Some(res))
        })
        .unwrap();
    assert_eq!(f.get("/api/thing").await.json::<Value>().await.unwrap(), json!({"ok":true}));
});
case!(should_parse_json_body, f, {
    f.server
        .post("/json", |req, res, _| async move {
            res.set_json(&req.json().await?)?;
            Ok(None)
        })
        .unwrap();
    let res =
        f.client.post(format!("{}/json", f.server.public_url())).json(&json!({"foo":"bar"})).send().await.unwrap();
    assert_eq!(res.json::<Value>().await.unwrap(), json!({"foo":"bar"}));
});
case!(should_parse_text_body, f, {
    f.server
        .post("/text", |req, res, _| async move {
            res.set_body(req.text().await?.to_uppercase());
            Ok(None)
        })
        .unwrap();
    let res = f.client.post(format!("{}/text", f.server.public_url())).body("hello").send().await.unwrap();
    assert_eq!(res.text().await.unwrap(), "HELLO");
});

fn validator(value: Value) -> std::result::Result<String, ValidationError> {
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| ValidationError { issues: vec![json!({"message":"expected string","path":[]})] })
}
case!(should_pass_validated_body_through, f, {
    f.server
        .post("/v", |req, res, _| async move {
            res.set_body(req.validate_json(async |v| validator(v)).await?.to_uppercase());
            Ok(None)
        })
        .unwrap();
    let res = f.client.post(format!("{}/v", f.server.public_url())).json(&"hello").send().await.unwrap();
    assert_eq!(res.text().await.unwrap(), "HELLO");
});
case!(should_respond_422_with_issues_on_validation_failure, f, {
    f.server
        .post("/v", |req, res, _| async move {
            res.set_body(req.validate_json(async |v| validator(v)).await?);
            Ok(None)
        })
        .unwrap();
    let res = f.client.post(format!("{}/v", f.server.public_url())).json(&json!({})).send().await.unwrap();
    assert_eq!(res.status(), 422);
    assert_eq!(res.json::<Value>().await.unwrap()["issues"][0]["message"], "expected string");
});
case!(should_support_async_validators, f, {
    f.server
        .post("/a", |req, res, _| async move {
            let n = req
                .validate_json(async |v| {
                    tokio::task::yield_now().await;
                    v.as_i64().ok_or_else(|| ValidationError { issues: vec![json!({"message":"expected number"})] })
                })
                .await?;
            res.set_body((n * 2).to_string());
            Ok(None)
        })
        .unwrap();
    assert_eq!(
        f.client.post(format!("{}/a", f.server.public_url())).json(&21).send().await.unwrap().text().await.unwrap(),
        "42"
    );
    assert_eq!(f.client.post(format!("{}/a", f.server.public_url())).json(&"nope").send().await.unwrap().status(), 422);
});
case!(should_set_custom_response_headers, f, {
    f.server
        .get("/headers", |_, res, _| async move {
            res.set_header(HeaderName::from_static("x-custom"), HeaderValue::from_static("value"));
            res.set_body("ok");
            Ok(None)
        })
        .unwrap();
    assert_eq!(f.get("/headers").await.headers()["x-custom"], "value");
});
case!(should_auto_derive_content_type_from_res_text, f, {
    f.server
        .get("/t", |_, res, _| async move {
            res.set_text("hi");
            Ok(None)
        })
        .unwrap();
    assert_eq!(f.get("/t").await.headers()["content-type"], "text/plain;charset=UTF-8");
});
case!(should_set_content_type_application_json_for_res_json, f, {
    f.server
        .get("/j", |_, res, _| async move {
            res.set_json(&json!({"a":1}))?;
            Ok(None)
        })
        .unwrap();
    assert_eq!(f.get("/j").await.headers()["content-type"], "application/json");
});
case!(should_preserve_user_content_type_when_set_before_body, f, {
    f.server
        .get("/a", |_, res, _| async move {
            res.set_header(http::header::CONTENT_TYPE, HeaderValue::from_static("text/html"));
            res.set_text("<p>hi</p>");
            Ok(None)
        })
        .unwrap();
    assert_eq!(f.get("/a").await.headers()["content-type"], "text/html");
});
case!(should_preserve_user_content_type_when_set_after_body, f, {
    f.server
        .get("/b", |_, res, _| async move {
            res.set_text("<p>hi</p>");
            res.set_header(http::header::CONTENT_TYPE, HeaderValue::from_static("text/html"));
            Ok(None)
        })
        .unwrap();
    assert_eq!(f.get("/b").await.headers()["content-type"], "text/html");
});
case!(should_overwrite_body_on_subsequent_writes, f, {
    f.server
        .get("/overwrite", |_, res, _| async move {
            res.set_text("first");
            res.set_text("second");
            Ok(None)
        })
        .unwrap();
    assert_eq!(f.get("/overwrite").await.text().await.unwrap(), "second");
});
fn chunks() -> Body {
    Body::stream(stream::iter(["hello", " ", "world"].map(|s| Ok(Bytes::from_static(s.as_bytes())))))
}
case!(should_allow_method_read_then_flush_via_end, f, {
    f.server
        .get("/stream", |_, res, _| async move {
            res.set_body(chunks());
            assert_eq!(res.text().await?, "hello world");
            Ok(None)
        })
        .unwrap();
    assert_eq!(f.get("/stream").await.text().await.unwrap(), "hello world");
});
case!(should_allow_two_concurrent_method_reads_on_the_same_stream_body, f, {
    f.server
        .get("/stream", |_, res, _| async move {
            res.set_body(chunks());
            let (a, b) = tokio::try_join!(res.text(), res.text())?;
            assert_eq!(a, b);
            assert_eq!(a, "hello world");
            res.set_text("done");
            Ok(None)
        })
        .unwrap();
    assert_eq!(f.get("/stream").await.text().await.unwrap(), "done");
});
case!(should_allow_body_getter_read_then_flush_via_end, f, {
    f.server
        .get("/stream", |_, res, _| async move {
            res.set_body(chunks());
            assert_eq!(res.body().bytes().await?, "hello world");
            Ok(None)
        })
        .unwrap();
    assert_eq!(f.get("/stream").await.text().await.unwrap(), "hello world");
});
case!(should_remove_route_on_dispose, f, {
    let route = f
        .server
        .get("/temp", |_, res, _| async move {
            res.set_body("exists");
            Ok(None)
        })
        .unwrap();
    assert_eq!(f.get("/temp").await.status(), 200);
    route.dispose().await.unwrap();
    assert_eq!(f.get("/temp").await.status(), 404);
});
case!(should_compute_public_url_from_host_and_port, f, {
    assert_eq!(f.server.public_url(), format!("http://{}", f.server.address()));
});
case!(should_return_500_when_route_throws, f, {
    f.server.get("/error", |_, _, _| async { Err(Error::msg("test error")) }).unwrap();
    assert_eq!(f.get("/error").await.status(), 500);
});
case!(should_not_override_status_if_already_set_before_throw, f, {
    f.server
        .get("/error", |_, res, _| async move {
            res.set_status(StatusCode::SERVICE_UNAVAILABLE);
            Err(Error::msg("test error"))
        })
        .unwrap();
    assert_eq!(f.get("/error").await.status(), 503);
});
case!(should_scope_route_under_intercept_path, f, {
    f.root
        .intercept("server", json!({"path":"/api"}))
        .get::<Server>()
        .unwrap()
        .get("/data", |_, res, _| async move {
            res.set_body("scoped");
            Ok(None)
        })
        .unwrap();
    assert_eq!(f.get("/api/data").await.text().await.unwrap(), "scoped");
    assert_eq!(f.get("/data").await.status(), 404);
});
case!(should_expose_per_route_options_to_server_route_request_listeners, f, {
    f.server
        .on_route_request(Arc::new(|_, res, route, _| {
            Box::pin(async move {
                res.set_json(&route.options)?;
                Ok(None)
            })
        }))
        .unwrap();
    f.server.route("get", "/opts", handler(|_, _, _| async { panic!("unreachable") }), json!({"tag":"hello"})).unwrap();
    assert_eq!(f.get("/opts").await.json::<Value>().await.unwrap(), json!({"tag":"hello"}));
});
case!(should_allow_route_request_listener_to_short_circuit_before_callback, f, {
    let reached = Arc::new(AtomicBool::new(false));
    let seen = reached.clone();
    f.server
        .on_route_request(Arc::new(|_, res, _, _| {
            Box::pin(async move {
                res.set_status(StatusCode::UNAUTHORIZED);
                Ok(None)
            })
        }))
        .unwrap();
    f.server
        .get("/guard", move |_, _, _| {
            seen.store(true, Ordering::SeqCst);
            async { Ok(None) }
        })
        .unwrap();
    assert_eq!(f.get("/guard").await.status(), 401);
    assert!(!reached.load(Ordering::SeqCst));
});

macro_rules! override_case {
    ($name:ident, $options:expr, $meta:expr, $expected:expr) => { case!($name, f, {
        f.server.on_route_request(Arc::new(|_,res,route,_| Box::pin(async move { res.set_json(&route.options)?; Ok(None) }))).unwrap();
        let scoped = f.root.intercept("server", json!({"routes":{"GET /test":$meta}})).get::<Server>().unwrap();
        scoped.route("get", "/test", handler(|_,_,_| async { Ok(None) }), $options).unwrap();
        assert_eq!(f.get("/test").await.json::<Value>().await.unwrap(),$expected);
    }); };
}
override_case!(
    intercept_routes_should_override_per_route_options_on_same_key,
    json!({"tags":["a","b"]}),
    json!({"tags":["b","c"]}),
    json!({"tags":["b","c"]})
);
override_case!(
    intercept_scalar_should_override_per_route_scalar,
    json!({"mode":"lax"}),
    json!({"mode":"strict"}),
    json!({"mode":"strict"})
);
override_case!(
    per_route_keys_absent_from_intercept_are_preserved,
    json!({"a":1}),
    json!({"b":2}),
    json!({"a":1,"b":2})
);
case!(intercept_key_is_method_specific, f, {
    f.server
        .on_route_request(Arc::new(|_, res, route, _| {
            Box::pin(async move {
                res.set_json(&route.options)?;
                Ok(None)
            })
        }))
        .unwrap();
    let scoped =
        f.root.intercept("server", json!({"routes":{"POST /same":{"kind":"post-only"}}})).get::<Server>().unwrap();
    scoped.get("/same", |_, _, _| async { Ok(None) }).unwrap();
    scoped.post("/same", |_, _, _| async { Ok(None) }).unwrap();
    assert_eq!(f.get("/same").await.json::<Value>().await.unwrap(), json!({}));
    assert_eq!(
        f.client.post(format!("{}/same", f.server.public_url())).send().await.unwrap().json::<Value>().await.unwrap(),
        json!({"kind":"post-only"})
    );
});

#[tokio::test]
async fn should_resolve_wildcard_host_to_127_0_0_1() {
    let mut config = ServerConfig::local(0);
    config.host = "0.0.0.0".into();
    let f = Fixture::new(config).await;
    assert!(f.server.public_url().starts_with("http://127.0.0.1:"));
    f.close().await;
}
#[tokio::test]
async fn should_prefer_config_public_url_when_set() {
    let mut config = ServerConfig::local(0);
    config.public_url = Some("https://example.com/".into());
    let f = Fixture::new(config).await;
    assert_eq!(f.server.public_url(), "https://example.com");
    f.close().await;
}
#[tokio::test]
async fn should_match_the_form_boundary_of_the_content_type_after_replacing_the_body() {
    use kardia_server::{FormData, FormEntry, FormValue};
    let f = Fixture::new(ServerConfig::local(0)).await;
    f.server
        .get("/form", |_, res, _| async move {
            res.set_form_data(FormData(vec![FormEntry { name: "a".into(), value: FormValue::Text("1".into()) }]));
            // A second body generates its own boundary, which the header must follow.
            res.set_form_data(FormData(vec![FormEntry { name: "b".into(), value: FormValue::Text("2".into()) }]));
            Ok(None)
        })
        .unwrap();
    let res = f.get("/form").await;
    let header = res.headers().get("content-type").unwrap().to_str().unwrap().to_owned();
    let body = res.text().await.unwrap();
    assert!(header.contains("boundary="), "{header}");
    // Every part of the body is separated by the boundary the header announces.
    assert!(body.contains(&format!("--{}\r\n", header.split("boundary=").nth(1).unwrap())), "{header}\n{body}");
    assert!(body.contains("name=\"b\""), "{body}");
    f.close().await;
}

#[tokio::test]
async fn should_bracket_an_ipv6_host_in_the_public_url() {
    // ::1 may be unavailable, e.g. in a container without IPv6.
    let mut config = ServerConfig::local(0);
    config.host = "::1".into();
    let Ok(f) = Fixture::try_new(config).await else { return };
    assert!(f.server.public_url().starts_with("http://[::1]:"), "{}", f.server.public_url());
    f.close().await;
}
#[tokio::test]
async fn should_fallback_to_next_port_if_configured_port_is_in_use() {
    let first = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = first.local_addr().unwrap().port();
    let mut config = ServerConfig::local(port);
    config.max_port = Some(port.saturating_add(10));
    let f = Fixture::new(config).await;
    assert!(f.server.address().port() > port);
    f.close().await;
}
