#![allow(unused_imports)]
#![allow(unused_variables)]
#![allow(dead_code)]

//! JS runtime test-suite. Split thematically from the former monolithic
//! `runtime.rs`; shared fixtures live here and every theme pulls them in
//! with `use super::*`.

use super::*;
pub(crate) use obscura_dom::parse_html;

mod tests_realm_scheduler;
mod tests_dom_core;
mod tests_style_viewport_loop;
mod tests_tree_traversal;
mod tests_scroll_geometry;
mod tests_render_resources;
mod tests_animation_waapi;
mod tests_render_invalidation;
mod tests_intersection_cssom;
mod tests_canvas_scripts;
mod tests_image_lifecycle;
mod tests_dom_interactions;
mod tests_cookies_fetch_events;
mod tests_modules;
mod tests_web_platform;

// ---- shared fixtures/helpers (moved verbatim from the former tests module) ----

    pub(crate) fn setup_runtime(html: &str) -> ObscuraJsRuntime {
        let dom = parse_html(html);
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_url("http://example.com/test");
        rt.set_title("Test Page");
        rt.run_page_init();
        rt
    }

    pub(crate) fn delayed_fetch_runtime(
        response_delay: std::time::Duration,
    ) -> (ObscuraJsRuntime, std::sync::mpsc::Receiver<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};

            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            accepted_tx.send(()).unwrap();
            std::thread::sleep(response_delay);
            let body = "hydrated";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            let _ = stream.write_all(response.as_bytes());
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
        (rt, accepted_rx)
    }

    #[cfg(feature = "render")]
    pub(crate) fn parser_image_runtime(
        html: &str,
        loader: impl obscura_render::RenderResourceLoader + 'static,
    ) -> ObscuraJsRuntime {
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(parse_html(html));
        rt.set_url("http://example.com/page/index.html");
        rt.state.borrow_mut().render_resources =
            obscura_render::RenderResourceCache::with_loader(loader);
        rt.run_page_init();
        rt
    }

    #[cfg(feature = "render")]
    pub(crate) fn two_by_three_png() -> Vec<u8> {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(
                "iVBORw0KGgoAAAANSUhEUgAAAAIAAAADCAYAAAC56t6B\
                 AAAAFklEQVR4nGP8z8Dwn4GBgYGJAQrgDAAxOwIE7x6DkQAAAABJRU5ErkJggg=="
                    .replace(char::is_whitespace, ""),
            )
            .unwrap()
    }

    pub(crate) fn change_srcset_image_sizes(rt: &mut ObscuraJsRuntime) {
        rt.execute_script("change-responsive-sizes", r#"srcsetImage.sizes = "800px";"#)
            .unwrap();
    }

    /// Regression for #105: `HTMLFormElement` must expose `.elements` so

    pub(crate) fn setup_runtime_with_cookies(
        html: &str,
    ) -> (ObscuraJsRuntime, std::sync::Arc<obscura_net::CookieJar>) {
        let dom = obscura_dom::parse_html(html);
        let jar = std::sync::Arc::new(obscura_net::CookieJar::new());
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_url("http://example.com/test");
        rt.set_title("Test Page");
        rt.set_cookie_jar(jar.clone());
        rt.run_page_init();
        (rt, jar)
    }

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

    pub(crate) fn spawn_one_response_server(status: &str, body: &str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let status = status.to_string();
        let body = body.to_string();
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};

            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 {}\r\nContent-Type: application/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status,
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        format!("http://{}", address)
    }

    pub(crate) fn spawn_duplicate_module_graph_server() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};

            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 2048];
                let length = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..length]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_ascii_whitespace().nth(1))
                    .unwrap_or("/");
                let body = match path {
                    "/entry.js" => {
                        "import './shared.js'; globalThis.__module_entry_ran = true;"
                    }
                    "/shared.js" => {
                        "globalThis.__shared_module_runs = \
                         (globalThis.__shared_module_runs || 0) + 1;"
                    }
                    _ => "throw new Error('unexpected module path');",
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Type: application/javascript\r\n\
                     Content-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len(),
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        format!("http://{}", address)
    }

    #[derive(Clone, Copy)]
    enum ModuleGraphFixture {
        CookieProtected,
        RedirectedChild,
    }

    pub(crate) fn spawn_module_graph_server(
        fixture: ModuleGraphFixture,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (requests_tx, requests_rx) = std::sync::mpsc::channel();
        let request_count = match fixture {
            ModuleGraphFixture::CookieProtected => 2,
            ModuleGraphFixture::RedirectedChild => 3,
        };
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};

            for _ in 0..request_count {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = vec![0u8; 8192];
                let length = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..length]).to_string();
                let lower_request = request.to_ascii_lowercase();
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_ascii_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_string();
                requests_tx.send(request.clone()).unwrap();

                let (status, extra_headers, body) = match (fixture, path.as_str()) {
                    (ModuleGraphFixture::CookieProtected, "/entry.js") => (
                        "200 OK",
                        "",
                        "import { value } from './child.js'; \
                         globalThis.__module_graph_value = value;",
                    ),
                    (ModuleGraphFixture::CookieProtected, "/child.js")
                        if lower_request.contains("\r\ncookie: session=ok\r\n")
                            && lower_request
                                .contains("\r\nuser-agent: modulegraphtest/1.0\r\n")
                            && lower_request.contains("\r\nx-module-test: shared\r\n") =>
                    {
                        ("200 OK", "", "export const value = 'cookie-child';")
                    }
                    (ModuleGraphFixture::CookieProtected, "/child.js") => (
                        "401 Unauthorized",
                        "",
                        "throw new Error('page request context missing');",
                    ),
                    (ModuleGraphFixture::RedirectedChild, "/entry.js") => (
                        "200 OK",
                        "",
                        "import { value } from './redirect.js'; \
                         globalThis.__module_graph_value = value;",
                    ),
                    (ModuleGraphFixture::RedirectedChild, "/redirect.js") => {
                        ("302 Found", "Location: /child.js\r\n", "")
                    }
                    (ModuleGraphFixture::RedirectedChild, "/child.js") => {
                        ("200 OK", "", "export const value = 'redirect-child';")
                    }
                    _ => ("404 Not Found", "", "not found"),
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\n\
                     Content-Type: application/javascript\r\n\
                     {extra_headers}Content-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len(),
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (format!("http://{}", address), requests_rx)
    }

    pub(crate) fn spawn_import_map_server() -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (requests_tx, requests_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};

            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 2048];
                let length = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..length]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_ascii_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_string();
                requests_tx.send(path.clone()).unwrap();
                let (status, body) = match path.as_str() {
                    "/vendor/pkg/feature.js" => ("200 OK", "export const value = 'prefix-static';"),
                    "/vendor/dynamic.js" => ("200 OK", "export const value = 'exact-dynamic';"),
                    _ => ("404 Not Found", "not found"),
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\n\
                     Content-Type: application/javascript\r\n\
                     Content-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len(),
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (format!("http://{}", address), requests_rx)
    }

    pub(crate) fn spawn_root_module_import_map_server() -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (requests_tx, requests_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};

            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let length = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..length]);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_ascii_whitespace().nth(1))
                .unwrap_or("/")
                .to_string();
            requests_tx.send(path.clone()).unwrap();
            let body = if path == "/entry.js" {
                "globalThis.__root_module_identity = 'entry';"
            } else {
                "globalThis.__root_module_identity = 'remapped';"
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\n\
                 Content-Type: application/javascript\r\n\
                 Content-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len(),
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        (format!("http://{}", address), requests_rx)
    }
