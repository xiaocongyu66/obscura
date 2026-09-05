#![allow(unused_imports)]
#![allow(unused_variables)]
#![allow(dead_code)]


use super::*;
use crate::runtime::*;
use obscura_dom::parse_html;
#[allow(unused_imports)]
use crate::module_loader::ObscuraModuleLoader;
#[cfg(feature = "render")]
#[allow(unused_imports)]
use crate::ops::ImageRequestProfile;
#[allow(unused_imports)]
use crate::ops::ObscuraState;

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn entry_module_http_failure_is_not_evaluated_as_empty_source() {
        let base = spawn_one_response_server("404 Not Found", "not found");
        let jar = std::sync::Arc::new(obscura_net::CookieJar::new());
        let client = std::sync::Arc::new(obscura_net::ObscuraHttpClient::with_full_options(
            jar, None, true,
        ));
        let mut rt = ObscuraJsRuntime::with_base_url(&format!("{}/", base));
        rt.set_http_client(client);

        let error = rt
            .load_module(&format!("{}/entry.js", base), 1_000)
            .await
            .unwrap_err();
        assert!(
            error.contains("HTTP 404"),
            "expected entry fetch status in error, got: {}",
            error
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn dependency_prepared_as_root_is_evaluated_only_once() {
        let base = spawn_duplicate_module_graph_server();
        let jar = std::sync::Arc::new(obscura_net::CookieJar::new());
        let client = std::sync::Arc::new(obscura_net::ObscuraHttpClient::with_full_options(
            jar, None, true,
        ));
        let mut rt = ObscuraJsRuntime::with_base_url(&format!("{}/", base));
        rt.set_http_client(client);

        // The HTML scheduler prepares all module graphs before evaluating any
        // of them. The shared URL is both a dependency and a later root, which
        // used to reach deno_core::mod_evaluate twice and panic (#591).
        let entry = rt
            .prepare_module(&format!("{}/entry.js", base), 1_000)
            .await
            .unwrap();
        let shared = rt
            .prepare_module(&format!("{}/shared.js", base), 1_000)
            .await
            .unwrap();

        rt.evaluate_prepared_module(entry, 1_000).await.unwrap();
        rt.evaluate_prepared_module(shared, 1_000).await.unwrap();

        assert_eq!(
            rt.evaluate("globalThis.__module_entry_ran === true").unwrap(),
            serde_json::json!(true),
        );
        assert_eq!(
            rt.evaluate("globalThis.__shared_module_runs").unwrap(),
            serde_json::json!(1.0),
        );
    }

    #[test]
    pub(crate) fn heap_limit_terminates_script_and_runtime_recovers() {
        crate::v8_flags::set_v8_flags("--max-old-space-size=32 --max-semi-space-size=1");
        let mut rt = ObscuraJsRuntime::new();

        for _ in 0..2 {
            let error = rt
                .evaluate(
                    "(() => { const chunks = []; for (;;) { \
                     chunks.push(new Array(262144).fill(1.25)); } })()",
                )
                .unwrap_err();
            assert!(
                error.contains("heap limit exceeded"),
                "unexpected heap failure: {error}",
            );
            assert_eq!(
                rt.evaluate("globalThis.__runtime_survived_oom = true").unwrap(),
                serde_json::json!(true),
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn descendant_module_uses_page_cookie_identity_and_headers() {
        let (base, requests) = spawn_module_graph_server(ModuleGraphFixture::CookieProtected);
        let page_url = url::Url::parse(&format!("{}/", base)).unwrap();
        let jar = std::sync::Arc::new(obscura_net::CookieJar::new());
        jar.set_cookie("session=ok; Path=/", &page_url);
        let client = std::sync::Arc::new(obscura_net::ObscuraHttpClient::with_full_options(
            jar.clone(),
            None,
            true,
        ));
        client.set_user_agent("ModuleGraphTest/1.0").await;
        client
            .set_extra_headers(std::collections::HashMap::from([(
                "x-module-test".to_string(),
                "shared".to_string(),
            )]))
            .await;
        let callback_urls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let callbacks = std::sync::Arc::new(obscura_net::CallbackRegistry::new());
        let callback_urls_capture = callback_urls.clone();
        callbacks.add_request(std::sync::Arc::new(move |request| {
            callback_urls_capture
                .lock()
                .unwrap()
                .push(request.url.path().to_string());
        }));

        let mut rt = ObscuraJsRuntime::with_base_url(&format!("{}/", base));
        rt.set_cookie_jar(jar);
        rt.set_http_client(client);
        rt.set_callbacks(callbacks);
        rt.load_module(&format!("{}/entry.js", base), 1_000)
            .await
            .unwrap();

        assert_eq!(
            rt.evaluate("globalThis.__module_graph_value").unwrap(),
            serde_json::json!("cookie-child"),
        );
        let requests = (0..2)
            .map(|_| {
                requests
                    .recv_timeout(std::time::Duration::from_secs(1))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(requests
            .iter()
            .any(|request| request.starts_with("GET /entry.js ")));
        let child = requests
            .iter()
            .find(|request| request.starts_with("GET /child.js "))
            .expect("descendant request");
        let child_lower = child.to_ascii_lowercase();
        assert!(
            child_lower.contains("\r\ncookie: session=ok\r\n"),
            "{child}"
        );
        assert!(
            child_lower.contains("\r\nuser-agent: modulegraphtest/1.0\r\n"),
            "{child}"
        );
        assert!(
            child_lower.contains("\r\nx-module-test: shared\r\n"),
            "{child}"
        );
        assert_eq!(
            *callback_urls.lock().unwrap(),
            vec!["/entry.js", "/child.js"],
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn cross_origin_module_descendant_does_not_gain_module_origin_cookies() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let module_base = format!("http://{address}");
        let document_url = "http://127.0.0.1:1/page";
        let document_origin = "http://127.0.0.1:1";
        let (requests_tx, requests_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};

            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 4096];
                let length = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..length]).to_string();
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_ascii_whitespace().nth(1))
                    .unwrap_or("/");
                let body = match path {
                    "/entry.js" => {
                        "import { value } from './child.js'; globalThis.__cors_value = value;"
                    }
                    "/child.js" => "export const value = 'safe';",
                    _ => "throw new Error('unexpected module path');",
                };
                requests_tx.send(request).unwrap();
                let response = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Type: application/javascript\r\n\
                     Access-Control-Allow-Origin: {document_origin}\r\n\
                     Cache-Control: public, max-age=3600\r\n\
                     Content-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len(),
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });

        let module_origin = url::Url::parse(&module_base).unwrap();
        let jar = std::sync::Arc::new(obscura_net::CookieJar::new());
        jar.set_cookie("cdn_session=secret; Path=/", &module_origin);
        let client = std::sync::Arc::new(obscura_net::ObscuraHttpClient::with_full_options(
            jar, None, true,
        ));
        let mut rt = ObscuraJsRuntime::with_base_url(document_url);
        rt.set_http_client(client);
        rt.load_module(&format!("{module_base}/entry.js"), 1_000)
            .await
            .unwrap();

        assert_eq!(
            rt.evaluate("globalThis.__cors_value").unwrap(),
            serde_json::json!("safe"),
        );
        let requests = (0..2)
            .map(|_| {
                requests_rx
                    .recv_timeout(std::time::Duration::from_secs(1))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        for request in &requests {
            let lower = request.to_ascii_lowercase();
            assert!(lower.contains("\r\norigin: http://127.0.0.1:1\r\n"), "{request}");
            assert!(!lower.contains("\r\ncookie:"), "{request}");
        }
        let child = requests
            .iter()
            .find(|request| request.starts_with("GET /child.js "))
            .expect("child module request")
            .to_ascii_lowercase();
        assert!(
            child.contains(&format!("\r\nreferer: {module_base}/entry.js\r\n")),
            "{child}",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn descendant_module_follows_page_client_redirects() {
        let (base, requests) = spawn_module_graph_server(ModuleGraphFixture::RedirectedChild);
        let jar = std::sync::Arc::new(obscura_net::CookieJar::new());
        let client = std::sync::Arc::new(obscura_net::ObscuraHttpClient::with_full_options(
            jar, None, true,
        ));
        let mut rt = ObscuraJsRuntime::with_base_url(&format!("{}/", base));
        rt.set_http_client(client);
        rt.load_module(&format!("{}/entry.js", base), 1_000)
            .await
            .unwrap();

        assert_eq!(
            rt.evaluate("globalThis.__module_graph_value").unwrap(),
            serde_json::json!("redirect-child"),
        );
        let paths = (0..3)
            .map(|_| {
                requests
                    .recv_timeout(std::time::Duration::from_secs(1))
                    .unwrap()
                    .lines()
                    .next()
                    .unwrap()
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            vec![
                "GET /entry.js HTTP/1.1",
                "GET /redirect.js HTTP/1.1",
                "GET /child.js HTTP/1.1",
            ],
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn import_map_resolves_prefix_static_and_exact_dynamic_imports() {
        let (base, requests) = spawn_import_map_server();
        let jar = std::sync::Arc::new(obscura_net::CookieJar::new());
        let client = std::sync::Arc::new(obscura_net::ObscuraHttpClient::with_full_options(
            jar, None, true,
        ));
        let mut rt = ObscuraJsRuntime::with_base_url(&format!("{}/app/index.html", base));
        rt.set_http_client(client);
        rt.add_import_map(
            r#"{
                "imports": {
                    "pkg/": "../vendor/pkg/",
                    "dynamic-pkg": "../vendor/dynamic.js"
                }
            }"#,
            &format!("{}/config/import-map.json", base),
        )
        .unwrap();

        rt.load_inline_module(
            "import { value as prefix } from 'pkg/feature.js'; \
             const dynamic = (await import('dynamic-pkg')).value; \
             globalThis.__import_map_values = [prefix, dynamic];",
            &format!("{}/app/index.html", base),
            1_000,
        )
        .await
        .unwrap();

        assert_eq!(
            rt.evaluate("globalThis.__import_map_values").unwrap(),
            serde_json::json!(["prefix-static", "exact-dynamic"]),
        );
        let paths = (0..2)
            .map(|_| {
                requests
                    .recv_timeout(std::time::Duration::from_secs(1))
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(
            paths.contains(&"/vendor/pkg/feature.js".to_string()),
            "{paths:?}"
        );
        assert!(
            paths.contains(&"/vendor/dynamic.js".to_string()),
            "{paths:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn import_map_does_not_remap_external_root_module_url() {
        let (base, requests) = spawn_root_module_import_map_server();
        let jar = std::sync::Arc::new(obscura_net::CookieJar::new());
        let client = std::sync::Arc::new(obscura_net::ObscuraHttpClient::with_full_options(
            jar, None, true,
        ));
        let mut rt = ObscuraJsRuntime::with_base_url(&format!("{}/index.html", base));
        rt.set_http_client(client);
        rt.add_import_map(
            &format!(r#"{{"imports":{{"{base}/entry.js":"{base}/remapped.js"}}}}"#),
            &format!("{}/index.html", base),
        )
        .unwrap();

        rt.load_module(&format!("{}/entry.js", base), 1_000)
            .await
            .unwrap();
        assert_eq!(
            rt.evaluate("globalThis.__root_module_identity").unwrap(),
            serde_json::json!("entry"),
        );
        assert_eq!(
            requests
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            "/entry.js",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn inline_modules_expose_document_base_as_import_meta_url() {
        let mut rt = ObscuraJsRuntime::with_base_url("https://example.com/page/index.html");
        rt.load_inline_module(
            "globalThis.__first_inline_url = import.meta.url;",
            "https://example.com/base/",
            1_000,
        )
        .await
        .unwrap();
        rt.load_inline_module(
            "globalThis.__second_inline_url = import.meta.url;",
            "https://example.com/base/",
            1_000,
        )
        .await
        .unwrap();

        assert_eq!(
            rt.evaluate("[globalThis.__first_inline_url, globalThis.__second_inline_url]")
                .unwrap(),
            serde_json::json!(["https://example.com/base/", "https://example.com/base/"])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn classic_script_url_is_dynamic_import_referrer() {
        use std::io::{Read as _, Write as _};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let request_thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let length = stream.read(&mut request).unwrap();
            let path = String::from_utf8_lossy(&request[..length])
                .lines()
                .next()
                .and_then(|line| line.split_ascii_whitespace().nth(1))
                .unwrap_or("/")
                .to_string();
            let body = "export const value = 'scoped-classic';";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            stream.write_all(response.as_bytes()).unwrap();
            path
        });
        let base = format!("http://{address}");
        let jar = std::sync::Arc::new(obscura_net::CookieJar::new());
        let client = std::sync::Arc::new(obscura_net::ObscuraHttpClient::with_full_options(
            jar, None, true,
        ));
        let mut rt = ObscuraJsRuntime::with_base_url(&format!("{base}/page/index.html"));
        rt.set_http_client(client);
        rt.set_dom(parse_html("<html><body></body></html>"));
        rt.run_page_init();
        rt.add_import_map(
            &format!(r#"{{"scopes":{{"{base}/classic/":{{"pkg":"{base}/scoped.js"}}}}}}"#),
            &format!("{base}/page/index.html"),
        )
        .unwrap();

        rt.execute_script(
            &format!("{base}/classic/entry.js"),
            "document.documentElement.setAttribute('data-classic-op', 'ran'); \
             import('pkg').then(module => { globalThis.__classic_import = module.value; });",
        )
        .unwrap();
        rt.run_event_loop().await.unwrap();

        assert_eq!(
            rt.evaluate("globalThis.__classic_import").unwrap(),
            serde_json::json!("scoped-classic")
        );
        assert_eq!(
            rt.evaluate("document.documentElement.getAttribute('data-classic-op')")
                .unwrap(),
            serde_json::json!("ran")
        );
        assert_eq!(request_thread.join().unwrap(), "/scoped.js");
    }

    #[test]
    pub(crate) fn timed_out_classic_script_leaves_runtime_reusable() {
        let mut rt = ObscuraJsRuntime::new();
        rt.execute_script_with_timeout(
            "https://example.test/hang.js",
            "while (true) {}",
            std::time::Duration::from_millis(20),
        )
        .unwrap();
        rt.execute_script(
            "https://example.test/after-timeout.js",
            "globalThis.__after_timeout = true;",
        )
        .unwrap();
        assert_eq!(
            rt.evaluate("globalThis.__after_timeout").unwrap(),
            serde_json::json!(true)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn inline_module_graph_error_propagates() {
        let mut rt = ObscuraJsRuntime::with_base_url("https://example.com/");
        let error = rt
            .load_inline_module("import 'bare-specifier';", "https://example.com/", 1_000)
            .await
            .unwrap_err();
        assert!(
            error.contains("Inline module load error"),
            "expected graph load error, got: {}",
            error
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn inline_module_evaluation_error_propagates() {
        let mut rt = ObscuraJsRuntime::with_base_url("https://example.com/");
        let error = rt
            .load_inline_module(
                "throw new Error('module-evaluation-boom');",
                "https://example.com/",
                1_000,
            )
            .await
            .unwrap_err();
        assert!(
            error.contains("Inline module eval error") && error.contains("module-evaluation-boom"),
            "expected evaluation error, got: {}",
            error
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn inline_module_evaluation_timeout_propagates() {
        let mut rt = ObscuraJsRuntime::with_base_url("https://example.com/");
        let error = rt
            .load_inline_module(
                "await new Promise(resolve => setTimeout(resolve, 10000));",
                "https://example.com/",
                20,
            )
            .await
            .unwrap_err();
        assert!(
            error.contains("Inline module evaluation timed out after"),
            "expected evaluation timeout, got: {}",
            error
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn successful_inline_module_does_not_wait_for_interval_idle() {
        let mut rt = ObscuraJsRuntime::with_base_url("https://example.com/");
        rt.load_inline_module(
            "globalThis.__module_loaded = true; setInterval(() => {}, 10000);",
            "https://example.com/",
            500,
        )
        .await
        .unwrap();
        assert_eq!(
            rt.evaluate("globalThis.__module_loaded").unwrap(),
            serde_json::json!(true)
        );
    }

    // Issue #139 — proxy_url must thread through to both the ES-module
    // loader (module_loader.rs) and op_fetch_url's reqwest client
    // (ops.rs::build_request_client). Pre-fix both built clients with
    // `Client::builder().build()` — no proxy — so JS fetch/XHR and
    // dynamic imports silently bypassed BrowserContext.proxy_url.
    //
    // Phase 5.5 RED check: each test references a symbol that does NOT
    // exist on main (proxy_url() accessor, with_proxy ctor,
    // with_base_url_and_proxy ctor), so the tests fail to compile without
    // the prod fix.
    #[test]
    pub(crate) fn http_client_round_trips_proxy_url() {
        use obscura_net::{CookieJar, ObscuraHttpClient};
        let jar = std::sync::Arc::new(CookieJar::new());
        let configured =
            ObscuraHttpClient::with_options(jar.clone(), Some("http://proxy.test:8080"));
        assert_eq!(
            configured.proxy_url(),
            Some("http://proxy.test:8080"),
            "proxy_url() must expose the value passed to with_options"
        );

        let direct = ObscuraHttpClient::with_options(jar, None);
        assert_eq!(
            direct.proxy_url(),
            None,
            "proxy_url() must return None when no proxy was configured"
        );
    }

    #[test]
    pub(crate) fn module_loader_stores_proxy_for_dynamic_imports() {
        use crate::module_loader::ObscuraModuleLoader;
        let loader = ObscuraModuleLoader::with_proxy(
            "https://example.com/",
            Some("http://proxy.test:8080".to_string()),
        );
        assert_eq!(loader.proxy_url.as_deref(), Some("http://proxy.test:8080"));
        assert_eq!(loader.base_url, "https://example.com/");

        // Default constructor must keep the historical "no proxy" behaviour.
        let direct = ObscuraModuleLoader::new("https://example.com/");
        assert_eq!(direct.proxy_url, None);
    }

    #[test]
    pub(crate) fn runtime_with_base_url_and_proxy_constructs_successfully() {
        // Sanity-check the public ctor that page.rs uses to thread proxy
        // through to the module loader. Direct (None) and proxied paths
        // must both initialise the JS environment.
        let _direct = ObscuraJsRuntime::with_base_url_and_proxy("https://example.com/", None);
        let _proxied = ObscuraJsRuntime::with_base_url_and_proxy(
            "https://example.com/",
            Some("http://proxy.test:8080".to_string()),
        );
    }

    // ── Issue #45 (Playwright actionability) regression tests ────────────────
    // Kept at the end of the module so they don't share textual context with
    // unrelated test additions in other branches (avoids spurious merge
    // conflicts when both this branch and an unrelated bootstrap.js change
    // add tests near the start of `mod tests`).

    /// Playwright >= 1.25 calls `element.checkVisibility(...)` before every
    /// input event. If the method isn't defined Playwright retries until its
    /// action timeout fires. Without a layout engine we can't compute it
    /// properly, so the stub always returns true — still strictly better
