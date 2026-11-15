use kardia_core::{Context, Svc};
use kardia_fs::{FsConfig, FsPlugin, LocalFsPlugin, Url};
use kardia_server::{Server, ServerConfig, ServerExt, ServerPlugin};
use kardia_server_static::StaticPlugin;
use serde_json::{Value, json};

struct Fixture {
    root: Context,
    server: Svc<Server>,
    _directory: tempfile::TempDir,
}
impl Fixture {
    async fn new(mut config: Value) -> Self {
        let directory = tempfile::tempdir().unwrap();
        for (name, body) in [
            ("hello.html", "<h1>Hello</h1>\n"),
            ("noext", "plain text\n"),
            ("page", "<h1>Page without extension</h1>\n"),
            ("page.html", "alternative"),
            ("index.html", "<h1>Index</h1>\n"),
            ("sub/index.html", "<h1>Sub Index</h1>\n"),
            ("errors/404.html", "<h1>Not Found</h1>\n"),
            (".secret", "hidden"),
        ] {
            let path = directory.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        let root = Context::new();
        let base = Url::from_directory_path(directory.path()).unwrap();
        root.plugin::<FsPlugin>(FsConfig { base_url: Some(base) }).unwrap().settled().await.unwrap();
        root.plugin::<LocalFsPlugin>(Default::default()).unwrap().settled().await.unwrap();
        root.plugin::<ServerPlugin>(ServerConfig::local(0)).unwrap().settled().await.unwrap();
        config["root"] = json!(".");
        root.plugin_value::<StaticPlugin>(config).unwrap().settled().await.unwrap();
        let server = root.get::<Server>().unwrap();
        Self { root, server, _directory: directory }
    }
    async fn get(&self, path: &str) -> reqwest::Response {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
            .get(format!("{}{path}", self.server.public_url()))
            .send()
            .await
            .unwrap()
    }
    async fn text(&self, path: &str) -> String {
        let r = self.get(path).await;
        assert_eq!(r.status(), 200);
        r.text().await.unwrap()
    }
    async fn close(self) {
        self.root.fiber().dispose().await.unwrap();
    }
}
macro_rules! case {($name:ident,$config:expr,$f:ident,$body:block)=>{#[tokio::test]async fn $name(){let $f=Fixture::new($config).await;$body $f.close().await;}};}
case!(should_resolve_a_relative_root_against_the_base_url_of_fs, json!({}), f, {
    assert_eq!(f.text("/hello.html").await, "<h1>Hello</h1>\n");
});
case!(should_serve_an_existing_file, json!({}), f, {
    assert_eq!(f.text("/hello.html").await, "<h1>Hello</h1>\n");
});
case!(should_return_404_for_missing_file, json!({}), f, {
    assert_eq!(f.get("/nonexistent.html").await.status(), 404);
});
case!(should_serve_file_without_extension, json!({}), f, {
    assert_eq!(f.text("/noext").await, "plain text\n");
});
case!(should_not_try_extensions_by_default, json!({}), f, {
    assert_eq!(f.get("/hello").await.status(), 404);
});
case!(should_try_html_extension_when_configured, json!({"extensions":[".html"]}), f, {
    assert_eq!(f.text("/hello").await, "<h1>Hello</h1>\n");
});
case!(should_prefer_exact_file_over_extension_fallback, json!({"extensions":[".html"]}), f, {
    assert_eq!(f.text("/page").await, "<h1>Page without extension</h1>\n");
});
case!(should_serve_index_file_for_root, json!({}), f, {
    assert_eq!(f.text("/").await, "<h1>Index</h1>\n");
});
case!(should_serve_index_file_for_subdirectory_with_trailing_slash, json!({}), f, {
    assert_eq!(f.text("/sub/").await, "<h1>Sub Index</h1>\n");
});
case!(should_redirect_directory_without_trailing_slash, json!({}), f, {
    let r = f.get("/sub").await;
    assert_eq!(r.status(), 301);
    assert_eq!(r.headers()["location"], "/sub/");
});
case!(should_not_redirect_when_redirect_is_disabled, json!({"redirect":false}), f, {
    assert_eq!(f.get("/sub").await.status(), 404);
});
case!(should_return_403_for_path_traversal, json!({}), f, {
    assert_eq!(f.get("/..%2F..%2F..%2Fetc/passwd").await.status(), 403);
});
case!(should_not_fallthrough_by_default, json!({}), f, {
    assert_eq!(f.get("/missing").await.status(), 404);
});
case!(should_fallthrough_when_enabled, json!({"fallthrough":true}), f, {
    f.server
        .get("{/*path}", |_, res, _| async move {
            res.set_body("fallback");
            Ok(None)
        })
        .unwrap();
    assert_eq!(f.text("/missing").await, "fallback");
});
case!(should_serve_custom_error_page_with_original_status, json!({"errorPages":{"404":"errors/404.html"}}), f, {
    let r = f.get("/missing").await;
    assert_eq!(r.status(), 404);
    assert_eq!(r.text().await.unwrap(), "<h1>Not Found</h1>\n");
});
case!(
    should_display_the_error_page_regardless_of_disposition,
    json!({"disposition":"attachment","errorPages":{"404":"errors/404.html"}}),
    f,
    {
        let r = f.get("/missing").await;
        assert_eq!(r.status(), 404);
        assert!(!r.headers().contains_key("content-disposition"));
    }
);
case!(should_block_files_matching_exclude_pattern, json!({"exclude":["**/.*"]}), f, {
    assert_eq!(f.get("/.secret").await.status(), 404);
});
case!(should_serve_files_not_matching_exclude_pattern, json!({"exclude":["**/.*"]}), f, {
    assert_eq!(f.get("/hello.html").await.status(), 200);
});
case!(should_support_multiple_exclude_patterns, json!({"exclude":["*.html","noext"]}), f, {
    assert_eq!(f.get("/hello.html").await.status(), 404);
    assert_eq!(f.get("/noext").await.status(), 404);
});
case!(should_match_exclude_patterns_against_paths_separated_by_slashes, json!({"exclude":["sub/**"]}), f, {
    assert_eq!(f.get("/sub/index.html").await.status(), 404);
    assert_eq!(f.get("/hello.html").await.status(), 200);
});
case!(should_set_an_attachment_disposition, json!({"disposition":"attachment"}), f, {
    assert_eq!(f.get("/hello.html").await.headers()["content-disposition"], "attachment; filename*=UTF-8''hello.html");
});
case!(should_set_an_inline_disposition, json!({"disposition":"inline"}), f, {
    assert_eq!(f.get("/hello.html").await.headers()["content-disposition"], "inline");
});
