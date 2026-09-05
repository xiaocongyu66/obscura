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

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn parser_images_load_concurrently_without_blocking_the_event_loop() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server_accepted = accepted.clone();
        let server_active = active.clone();
        let server_max_active = max_active.clone();
        let png = two_by_three_png();
        std::thread::spawn(move || {
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                server_accepted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let active = server_active.clone();
                let max_active = server_max_active.clone();
                let png = png.clone();
                std::thread::spawn(move || {
                    use std::io::{Read as _, Write as _};

                    let mut request = [0u8; 2048];
                    let _ = stream.read(&mut request);
                    let concurrent = active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    max_active.fetch_max(concurrent, std::sync::atomic::Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(150));
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        png.len()
                    );
                    stream.write_all(response.as_bytes()).unwrap();
                    stream.write_all(&png).unwrap();
                    active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                });
            }
        });

        let base = format!("http://{address}");
        let html = format!(
            r#"<img src="{base}/one.png"><img src="{base}/two.png">
                <img src="{base}/three.png"><img src="{base}/shared.png">
                <img src="{base}/shared.png">"#
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(parse_html(&html));
        rt.set_url(&format!("{base}/page.html"));
        rt.set_http_client(std::sync::Arc::new(
            obscura_net::ObscuraHttpClient::with_full_options(
                std::sync::Arc::new(obscura_net::CookieJar::new()),
                None,
                true,
            ),
        ));
        rt.run_page_init();

        let started = std::time::Instant::now();
        let result = rt
            .evaluate_for_cdp(
                r#"
                new Promise(resolve => {
                    globalThis.__imageTimerRan = false;
                    setTimeout(() => { __imageTimerRan = true; }, 10);
                    const images = Array.from(document.images);
                    const events = [];
                    const finish = (type, image) => {
                        events.push([
                            type,
                            __imageTimerRan,
                            image.naturalWidth,
                            image.naturalHeight,
                        ]);
                        if (events.length === images.length) resolve(events);
                    };
                    for (const image of images) {
                        image.addEventListener("load", () => finish("load", image));
                        image.addEventListener("error", () => finish("error", image));
                        void image.complete;
                    }
                    setTimeout(() => resolve([["timed out"]]), 2000);
                })
                "#,
                true,
                true,
            )
            .await
            .unwrap();
        let elapsed = started.elapsed();
        assert_eq!(
            result.value.unwrap(),
            serde_json::json!([
                ["load", true, 2, 3],
                ["load", true, 2, 3],
                ["load", true, 2, 3],
                ["load", true, 2, 3],
                ["load", true, 2, 3],
            ])
        );
        assert_eq!(
            accepted.load(std::sync::atomic::Ordering::SeqCst),
            4,
            "two elements selecting one URL must share a single request"
        );
        assert!(
            max_active.load(std::sync::atomic::Ordering::SeqCst) >= 3,
            "slow image requests did not overlap"
        );
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "four 150ms image requests serialized: {elapsed:?}"
        );
        assert_eq!(
            rt.state
                .borrow()
                .page_in_flight
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn image_lifecycle_cache_is_separated_by_cors_credentials_profile() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let server_requests = requests.clone();
        let png = two_by_three_png();
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 4096];
                let read = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("")
                    .to_string();
                let mode = request
                    .lines()
                    .find_map(|line| line.strip_prefix("sec-fetch-mode: "))
                    .unwrap_or("")
                    .trim()
                    .to_string();
                server_requests.lock().unwrap().push((path.clone(), mode));
                let cors_headers = if path == "/cors.png" {
                    // Anonymous accepts wildcard; use-credentials must reject
                    // it even when credentials permission is also present.
                    "Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Credentials: true\r\n"
                } else {
                    ""
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\n{cors_headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    png.len()
                );
                stream.write_all(response.as_bytes()).unwrap();
                stream.write_all(&png).unwrap();
            }
        });

        let base = format!("http://{address}");
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(parse_html(&format!(r#"<img id="image" src="{base}/plain.png">"#)));
        // Deliberately make the image cross-origin from the document.
        rt.set_url("http://127.0.0.1:1/page.html");
        rt.set_http_client(std::sync::Arc::new(
            obscura_net::ObscuraHttpClient::with_full_options(
                std::sync::Arc::new(obscura_net::CookieJar::new()),
                None,
                true,
            ),
        ));
        rt.run_page_init();
        rt.execute_script(
            "observe-profiled-image",
            r#"
                globalThis.image = document.getElementById("image");
                globalThis.__profileEvents = [];
                image.addEventListener("load", () => __profileEvents.push("load"));
                image.addEventListener("error", () => __profileEvents.push("error"));
                void image.complete;
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("[image.complete, image.naturalWidth, __profileEvents]")
                .unwrap(),
            serde_json::json!([true, 2, ["load"]])
        );

        rt.execute_script("require-anonymous-cors", r#"image.crossOrigin = "anonymous";"#)
            .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("[image.complete, image.naturalWidth, __profileEvents]")
                .unwrap(),
            serde_json::json!([true, 0, ["load", "error"]]),
            "URL-keyed no-CORS bytes must not satisfy an anonymous CORS request"
        );

        rt.execute_script("restore-no-cors", "image.removeAttribute('crossorigin');")
            .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("[image.complete, image.naturalWidth, __profileEvents]")
                .unwrap(),
            serde_json::json!([true, 2, ["load", "error", "load"]]),
            "a CORS failure must not poison the earlier no-CORS success"
        );

        rt.execute_script(
            "load-anonymous-cors",
            &format!(r#"image.crossOrigin = "anonymous"; image.src = "{base}/cors.png";"#),
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("[image.complete, image.naturalWidth, __profileEvents]")
                .unwrap(),
            serde_json::json!([true, 2, ["load", "error", "load", "load"]])
        );

        rt.execute_script(
            "require-credentialed-cors",
            r#"image.crossOrigin = "use-credentials";"#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("[image.complete, image.naturalWidth, __profileEvents]")
                .unwrap(),
            serde_json::json!([true, 0, ["load", "error", "load", "load", "error"]]),
            "anonymous CORS success must not satisfy use-credentials"
        );

        rt.execute_script(
            "restore-anonymous-cors",
            r#"image.crossOrigin = "anonymous";"#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("[image.complete, image.naturalWidth, __profileEvents]")
                .unwrap(),
            serde_json::json!([true, 2, ["load", "error", "load", "load", "error", "load"]]),
            "credentialed CORS failure must not poison anonymous success"
        );

        assert_eq!(
            *requests.lock().unwrap(),
            vec![
                ("/plain.png".to_string(), "no-cors".to_string()),
                ("/plain.png".to_string(), "cors".to_string()),
                ("/cors.png".to_string(), "cors".to_string()),
                ("/cors.png".to_string(), "cors".to_string()),
            ]
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn parser_image_data_src_mutation_does_not_restart_lifecycle() {
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requests.clone();
        let png = two_by_three_png();
        let mut rt = parser_image_runtime(
            r#"<img id="image" src="real.png" data-src="deferred.png">"#,
            move |url: &str| {
                seen.lock().unwrap().push(url.to_string());
                Some(png.clone())
            },
        );
        rt.execute_script(
            "observe-data-src-mutation",
            r#"
                globalThis.__dataSrcEvents = [];
                const image = document.getElementById("image");
                image.addEventListener("load", () => __dataSrcEvents.push(image.currentSrc));
                void image.complete;
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("[image.complete, image.currentSrc, __dataSrcEvents]")
                .unwrap(),
            serde_json::json!([
                true,
                "http://example.com/page/real.png",
                ["http://example.com/page/real.png"]
            ])
        );

        rt.execute_script(
            "mutate-non-source-data-attribute",
            r#"
                image.dataset.src = "ignored.png";
                globalThis.__afterDataSrcMutation = [image.complete, image.currentSrc];
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("[__afterDataSrcMutation, __dataSrcEvents]")
                .unwrap(),
            serde_json::json!([
                [true, "http://example.com/page/real.png"],
                ["http://example.com/page/real.png"]
            ])
        );
        assert_eq!(
            *requests.lock().unwrap(),
            vec!["http://example.com/page/real.png".to_string()]
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn parser_image_data_src_is_inert_until_script_assigns_src() {
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requests.clone();
        let png = two_by_three_png();
        let mut rt = parser_image_runtime(
            r#"<img id="image" data-src="promoted.png">"#,
            move |url: &str| {
                seen.lock().unwrap().push(url.to_string());
                Some(png.clone())
            },
        );
        assert_eq!(
            rt.evaluate(
                r#"(() => {
                    globalThis.image = document.getElementById("image");
                    globalThis.__promotedEvents = [];
                    image.addEventListener("load", () => __promotedEvents.push(image.currentSrc));
                    return [image.src, image.currentSrc, image.complete,
                            image.naturalWidth, image.naturalHeight];
                })()"#,
            )
            .unwrap(),
            serde_json::json!(["", "", true, 0, 0])
        );
        rt.run_event_loop_bounded(100).await.unwrap();
        assert!(requests.lock().unwrap().is_empty());

        rt.execute_script(
            "promote-data-src-through-page-script",
            r#"
                image.src = image.dataset.src;
                globalThis.__afterSrcPromotion = [image.complete, image.currentSrc];
            "#,
        )
        .unwrap();
        assert_eq!(
            rt.evaluate("__afterSrcPromotion").unwrap(),
            serde_json::json!([false, "http://example.com/page/promoted.png"])
        );
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate(
                "[image.complete, image.naturalWidth, image.naturalHeight, \
                  image.currentSrc, __promotedEvents]"
            )
            .unwrap(),
            serde_json::json!([
                true,
                2,
                3,
                "http://example.com/page/promoted.png",
                ["http://example.com/page/promoted.png"]
            ])
        );
        assert_eq!(
            *requests.lock().unwrap(),
            vec!["http://example.com/page/promoted.png".to_string()]
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn parser_image_lifecycle_uses_shared_render_resource() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let loader_calls = calls.clone();
        let png = two_by_three_png();
        let mut rt = parser_image_runtime(
            r#"<img id="hero" src="../assets/hero.png">"#,
            move |_url: &str| {
                loader_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Some(png.clone())
            },
        );
        rt.execute_script(
            "observe-parser-image",
            r#"
                globalThis.__imageEvents = [];
                globalThis.__decodeState = "pending";
                const image = document.getElementById("hero");
                globalThis.__imageInitial = [
                    image instanceof HTMLImageElement,
                    image.complete,
                    image.naturalWidth,
                    image.naturalHeight
                ];
                image.addEventListener("load", () => __imageEvents.push("load"));
                image.addEventListener("error", () => __imageEvents.push("error"));
                image.decode().then(
                    () => { __decodeState = "resolved"; },
                    error => { __decodeState = error.name; }
                );
            "#,
        )
        .unwrap();
        assert_eq!(
            rt.evaluate("__imageInitial").unwrap(),
            serde_json::json!([true, false, 0, 0])
        );

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate(
                r#"[
                    image.complete,
                    image.naturalWidth,
                    image.naturalHeight,
                    image.currentSrc,
                    __imageEvents,
                    __decodeState
                ]"#,
            )
            .unwrap(),
            serde_json::json!([
                true,
                2,
                3,
                "http://example.com/assets/hero.png",
                ["load"],
                "resolved"
            ])
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        // A subsequent request for the same URL is served by the retained
        // renderer bytes rather than calling the loader again.
        rt.execute_script("reload-image", r#"image.src = "../assets/hero.png";"#)
            .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn parser_image_failure_completes_and_rejects_decode() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let loader_calls = calls.clone();
        let mut rt = parser_image_runtime(
            r#"<img id="broken" src="missing.png">"#,
            move |_url: &str| {
                loader_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                None
            },
        );
        rt.execute_script(
            "observe-broken-image",
            r#"
                globalThis.__brokenEvents = [];
                globalThis.__brokenDecode = "pending";
                const broken = document.getElementById("broken");
                broken.onload = () => __brokenEvents.push("load");
                broken.onerror = () => __brokenEvents.push("error");
                broken.decode().then(
                    () => { __brokenDecode = "resolved"; },
                    error => { __brokenDecode = error.name; }
                );
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate(
                "[broken.complete, broken.naturalWidth, broken.naturalHeight, \
                  __brokenEvents, __brokenDecode]"
            )
            .unwrap(),
            serde_json::json!([true, 0, 0, ["error"], "EncodingError"])
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn parser_image_first_getter_observes_prepare_seeded_cache_synchronously() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let loader_calls = calls.clone();
        let png = two_by_three_png();
        let mut rt = parser_image_runtime(
            r#"<img id="cached" src="cached.png">"#,
            move |_url: &str| {
                loader_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Some(png.clone())
            },
        );
        {
            let mut state = rt.state.borrow_mut();
            assert!(ensure_prepared_render(&mut state).is_some());
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        // Constructing the JS wrapper happens here, after prepare_dom loaded
        // the resource. The first complete getter must see that cache hit
        // immediately; it must not briefly regress to pending/zero.
        assert_eq!(
            rt.evaluate(
                r#"(() => {
                    const cached = document.getElementById("cached");
                    return [
                        cached.complete,
                        cached.naturalWidth,
                        cached.naturalHeight,
                        cached.currentSrc
                    ];
                })()"#,
            )
            .unwrap(),
            serde_json::json!([true, 2, 3, "http://example.com/page/cached.png"])
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn parser_image_fallback_invalidates_only_new_intrinsic_geometry() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let loader_calls = calls.clone();
        let png = two_by_three_png();
        let mut rt = parser_image_runtime(
            r#"<img id="late" src="late.png">"#,
            move |_url: &str| {
                loader_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Some(png.clone())
            },
        );
        {
            let mut state = rt.state.borrow_mut();
            let previous = state.render_resources.set_sync_loading_enabled(false);
            assert!(ensure_prepared_render(&mut state).is_some());
            state
                .render_resources
                .set_sync_loading_enabled(previous);
            assert!(state.prepared_render.is_some());
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);

        rt.execute_script(
            "load-image-after-layout",
            r#"
                globalThis.__lateEvents = [];
                const late = document.getElementById("late");
                late.addEventListener("load", () => __lateEvents.push("load"));
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate(
                "[late.complete, late.naturalWidth, late.naturalHeight, __lateEvents]"
            )
            .unwrap(),
            serde_json::json!([true, 2, 3, ["load"]])
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        {
            let state = rt.state.borrow();
            assert!(state.prepared_render.is_some());
            assert_eq!(
                state.pending_style_mutations,
                vec![obscura_render::RetainedStyleMutation::Resource]
            );
        }

        // Once the successful dimensions are retained, another loading-form
        // metadata probe is only a cache hit and must preserve fresh layout.
        {
            let mut state = rt.state.borrow_mut();
            assert!(ensure_prepared_render(&mut state).is_some());
            assert!(state.prepared_render.is_some());
        }
        rt.execute_script(
            "reload-retained-image",
            r#"late.src = "late.png";"#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(rt.state.borrow().prepared_render.is_some());

        let missing_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let missing_loader_calls = missing_calls.clone();
        let mut missing = parser_image_runtime(
            r#"<img id="missing" src="missing.png">"#,
            move |_url: &str| {
                missing_loader_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                None
            },
        );
        {
            let mut state = missing.state.borrow_mut();
            let previous = state.render_resources.set_sync_loading_enabled(false);
            assert!(ensure_prepared_render(&mut state).is_some());
            state
                .render_resources
                .set_sync_loading_enabled(previous);
            assert!(state.prepared_render.is_some());
        }
        missing
            .execute_script(
                "fail-image-after-layout",
                r#"
                    globalThis.__missingEvents = [];
                    const missing = document.getElementById("missing");
                    missing.addEventListener("error", () => __missingEvents.push("error"));
                "#,
            )
            .unwrap();
        missing.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            missing
                .evaluate(
                    "[missing.complete, missing.naturalWidth, missing.naturalHeight, __missingEvents]"
                )
                .unwrap(),
            serde_json::json!([true, 0, 0, ["error"]])
        );
        assert_eq!(
            missing_calls.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert!(missing.state.borrow().prepared_render.is_some());
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn stable_cached_image_getters_do_not_queue_resize_geometry_work() {
        let png = two_by_three_png();
        let mut rt = parser_image_runtime(
            r#"<img id="cached" src="cached.png">
               <div id="probe" style="width:40px;height:20px"></div>"#,
            move |_url: &str| Some(png.clone()),
        );
        rt.execute_script(
            "settle-cached-image",
            r#"
                const cached = document.getElementById("cached");
                void cached.complete;
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("[cached.complete, cached.naturalWidth, cached.naturalHeight]")
                .unwrap(),
            serde_json::json!([true, 2, 3])
        );

        rt.execute_script(
            "observe-unrelated-geometry",
            r#"
                globalThis.__stableGetterResizeRecords = 0;
                globalThis.__stableGetterObserver = new ResizeObserver(entries => {
                    __stableGetterResizeRecords += entries.length;
                });
                __stableGetterObserver.observe(document.getElementById("probe"));
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate(
                "[__stableGetterResizeRecords, __obscura_nextPendingTimeoutDelay()]"
            )
            .unwrap(),
            serde_json::json!([1, -1])
        );

        rt.execute_script(
            "read-stable-image-cache",
            r#"
                for (let i = 0; i < 50; i++) {
                    void cached.complete;
                    void cached.currentSrc;
                    void cached.naturalWidth;
                    void cached.naturalHeight;
                }
            "#,
        )
        .unwrap();
        // Cached lifecycle reads do not change intrinsic dimensions, so they
        // must not enqueue a rendering checkpoint (and its geometry walk).
        assert_eq!(
            rt.evaluate(
                "[__stableGetterResizeRecords, __obscura_nextPendingTimeoutDelay()]"
            )
            .unwrap(),
            serde_json::json!([1, -1])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn parser_image_source_replacement_cancels_queued_completion() {
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requests.clone();
        let first = two_by_three_png();
        use base64::Engine as _;
        let second = base64::engine::general_purpose::STANDARD
            .decode(
                "iVBORw0KGgoAAAANSUhEUgAAAAQAAAAFCAYAAABirU3b\
                 AAAAFUlEQVR4nGNk+M/wnwEJMDGgATIEAKVaAgg/Jbt7AAAAAElFTkSuQmCC"
                    .replace(char::is_whitespace, ""),
            )
            .unwrap();
        let mut rt = parser_image_runtime(r#"<img id="swap" src="old.png">"#, move |url: &str| {
            seen.lock().unwrap().push(url.to_string());
            if url.ends_with("/new.png") {
                Some(second.clone())
            } else {
                Some(first.clone())
            }
        });
        rt.execute_script(
            "replace-image-source",
            r#"
                globalThis.__swapEvents = [];
                const swap = document.getElementById("swap");
                swap.addEventListener("load", () => __swapEvents.push(swap.currentSrc));
                swap.src = "new.png";
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate(
                "[swap.complete, swap.naturalWidth, swap.naturalHeight, \
                  swap.currentSrc, __swapEvents]"
            )
            .unwrap(),
            serde_json::json!([
                true,
                4,
                5,
                "http://example.com/page/new.png",
                ["http://example.com/page/new.png"]
            ])
        );
        assert_eq!(
            *requests.lock().unwrap(),
            vec!["http://example.com/page/new.png".to_string()]
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn responsive_picture_lifecycle_tracks_viewport_density_and_source_media() {
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requests.clone();
        let png = two_by_three_png();
        let mut rt = parser_image_runtime(
            r#"
                <picture>
                    <source type="image/avif" srcset="unsupported.avif">
                    <source id="wide-source" media="(min-width: 800px)"
                            srcset="wide.png 2x">
                    <source media="(max-width: 799px)" srcset="narrow.png">
                    <img id="responsive-picture" src="fallback.png">
                </picture>
            "#,
            move |url: &str| {
                seen.lock().unwrap().push(url.to_string());
                Some(png.clone())
            },
        );
        rt.set_viewport(1000.0, 600.0);
        rt.execute_script(
            "observe-responsive-picture",
            r#"
                globalThis.__pictureLoads = [];
                const pictureImage = document.getElementById("responsive-picture");
                pictureImage.addEventListener("load", () => {
                    __pictureLoads.push(pictureImage.currentSrc);
                });
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate(
                "[pictureImage.currentSrc, pictureImage.naturalWidth, \
                  pictureImage.naturalHeight, __pictureLoads]"
            )
            .unwrap(),
            serde_json::json!([
                "http://example.com/page/wide.png",
                1,
                2,
                ["http://example.com/page/wide.png"]
            ])
        );

        // A live viewport change re-runs the renderer's media/source
        // selection. The cache-only complete getter must report pending but
        // must not perform the load itself.
        rt.set_viewport(600.0, 600.0);
        assert_eq!(
            rt.evaluate("pictureImage.complete").unwrap(),
            serde_json::json!(false)
        );
        assert_eq!(requests.lock().unwrap().len(), 1);
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate(
                "[pictureImage.currentSrc, pictureImage.naturalWidth, \
                  pictureImage.naturalHeight, __pictureLoads]"
            )
            .unwrap(),
            serde_json::json!([
                "http://example.com/page/narrow.png",
                2,
                3,
                [
                    "http://example.com/page/wide.png",
                    "http://example.com/page/narrow.png"
                ]
            ])
        );

        // Mutating a <source> selection input invalidates its associated img.
        // The wide bytes are already shared in the render cache, but lifecycle
        // completion remains task-queued and emits one new load event.
        rt.execute_script(
            "mutate-picture-source",
            r#"
                document.getElementById("wide-source").setAttribute("media", "all");
                globalThis.__pictureCompleteAfterSourceMutation = pictureImage.complete;
            "#,
        )
        .unwrap();
        assert_eq!(
            rt.evaluate("__pictureCompleteAfterSourceMutation").unwrap(),
            serde_json::json!(false)
        );
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("[pictureImage.currentSrc, __pictureLoads]")
                .unwrap(),
            serde_json::json!([
                "http://example.com/page/wide.png",
                [
                    "http://example.com/page/wide.png",
                    "http://example.com/page/narrow.png",
                    "http://example.com/page/wide.png"
                ]
            ])
        );
        assert_eq!(
            *requests.lock().unwrap(),
            vec![
                "http://example.com/page/wide.png".to_string(),
                "http://example.com/page/narrow.png".to_string(),
            ]
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn responsive_srcset_sizes_uses_renderer_selected_current_src() {
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requests.clone();
        let png = two_by_three_png();
        let mut rt = parser_image_runtime(
            r#"<img id="responsive-srcset" src="fallback.png"
                     srcset="small.png 400w, large.png 800w" sizes="400px">"#,
            move |url: &str| {
                seen.lock().unwrap().push(url.to_string());
                Some(png.clone())
            },
        );
        rt.execute_script(
            "observe-responsive-srcset",
            r#"
                globalThis.__srcsetLoads = [];
                const srcsetImage = document.getElementById("responsive-srcset");
                srcsetImage.addEventListener("load", () => {
                    __srcsetLoads.push(srcsetImage.currentSrc);
                });
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("[srcsetImage.currentSrc, __srcsetLoads]")
                .unwrap(),
            serde_json::json!([
                "http://example.com/page/small.png",
                ["http://example.com/page/small.png"]
            ])
        );

        change_srcset_image_sizes(&mut rt);
        assert_eq!(
            rt.evaluate("srcsetImage.complete").unwrap(),
            serde_json::json!(false)
        );
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("[srcsetImage.currentSrc, __srcsetLoads]")
                .unwrap(),
            serde_json::json!([
                "http://example.com/page/large.png",
                [
                    "http://example.com/page/small.png",
                    "http://example.com/page/large.png"
                ]
            ])
        );
        assert_eq!(
            *requests.lock().unwrap(),
            vec![
                "http://example.com/page/small.png".to_string(),
                "http://example.com/page/large.png".to_string(),
            ]
        );
    }

    #[cfg(feature = "render")]
