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

    #[test]
    pub(crate) fn test_document_cookie_reads_http_cookies() {
        let (mut rt, jar) = setup_runtime_with_cookies("<html><body></body></html>");
        let url = url::Url::parse("http://example.com/test").unwrap();
        jar.set_cookie("session=abc123; Path=/", &url);
        jar.set_cookie("theme=dark; Path=/", &url);
        let result = rt.evaluate("document.cookie").unwrap();
        let cookie_str = result.as_str().unwrap();
        assert!(
            cookie_str.contains("session=abc123"),
            "expected session cookie, got: {}",
            cookie_str
        );
        assert!(
            cookie_str.contains("theme=dark"),
            "expected theme cookie, got: {}",
            cookie_str
        );
    }

    #[test]
    pub(crate) fn test_document_cookie_excludes_httponly() {
        let (mut rt, jar) = setup_runtime_with_cookies("<html><body></body></html>");
        let url = url::Url::parse("http://example.com/test").unwrap();
        jar.set_cookie("visible=yes; Path=/", &url);
        jar.set_cookie("secret=token; Path=/; HttpOnly", &url);
        let result = rt.evaluate("document.cookie").unwrap();
        let cookie_str = result.as_str().unwrap();
        assert!(
            cookie_str.contains("visible=yes"),
            "expected visible cookie, got: {}",
            cookie_str
        );
        assert!(
            !cookie_str.contains("secret"),
            "httpOnly cookie should not be visible to JS, got: {}",
            cookie_str
        );
    }

    #[test]
    pub(crate) fn test_document_cookie_setter_stores_in_jar() {
        let (mut rt, jar) = setup_runtime_with_cookies("<html><body></body></html>");
        rt.evaluate("document.cookie = 'foo=bar; Path=/'").unwrap();
        let url = url::Url::parse("http://example.com/test").unwrap();
        let result = rt.evaluate("document.cookie").unwrap();
        assert!(result.as_str().unwrap().contains("foo=bar"));
        let header = jar.get_cookie_header(&url);
        assert!(
            header.contains("foo=bar"),
            "cookie should be in jar, got: {}",
            header
        );
    }

    #[test]
    pub(crate) fn test_document_cookie_delete_via_max_age() {
        let (mut rt, jar) = setup_runtime_with_cookies("<html><body></body></html>");
        let url = url::Url::parse("http://example.com/test").unwrap();
        rt.evaluate("document.cookie = 'temp=val; Path=/'").unwrap();
        assert!(rt
            .evaluate("document.cookie")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("temp=val"));
        rt.evaluate("document.cookie = 'temp=; Max-Age=0'").unwrap();
        let result = rt.evaluate("document.cookie").unwrap();
        assert!(
            !result.as_str().unwrap().contains("temp="),
            "cookie should be deleted, got: {}",
            result
        );
        assert!(!jar.get_cookie_header(&url).contains("temp="));
    }

    #[test]
    pub(crate) fn test_document_cookie_js_and_http_merge() {
        let (mut rt, jar) = setup_runtime_with_cookies("<html><body></body></html>");
        let url = url::Url::parse("http://example.com/test").unwrap();
        jar.set_cookie("server_sid=xyz; Path=/", &url);
        rt.evaluate("document.cookie = 'client_pref=light'")
            .unwrap();
        let result = rt.evaluate("document.cookie").unwrap();
        let cookie_str = result.as_str().unwrap();
        assert!(
            cookie_str.contains("server_sid=xyz"),
            "expected server cookie, got: {}",
            cookie_str
        );
        assert!(
            cookie_str.contains("client_pref=light"),
            "expected client cookie, got: {}",
            cookie_str
        );
    }

    #[test]
    pub(crate) fn test_document_cookie_empty_when_no_cookies() {
        let (mut rt, _jar) = setup_runtime_with_cookies("<html><body></body></html>");
        let result = rt.evaluate("document.cookie").unwrap();
        assert_eq!(result.as_str().unwrap(), "");
    }

    #[test]
    pub(crate) fn test_document_cookie_no_jar_returns_empty() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt.evaluate("document.cookie").unwrap();
        assert_eq!(result.as_str().unwrap(), "");
    }

    #[test]
    pub(crate) fn test_document_write_appends_to_body() {
        let mut rt = setup_runtime("<html><body><p>Existing</p></body></html>");
        rt.evaluate("document.write('<div>Added</div>')").unwrap();
        let html = rt.evaluate("document.body.innerHTML").unwrap();
        let body = html.as_str().unwrap();
        assert!(
            body.contains("Existing"),
            "existing content should remain, got: {}",
            body
        );
        assert!(
            body.contains("Added"),
            "written content should appear, got: {}",
            body
        );
    }

    #[test]
    pub(crate) fn test_document_writeln() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.evaluate("document.writeln('Hello')").unwrap();
        let html = rt.evaluate("document.body.innerHTML").unwrap();
        assert!(html.as_str().unwrap().contains("Hello"));
    }

    #[test]
    pub(crate) fn test_document_write_multiple_args() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.evaluate("document.write('Hello', ' ', 'World')")
            .unwrap();
        let text = rt.evaluate("document.body.textContent").unwrap();
        assert_eq!(text.as_str().unwrap().trim(), "Hello World");
    }

    #[test]
    pub(crate) fn test_document_open_clears_body() {
        let mut rt = setup_runtime("<html><body><p>Old content</p></body></html>");
        rt.evaluate("document.open()").unwrap();
        let html = rt.evaluate("document.body.innerHTML").unwrap();
        assert_eq!(html.as_str().unwrap(), "");
    }

    #[test]
    pub(crate) fn test_document_write_html_elements() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.evaluate(r#"document.write('<h1 id="title">Test</h1><p>Para</p>')"#)
            .unwrap();
        let h1 = rt
            .evaluate("document.querySelector('h1').textContent")
            .unwrap();
        assert_eq!(h1.as_str().unwrap(), "Test");
        let p = rt
            .evaluate("document.querySelector('p').textContent")
            .unwrap();
        assert_eq!(p.as_str().unwrap(), "Para");
    }

    #[test]
    pub(crate) fn test_url_relative_resolution() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate("new URL('data.json', 'http://example.com/path/page.html').href")
            .unwrap();
        assert_eq!(
            result.as_str().unwrap(),
            "http://example.com/path/data.json"
        );

        let result = rt
            .evaluate("new URL('/api/data', 'http://example.com/path/page.html').href")
            .unwrap();
        assert_eq!(result.as_str().unwrap(), "http://example.com/api/data");

        let result = rt
            .evaluate("new URL('https://other.com/foo', 'http://example.com/bar').href")
            .unwrap();
        assert_eq!(result.as_str().unwrap(), "https://other.com/foo");

        let result = rt
            .evaluate("new URL('sub/file.js', 'http://example.com/a/b/c.html').href")
            .unwrap();
        assert_eq!(
            result.as_str().unwrap(),
            "http://example.com/a/b/sub/file.js"
        );

        let result = rt
            .evaluate("new URL('api.json', 'http://localhost:8080/dir/index.html').href")
            .unwrap();
        assert_eq!(
            result.as_str().unwrap(),
            "http://localhost:8080/dir/api.json"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn test_fetch_url_input_decodes_binary_body_base64() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .call_function_on_for_cdp(
                r#"async () => {
                const originalFetchOp = Deno.core.ops.op_fetch_url;
                try {
                    Deno.core.ops.op_fetch_url = (url) => {
                        globalThis.__capturedFetchUrl = url;
                        return JSON.stringify({
                            status: 200,
                            headers: { "content-type": "application/wasm" },
                            bodyBase64: "AGFzbQEAAAA=",
                            url,
                        });
                    };
                    const response = await fetch(new URL("/pkg/app_bg.wasm", document.URL));
                    const bytes = Array.from(new Uint8Array(await response.arrayBuffer()));
                    return { url: globalThis.__capturedFetchUrl, bytes };
                } finally {
                    Deno.core.ops.op_fetch_url = originalFetchOp;
                }
            }"#,
                None,
                &[],
                true,
                true,
            )
            .await
            .unwrap();

        assert_eq!(
            result.value.unwrap(),
            serde_json::json!({
                "url": "http://example.com/pkg/app_bg.wasm",
                "bytes": [0, 97, 115, 109, 1, 0, 0, 0],
            })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn fetch_and_xhr_forward_browser_credentials_modes() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .call_function_on_for_cdp(
                r#"async () => {
                    const originalFetchOp = Deno.core.ops.op_fetch_url;
                    const calls = [];
                    try {
                        Deno.core.ops.op_fetch_url =
                            (url, method, headers, body, origin, mode, credentials) => {
                                calls.push({ url, credentials });
                                return JSON.stringify({
                                    status: 200,
                                    headers: {},
                                    body: "ok",
                                    url,
                                });
                            };

                        await fetch("/default");
                        await fetch("/omit", { credentials: "omit" });
                        const request = new Request("/included", { credentials: "include" });
                        await fetch(request);
                        await fetch(request.clone());
                        await fetch(request, { credentials: "same-origin" });

                        const sendXhr = (path, withCredentials) => new Promise((resolve, reject) => {
                            const xhr = new XMLHttpRequest();
                            xhr.open("GET", path);
                            xhr.withCredentials = withCredentials;
                            xhr.onload = resolve;
                            xhr.onerror = reject;
                            xhr.send();
                        });
                        await sendXhr("/xhr-default", false);
                        await sendXhr("/xhr-credentialed", true);

                        let invalidFetchRejected = false;
                        try {
                            await fetch("/bad", { credentials: "invalid" });
                        } catch (error) {
                            invalidFetchRejected = error instanceof TypeError;
                        }

                        return { calls, invalidFetchRejected };
                    } finally {
                        Deno.core.ops.op_fetch_url = originalFetchOp;
                    }
                }"#,
                None,
                &[],
                true,
                true,
            )
            .await
            .unwrap();

        assert_eq!(
            result.value.unwrap(),
            serde_json::json!({
                "calls": [
                    { "url": "http://example.com/default", "credentials": "same-origin" },
                    { "url": "http://example.com/omit", "credentials": "omit" },
                    { "url": "http://example.com/included", "credentials": "include" },
                    { "url": "http://example.com/included", "credentials": "include" },
                    { "url": "http://example.com/included", "credentials": "same-origin" },
                    { "url": "http://example.com/xhr-default", "credentials": "same-origin" },
                    { "url": "http://example.com/xhr-credentialed", "credentials": "include" },
                ],
                "invalidFetchRejected": true,
            })
        );
    }
    /// Serves a redirect chain across `connections` consecutive
    /// requests: `/hop/N` replies with 302 to `/hop/N-1`, `/hop/0` is the
    /// target. To a path the fixture cannot read it replies with 400
    /// instead of the target. A broken fixture thereby fails the test
    /// instead of letting it pass.
    pub(crate) fn redirect_chain_runtime(connections: usize) -> ObscuraJsRuntime {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};

            for _ in 0..connections {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut buffer = [0u8; 2048];
                let read = stream.read(&mut buffer).unwrap_or(0);
                let hop = String::from_utf8_lossy(&buffer[..read])
                    .split_whitespace()
                    .nth(1)
                    .and_then(|path| path.strip_prefix("/hop/"))
                    .and_then(|hop| hop.parse::<usize>().ok());
                let response = match hop {
                    Some(0) => {
                        let body = "arrived";
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len(),
                        )
                    }
                    Some(hop) => format!(
                        "HTTP/1.1 302 Found\r\nLocation: /hop/{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        hop - 1,
                    ),
                    None => {
                        let body = "unparsed";
                        format!(
                            "HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len(),
                        )
                    }
                };
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let origin = format!("http://{address}");
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(parse_html("<html><body></body></html>"));
        rt.set_url(&format!("{origin}/page"));
        rt.set_http_client(std::sync::Arc::new(
            obscura_net::ObscuraHttpClient::with_full_options(
                std::sync::Arc::new(obscura_net::CookieJar::new()),
                None,
                true,
            ),
        ));
        rt.run_page_init();
        rt
    }

    /// HTTP-redirect fetch returns a network error as soon as the
    /// redirect count *reaches* 20, and only increments it afterwards. So
    /// the twentieth hop must still succeed:
    /// https://fetch.spec.whatwg.org/#http-redirect-fetch
    /// WPT covers the same pair in `fetch/api/redirect/redirect-count.any.js`.
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn fetch_follows_the_twentieth_redirect() {
        let mut rt = redirect_chain_runtime(21);
        let result = rt
            .call_function_on_for_cdp(
                r#"async () => (await fetch("/hop/20")).text()"#,
                None,
                &[],
                true,
                true,
            )
            .await
            .unwrap();

        assert_eq!(result.value.unwrap(), serde_json::json!("arrived"));
    }

    /// The other end of the same pair: the twenty-first redirect must
    /// fail. `fetch` reports a rejected result as a `TypeError`.
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn fetch_rejects_the_twenty_first_redirect() {
        let mut rt = redirect_chain_runtime(21);
        let result = rt
            .call_function_on_for_cdp(
                r#"async () => {
                    try {
                        const response = await fetch("/hop/21");
                        return "resolved " + (await response.text());
                    } catch (error) {
                        return error instanceof TypeError ? "rejected" : "other";
                    }
                }"#,
                None,
                &[],
                true,
                true,
            )
            .await
            .unwrap();

        assert_eq!(result.value.unwrap(), serde_json::json!("rejected"));
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn dynamic_linked_stylesheet_enters_the_live_dom_with_imports_rebased() {
        let mut rt =
            setup_runtime("<html><head></head><body><div class=\"card\"></div></body></html>");
        let result = rt
            .call_function_on_for_cdp(
                r#"async () => {
                    const originalFetchOp = Deno.core.ops.op_fetch_url;
                    try {
                        Deno.core.ops.op_fetch_url = (url) => JSON.stringify({
                            status: 200,
                            headers: { "content-type": "text/css" },
                            body: url.endsWith("/assets/route.css")
                                ? '@import "./theme/base.css"; .card { display:grid; background-image:url("../img/card.png") }'
                                : '.card { color:red; background-image:url("./grain.png") }',
                            url,
                        });
                        const link = document.createElement("link");
                        link.setAttribute("rel", "stylesheet");
                        link.setAttribute("href", "/assets/route.css");
                        const loaded = new Promise(resolve => {
                            link.onload = () => resolve();
                        });
                        document.head.appendChild(link);
                        await loaded;
                        const style = document.querySelector("style[data-obscura-linked]");
                        const css = style.textContent;
                        const afterLink = link.nextSibling === style;
                        const list = document.styleSheets;
                        const sheet = link.sheet;
                        const rules = sheet.cssRules;
                        const cssom = {
                            listed: list.length === 1 && list[0] === sheet,
                            stable: link.sheet === sheet && sheet.cssRules === rules,
                            owner: sheet.ownerNode === link,
                            href: sheet.href,
                            selectors: Array.from(rules, rule => rule.selectorText),
                        };
                        link.remove();
                        return {
                            afterLink,
                            importedBeforeRoute:
                                css.indexOf("color:red") < css.indexOf("display:grid"),
                            importedUrl:
                                css.includes("http://example.com/assets/theme/grain.png"),
                            routeUrl:
                                css.includes("http://example.com/img/card.png"),
                            removedWithLink:
                                !document.querySelector("style[data-obscura-linked]"),
                            cssom,
                            detachedCssom: sheet.ownerNode === null
                                && link.sheet === null
                                && list.length === 0,
                        };
                    } finally {
                        Deno.core.ops.op_fetch_url = originalFetchOp;
                    }
                }"#,
                None,
                &[],
                true,
                true,
            )
            .await
            .unwrap();

        assert_eq!(
            result.value.unwrap(),
            serde_json::json!({
                "afterLink": true,
                "importedBeforeRoute": true,
                "importedUrl": true,
                "routeUrl": true,
                "removedWithLink": true,
                "cssom": {
                    "listed": true,
                    "stable": true,
                    "owner": true,
                    "href": "http://example.com/assets/route.css",
                    "selectors": [".card", ".card"],
                },
                "detachedCssom": true,
            })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn unsuccessful_dynamic_script_response_fires_error_without_evaluating_body() {
        let mut rt = setup_runtime("<html><head></head><body></body></html>");
        let result = rt
            .call_function_on_for_cdp(
                r#"async () => {
                    const originalFetchOp = Deno.core.ops.op_fetch_url;
                    try {
                        Deno.core.ops.op_fetch_url = (url) => JSON.stringify({
                            status: 401,
                            headers: { "content-type": "application/json" },
                            body: "globalThis.__executedFailedScript = true",
                            url,
                        });
                        const script = document.createElement("script");
                        script.src = "/unauthorized.js";
                        const outcome = await new Promise(resolve => {
                            script.onload = () => resolve("load");
                            script.onerror = () => resolve("error");
                            document.head.appendChild(script);
                        });
                        return {
                            outcome,
                            executed: globalThis.__executedFailedScript === true,
                        };
                    } finally {
                        Deno.core.ops.op_fetch_url = originalFetchOp;
                    }
                }"#,
                None,
                &[],
                true,
                true,
            )
            .await
            .unwrap();

        assert_eq!(
            result.value.unwrap(),
            serde_json::json!({
                "outcome": "error",
                "executed": false,
            })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn dynamic_classic_scripts_are_async_by_default_but_honor_async_false_order() {
        let mut rt = setup_runtime("<html><head></head><body></body></html>");
        let result = rt
            .call_function_on_for_cdp(
                r#"async () => {
                    const originalFetchOp = Deno.core.ops.op_fetch_url;
                    const runPair = async (explicitlyInOrder) => {
                        globalThis.__dynamicOrder = [];
                        Deno.core.ops.op_fetch_url = (url) => new Promise(resolve => {
                            const slow = url.includes("slow");
                            setTimeout(() => resolve(JSON.stringify({
                                status: 200,
                                headers: {"content-type": "text/javascript"},
                                body: `globalThis.__dynamicOrder.push("${slow ? "slow" : "fast"}")`,
                                url,
                            })), slow ? 30 : 1);
                        });
                        const load = name => new Promise(resolve => {
                            const script = document.createElement("script");
                            if (explicitlyInOrder) script.async = false;
                            script.src = `/${name}.js`;
                            script.onload = resolve;
                            document.head.appendChild(script);
                        });
                        await Promise.all([load("slow"), load("fast")]);
                        return globalThis.__dynamicOrder.slice();
                    };
                    try {
                        const asyncOrder = await runPair(false);
                        const inOrder = await runPair(true);
                        return {
                            asyncOrder,
                            inOrder,
                            pending: globalThis.__obscura_hasPendingDynamicScripts(),
                        };
                    } finally {
                        Deno.core.ops.op_fetch_url = originalFetchOp;
                    }
                }"#,
                None,
                &[],
                true,
                true,
            )
            .await
            .unwrap();

        assert_eq!(
            result.value.unwrap(),
            serde_json::json!({
                "asyncOrder": ["fast", "slow"],
                "inOrder": ["slow", "fast"],
                "pending": false,
            })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn test_response_array_buffer_preserves_typed_array_view() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .call_function_on_for_cdp(
                r#"async () => {
                const bytes = new Uint8Array([9, 0, 97, 115, 109, 1, 8]);
                const response = new Response(bytes.subarray(1, 6));
                return Array.from(new Uint8Array(await response.arrayBuffer()));
            }"#,
                None,
                &[],
                true,
                true,
            )
            .await
            .unwrap();

        assert_eq!(
            result.value.unwrap(),
            serde_json::json!([0, 97, 115, 109, 1])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn test_wasm_instantiate_streaming_uses_response_array_buffer() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .call_function_on_for_cdp(
                r#"async () => {
                const bytes = new Uint8Array([0, 97, 115, 109, 1, 0, 0, 0]);
                const result = await WebAssembly.instantiateStreaming(
                    Promise.resolve(new Response(bytes)),
                    {},
                );
                return result.instance instanceof WebAssembly.Instance;
            }"#,
                None,
                &[],
                true,
                true,
            )
            .await
            .unwrap();

        assert_eq!(result.value.unwrap(), serde_json::json!(true));
    }

    #[test]
    pub(crate) fn test_text_decoder_respects_typed_array_view() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate("new TextDecoder().decode(new Uint8Array([65, 66, 67]).subarray(1, 2))")
            .unwrap();
        assert_eq!(result.as_str().unwrap(), "B");
    }

    #[test]
    pub(crate) fn test_document_doctype() {
        let mut rt = setup_runtime("<!DOCTYPE html><html><body></body></html>");
        let result = rt.evaluate("document.doctype !== null").unwrap();
        assert_eq!(result, serde_json::json!(true));

        let name = rt.evaluate("document.doctype.name").unwrap();
        assert_eq!(name, serde_json::json!("html"));

        let node_type = rt.evaluate("document.doctype.nodeType").unwrap();
        assert_eq!(node_type.as_f64().unwrap() as i64, 10);
    }

    #[test]
    pub(crate) fn test_document_doctype_null_when_missing() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt.evaluate("document.doctype === null").unwrap();
        assert_eq!(result, serde_json::json!(true));
    }

    #[test]
    pub(crate) fn test_xml_serializer_doctype() {
        let mut rt = setup_runtime("<!DOCTYPE html><html><body></body></html>");
        let result = rt
            .evaluate("new XMLSerializer().serializeToString(document.doctype)")
            .unwrap();
        assert_eq!(result.as_str().unwrap(), "<!DOCTYPE html>");
    }

    #[test]
    pub(crate) fn test_xml_serializer_element() {
        let mut rt = setup_runtime(r#"<html><body><div id="x">Hello</div></body></html>"#);
        let result = rt
            .evaluate("new XMLSerializer().serializeToString(document.getElementById('x'))")
            .unwrap();
        let html = result.as_str().unwrap();
        assert!(html.contains("<div"));
        assert!(html.contains("Hello"));
    }

    #[test]
    pub(crate) fn test_create_event_custom_event_has_init_method() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let kind = rt
            .evaluate("typeof document.createEvent('CustomEvent').initCustomEvent")
            .unwrap();
        assert_eq!(kind, serde_json::json!("function"));
    }

    #[test]
    pub(crate) fn test_init_custom_event_sets_fields() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "test",
            r#"
            globalThis.__e = document.createEvent('CustomEvent');
            globalThis.__e.initCustomEvent('myevent', true, false, {hello: 'world'});
        "#,
        )
        .unwrap();
        let t = rt.evaluate("globalThis.__e.type").unwrap();
        assert_eq!(t, serde_json::json!("myevent"));
        let b = rt.evaluate("globalThis.__e.bubbles").unwrap();
        assert_eq!(b, serde_json::json!(true));
        let c = rt.evaluate("globalThis.__e.cancelable").unwrap();
        assert_eq!(c, serde_json::json!(false));
        let d = rt.evaluate("globalThis.__e.detail.hello").unwrap();
        assert_eq!(d, serde_json::json!("world"));
    }

    #[test]
    pub(crate) fn test_create_event_returns_correct_class() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let cust = rt
            .evaluate("document.createEvent('CustomEvent') instanceof CustomEvent")
            .unwrap();
        assert_eq!(cust, serde_json::json!(true));
        let mouse = rt
            .evaluate("document.createEvent('MouseEvent') instanceof MouseEvent")
            .unwrap();
        assert_eq!(mouse, serde_json::json!(true));
        let mouses = rt
            .evaluate("document.createEvent('MouseEvents') instanceof MouseEvent")
            .unwrap();
        assert_eq!(mouses, serde_json::json!(true));
        let kb = rt
            .evaluate("document.createEvent('KeyboardEvent') instanceof KeyboardEvent")
            .unwrap();
        assert_eq!(kb, serde_json::json!(true));
        let hash_change = rt
            .evaluate("document.createEvent('HashChangeEvent') instanceof HashChangeEvent")
            .unwrap();
        assert_eq!(hash_change, serde_json::json!(true));
        let message = rt
            .evaluate("document.createEvent('MessageEvent') instanceof MessageEvent")
            .unwrap();
        assert_eq!(message, serde_json::json!(true));
    }

    #[test]
    pub(crate) fn cssstyledeclaration_is_a_usable_global_interface() {
        // CSSStyleDeclaration was pre-declared non-enumerable but never assigned
        // a value (the only WebIDL interface missing its globalThis.X = X line),
        // so it was `undefined` while `'CSSStyleDeclaration' in window` was true,
        // and `el.style instanceof CSSStyleDeclaration` threw. It must be a real
        // constructor, non-enumerable like a browser, and the type of .style.
        let mut rt = setup_runtime("<html><body></body></html>");
        let v = rt
            .evaluate("(function(){var d=Object.getOwnPropertyDescriptor(window,'CSSStyleDeclaration');return (typeof window.CSSStyleDeclaration)+'|'+(document.body.style instanceof CSSStyleDeclaration)+'|'+(d?d.enumerable:'missing');})()")
            .unwrap();
        assert_eq!(v, serde_json::json!("function|true|false"));
    }

    #[test]
    pub(crate) fn test_create_event_rejects_unknown_interface() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"(() => {
                    try {
                        document.createEvent('NotAnEventInterface');
                        return null;
                    } catch (error) {
                        return [error.name, error instanceof DOMException];
                    }
                })()"#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!(["NotSupportedError", true]));
    }

    #[test]
    pub(crate) fn event_constructor_matches_webidl_conformance() {
        // new Event()/new CustomEvent() must throw (type is a required arg),
        // the type argument must be coerced to a string, CustomEvent.detail must
        // default to null (not undefined), createEvent must still build a
        // type-"" event, and an explicit detail must be preserved.
        let mut rt = setup_runtime("<html><body></body></html>");
        let v = rt
            .evaluate(
                "(function(){\
                 var out=[];\
                 try{new Event();out.push('no-throw')}catch(e){out.push(e.name)}\
                 try{new CustomEvent();out.push('no-throw')}catch(e){out.push(e.name)}\
                 out.push(new Event(123).type+':'+typeof new Event(123).type);\
                 out.push(String(new CustomEvent('x').detail));\
                 out.push(String(new CustomEvent('x',{detail:7}).detail));\
                 out.push(new Event('click').type);\
                 out.push(JSON.stringify(document.createEvent('Event').type));\
                 return out.join('|');\
                 })()",
            )
            .unwrap();
        assert_eq!(
            v,
            serde_json::json!("TypeError|TypeError|123:string|null|7|click|\"\"")
        );
    }

    #[test]
    pub(crate) fn test_promise_rejection_event_requires_promise() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"(() => {
                    const promise = Promise.resolve(1);
                    const event = new PromiseRejectionEvent('unhandledrejection', {
                        promise,
                        reason: 'failed'
                    });
                    let missingPromiseThrows = false;
                    try {
                        new PromiseRejectionEvent('unhandledrejection');
                    } catch (error) {
                        missingPromiseThrows = error instanceof TypeError;
                    }
                    return [
                        event instanceof Event,
                        event.promise === promise,
                        event.reason === 'failed',
                        missingPromiseThrows
                    ];
                })()"#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!([true, true, true, true]));
    }

    #[test]
    pub(crate) fn test_create_event_rejects_promise_rejection_event() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"(() => {
                    try {
                        document.createEvent('PromiseRejectionEvent');
                        return null;
                    } catch (error) {
                        return [error.name, error instanceof DOMException];
                    }
                })()"#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!(["NotSupportedError", true]));
    }

    #[test]
    pub(crate) fn test_create_event_supports_legacy_event_aliases() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"['Event', 'Events', 'HTMLEvents', 'SVGEvents'].map(name => {
                    const event = document.createEvent(name);
                    return [event instanceof Event, event.constructor === Event, event.type];
                })"#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                [true, true, ""],
                [true, true, ""],
                [true, true, ""],
                [true, true, ""]
            ])
        );
    }

    #[test]
    pub(crate) fn test_storage_event_constructor_and_legacy_factory() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"(() => {
                    const event = new StorageEvent('storage', {
                        key: 'theme',
                        oldValue: 'light',
                        newValue: 'dark',
                        url: 'https://example.test/'
                    });
                    const legacy = document.createEvent('StorageEvent');
                    legacy.initStorageEvent(
                        'storage', false, false, 'count', '1', '2',
                        'https://example.test/', null
                    );
                    return [
                        event instanceof Event,
                        event.key,
                        event.oldValue,
                        event.newValue,
                        event.url,
                        legacy instanceof StorageEvent,
                        legacy.key,
                        legacy.newValue
                    ];
                })()"#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                true,
                "theme",
                "light",
                "dark",
                "https://example.test/",
                true,
                "count",
                "2"
            ])
        );
    }

    #[test]
    pub(crate) fn test_html_to_markdown_headings() {
        let mut rt =
            setup_runtime("<html><body><h1>Title</h1><h2>Sub</h2><p>Body</p></body></html>");
        let md = rt
            .evaluate(crate::HTML_TO_MARKDOWN_JS)
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        assert!(md.contains("# Title"), "missing H1: {}", md);
        assert!(md.contains("## Sub"), "missing H2: {}", md);
        assert!(md.contains("Body"), "missing paragraph text: {}", md);
    }

    #[test]
    pub(crate) fn test_html_to_markdown_links_and_inline() {
        let mut rt = setup_runtime(
            r#"<html><body><p>Hello <strong>world</strong> <a href="https://x.test/">link</a> <em>em</em></p></body></html>"#,
        );
        let md = rt
            .evaluate(crate::HTML_TO_MARKDOWN_JS)
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        assert!(md.contains("**world**"), "missing strong: {}", md);
        assert!(md.contains("*em*"), "missing em: {}", md);
        assert!(
            md.contains("[link](https://x.test/)"),
            "missing link: {}",
            md
        );
    }

    #[test]
    pub(crate) fn test_html_to_markdown_lists() {
        let mut rt = setup_runtime(
            "<html><body><ul><li>A</li><li>B</li></ul><ol><li>X</li><li>Y</li></ol></body></html>",
        );
        let md = rt
            .evaluate(crate::HTML_TO_MARKDOWN_JS)
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        assert!(md.contains("- A"), "missing unordered A: {}", md);
        assert!(md.contains("- B"), "missing unordered B: {}", md);
        assert!(md.contains("1. X"), "missing ordered X: {}", md);
    }

    #[test]
    pub(crate) fn test_html_to_markdown_skips_script_and_style() {
        let mut rt = setup_runtime(
            "<html><body><p>Text</p><script>alert(1)</script><style>body{color:red}</style></body></html>",
        );
        let md = rt
            .evaluate(crate::HTML_TO_MARKDOWN_JS)
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        assert!(md.contains("Text"), "missing visible text: {}", md);
        assert!(!md.contains("alert"), "leaked script content: {}", md);
        assert!(!md.contains("color:red"), "leaked style content: {}", md);
    }

    #[test]
    pub(crate) fn test_page_content_puppeteer_pattern() {
        let mut rt =
            setup_runtime("<!DOCTYPE html><html><head></head><body><p>Test</p></body></html>");
        let result = rt.evaluate(
            "(function() { let retVal = ''; if (document.doctype) retVal = new XMLSerializer().serializeToString(document.doctype); if (document.documentElement) retVal += document.documentElement.outerHTML; return retVal; })()"
        ).unwrap();
        let html = result.as_str().unwrap();
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<html>"));
        assert!(html.contains("<p>Test</p>"));
    }

    #[test]
    pub(crate) fn test_element_from_point_is_function() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let kind = rt.evaluate("typeof document.elementFromPoint").unwrap();
        assert_eq!(kind, serde_json::json!("function"));
        let kind2 = rt.evaluate("typeof document.elementsFromPoint").unwrap();
        assert_eq!(kind2, serde_json::json!("function"));
    }

    #[test]
    pub(crate) fn test_element_from_point_in_viewport_returns_body() {
        let mut rt = setup_runtime("<html><body><h1>Hi</h1></body></html>");
        let tag = rt
            .evaluate("document.elementFromPoint(10, 10)?.tagName")
            .unwrap();
        assert_eq!(tag, serde_json::json!("BODY"));
    }

    #[test]
    pub(crate) fn test_element_from_point_out_of_viewport_returns_null() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let neg_x = rt.evaluate("document.elementFromPoint(-1, 10)").unwrap();
        assert_eq!(neg_x, serde_json::Value::Null);
        let neg_y = rt.evaluate("document.elementFromPoint(10, -1)").unwrap();
        assert_eq!(neg_y, serde_json::Value::Null);
        let huge = rt
            .evaluate("document.elementFromPoint(99999, 99999)")
            .unwrap();
        assert_eq!(huge, serde_json::Value::Null);
    }

    #[test]
    pub(crate) fn test_elements_from_point_returns_array() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let len_in = rt
            .evaluate("document.elementsFromPoint(10, 10).length")
            .unwrap();
        assert_eq!(len_in.as_f64().unwrap() as i64, 1);
        let len_out = rt
            .evaluate("document.elementsFromPoint(-1, -1).length")
            .unwrap();
        assert_eq!(len_out.as_f64().unwrap() as i64, 0);
    }

    #[test]
    pub(crate) fn test_element_from_point_non_numeric_returns_null() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let nan = rt.evaluate("document.elementFromPoint(NaN, 10)").unwrap();
        assert_eq!(nan, serde_json::Value::Null);
        let inf = rt
            .evaluate("document.elementFromPoint(Infinity, 10)")
            .unwrap();
        assert_eq!(inf, serde_json::Value::Null);
    }
