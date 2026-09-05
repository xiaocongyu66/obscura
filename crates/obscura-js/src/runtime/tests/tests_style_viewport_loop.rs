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
    pub(crate) fn explicit_viewport_is_distinct_from_fingerprinted_screen() {
        let dom = parse_html("<html><body></body></html>");
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(1024.0, 768.0);
        rt.run_page_init();
        let result = rt
            .evaluate(
                "return [innerWidth, innerHeight, visualViewport.width,\
                         visualViewport.height, screen.width > 0, screen.height > 0];",
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([1024, 768, 1024, 768, true, true])
        );
    }

    #[test]
    pub(crate) fn screen_override_is_independent_live_and_preserves_screen_identity() {
        let dom = parse_html("<html><body></body></html>");
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(1024.0, 768.0);
        rt.run_page_init();
        rt.execute_script(
            "remember-screen",
            "globalThis.__screenBefore = screen;\
             globalThis.__screenSizeBefore = [screen.width, screen.height];",
        )
        .unwrap();

        rt.set_screen_size_override(Some((1440.0, 900.0)), true);
        assert_eq!(
            rt.evaluate(
                "[innerWidth, innerHeight, screen.width, screen.height,\
                  screen.availWidth, screen.availHeight, screen === __screenBefore]"
            )
            .unwrap(),
            serde_json::json!([1024, 768, 1440, 900, 1440, 900, true])
        );

        rt.set_screen_size_override(None, false);
        assert_eq!(
            rt.evaluate(
                "[innerWidth, innerHeight, screen.width === __screenSizeBefore[0],\
                  screen.height === __screenSizeBefore[1],\
                  screen.availHeight === screen.height - 40,\
                  screen === __screenBefore]"
            )
            .unwrap(),
            serde_json::json!([1024, 768, true, true, true, true])
        );
    }

    #[test]
    pub(crate) fn match_media_evaluates_query_lists_conjunctions_ranges_and_orientation() {
        let dom = parse_html("<html><body></body></html>");
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(1280.0, 720.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                return [
                    matchMedia("(min-width: 1024px) and (min-height: 700px)").matches,
                    matchMedia("(min-width: 1024px) and (min-height: 900px)").matches,
                    matchMedia("(max-width: 600px), screen and (orientation: landscape)").matches,
                    matchMedia("not print").matches,
                    matchMedia("not screen").matches,
                    matchMedia("only screen and (width: 1280px) and (height = 720px)").matches,
                    matchMedia("(1000px <= width < 1400px) and (height > 700px)").matches,
                    matchMedia("(orientation: portrait)").matches,
                    matchMedia("(prefers-color-scheme: light) and (pointer: fine) and (hover: hover)").matches,
                    matchMedia("(obscura-unknown-feature: yes)").matches
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([true, false, true, true, false, true, true, false, true, false])
        );
    }

    #[test]
    pub(crate) fn match_media_matches_are_live_across_viewport_resizes() {
        let dom = parse_html("<html><body></body></html>");
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(900.0, 600.0);
        rt.run_page_init();
        assert_eq!(
            rt.evaluate(
                r#"
                return [
                    (globalThis.__wideAndShort = matchMedia(
                        "(min-width: 800px) and (max-height: 700px)"
                    )).matches,
                    (globalThis.__portrait = matchMedia(
                        "(orientation: portrait)"
                    )).matches
                ];
                "#,
            )
            .unwrap(),
            serde_json::json!([true, false])
        );

        rt.set_viewport(600.0, 900.0);
        assert_eq!(
            rt.evaluate(
                "return [__wideAndShort.matches, __portrait.matches,\
                         matchMedia('(max-width: 600px), print').matches];",
            )
            .unwrap(),
            serde_json::json!([false, true, true])
        );
    }

    #[test]
    pub(crate) fn computed_style_access_does_not_get_shadowed_by_inline_style_proxy() {
        let mut rt = setup_runtime(
            r#"<html><body><div id="box" style="opacity:.5;width:40px"></div></body></html>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const box = document.getElementById("box");
                const computed = getComputedStyle(box);
                return [
                    computed.display,
                    computed.visibility,
                    computed.opacity,
                    computed.width,
                    computed.getPropertyValue("display"),
                    computed.getPropertyValue("background-color")
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                "block",
                "visible",
                "0.5",
                "40px",
                "block",
                "rgba(0, 0, 0, 0)"
            ])
        );
    }

    #[test]
    pub(crate) fn hyperlink_content_attributes_reflect_through_the_idl_surface() {
        let mut rt = setup_runtime(
            r#"<html><body>
                <a id="locale" hreflang="en-US" rel="alternate"
                   target="_blank" download="guide.pdf"
                   ping="/audit" referrerpolicy="no-referrer">English</a>
            </body></html>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const link = document.getElementById("locale");
                const initial = [
                    link.hreflang, link.rel, link.target, link.download,
                    link.ping, link.referrerPolicy,
                    link.hreflang.split("-")[1]
                ];
                link.hreflang = "de-DE";
                link.referrerPolicy = "origin";
                return [
                    initial,
                    link.getAttribute("hreflang"),
                    link.getAttribute("referrerpolicy")
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                [
                    "en-US",
                    "alternate",
                    "_blank",
                    "guide.pdf",
                    "/audit",
                    "no-referrer",
                    "US"
                ],
                "de-DE",
                "origin"
            ])
        );
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn ordinary_inline_keeps_computed_sizes_but_uses_content_geometry() {
        let dom = parse_html(
            r#"<html><head><style>
                html,body,p { margin:0 }
                #host { width:300px; font-size:16px; line-height:20px }
                #token {
                    position:relative;
                    width:100%; height:100px;
                    min-width:100%; min-height:100px;
                    max-width:100%; max-height:100px;
                    padding:0 5px; background:red
                }
                #after { position:relative }
                #atomic {
                    display:inline-block; box-sizing:border-box;
                    width:80px; height:30px; padding:0; border:0
                }
                #replaced {
                    display:inline; box-sizing:border-box;
                    width:80px; height:30px; min-width:0; min-height:0;
                    max-width:none; max-height:none; padding:0; border:0
                }
                #items { display:flex }
                #item {
                    display:inline; box-sizing:border-box; flex:none;
                    width:90px; height:25px; padding:0; border:0
                }
            </style></head><body>
                <p id="host">A <code id="token">token</code> <span id="after">after</span></p>
                <span id="atomic"></span>
                <input id="replaced">
                <div id="items"><span id="item"></span></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(400.0, 240.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                const token = document.getElementById("token");
                const after = document.getElementById("after");
                const computed = getComputedStyle(token);
                const rect = token.getBoundingClientRect();
                const afterRect = after.getBoundingClientRect();
                const atomic = document.getElementById("atomic").getBoundingClientRect();
                const replaced = document.getElementById("replaced").getBoundingClientRect();
                const item = document.getElementById("item").getBoundingClientRect();
                return {
                    computed: [
                        computed.width, computed.height,
                        computed.minWidth, computed.minHeight,
                        computed.maxWidth, computed.maxHeight
                    ],
                    rect: [rect.x, rect.y, rect.width, rect.height],
                    after: [afterRect.x, afterRect.y],
                    client: [token.clientWidth, token.clientHeight],
                    clientRects: Array.from(token.getClientRects(), r => [
                        r.x, r.y, r.width, r.height
                    ]),
                    atomic: [atomic.width, atomic.height],
                    replaced: [replaced.width, replaced.height],
                    item: [item.width, item.height, getComputedStyle(item).display]
                };
                "#,
            )
            .unwrap();

        assert_eq!(
            result["computed"],
            serde_json::json!(["100%", "100px", "100%", "100px", "100%", "100px"])
        );
        let rect = result["rect"].as_array().unwrap();
        let token_x = rect[0].as_f64().unwrap();
        let token_y = rect[1].as_f64().unwrap();
        let token_width = rect[2].as_f64().unwrap();
        let token_height = rect[3].as_f64().unwrap();
        assert!(
            token_width > 20.0 && token_width < 100.0,
            "ordinary inline should hug text and padding: {rect:?}"
        );
        assert!(
            token_height < 40.0,
            "ignored block size leaked into geometry"
        );
        assert_eq!(result["client"], serde_json::json!([0, 0]));
        let client_rects = result["clientRects"].as_array().unwrap();
        assert_eq!(client_rects.len(), 1);
        let client_rect = client_rects[0].as_array().unwrap();
        for (actual, expected) in client_rect
            .iter()
            .map(|value| value.as_f64().unwrap())
            .zip([token_x, token_y, token_width, token_height])
        {
            assert!(
                (actual - expected).abs() < 0.001,
                "getClientRects must expose the renderer's inline fragments"
            );
        }
        let after = result["after"].as_array().unwrap();
        assert!(after[0].as_f64().unwrap() >= token_x + token_width - 0.01);
        assert!((after[1].as_f64().unwrap() - token_y).abs() < 0.01);
        assert_eq!(result["atomic"], serde_json::json!([80, 30]));
        assert_eq!(result["replaced"], serde_json::json!([80, 30]));
        assert_eq!(result["item"], serde_json::json!([90, 25, "block"]));
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn computed_style_uses_renderer_stylesheet_cascade_and_invalidates() {
        let dom = parse_html(
            r#"<html><head><style>
                .base {
                    display:flex; position:relative; z-index:7;
                    visibility:hidden; opacity:.35;
                    background-color:rgb(10,20,30); color:rgb(40,50,60);
                    width:120px; height:40px; min-width:20px; max-width:160px;
                    box-sizing:border-box; overflow-x:clip; overflow-y:visible;
                    margin:1px 2px 3px 4px; padding:5px 6px 7px 8px;
                    border:2px solid rgb(70,80,90);
                    flex-direction:column; flex-wrap:wrap;
                    align-items:center; justify-content:space-between;
                    gap:6px 9px; transform:translate(3px,4px);
                }
                .alt { display:grid; width:150px; opacity:.8; }
            </style></head><body><div id="box" class="base"></div></body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(400.0, 200.0);
        rt.run_page_init();

        let initial = rt
            .evaluate(
                r#"
                const box = document.getElementById("box");
                const c = getComputedStyle(box);
                return [
                    c.display, c.position, c.zIndex, c.visibility, c.opacity,
                    c.backgroundColor, c.getPropertyValue("color"),
                    c.width, c.height, c.minWidth, c.maxWidth, c.boxSizing,
                    c.overflowX, c.overflowY,
                    c.marginTop, c.marginRight, c.marginBottom, c.marginLeft,
                    c.paddingTop, c.borderLeftWidth, c.borderLeftColor,
                    c.flexDirection, c.flexWrap, c.alignItems,
                    c.justifyContent, c.rowGap, c.columnGap, c.transform
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            initial,
            serde_json::json!([
                "flex",
                "relative",
                "7",
                "hidden",
                "0.35",
                "rgb(10, 20, 30)",
                "rgb(40, 50, 60)",
                "120px",
                "40px",
                "20px",
                "160px",
                "border-box",
                "clip",
                "visible",
                "1px",
                "2px",
                "3px",
                "4px",
                "5px",
                "2px",
                "rgb(70, 80, 90)",
                "column",
                "wrap",
                "center",
                "space-between",
                "6px",
                "9px",
                "matrix(1, 0, 0, 1, 3, 4)"
            ])
        );

        assert_eq!(
            rt.evaluate(
                r#"
                const box = document.getElementById("box");
                box.className = "alt";
                const c = getComputedStyle(box);
                return [c.display, c.width, c.opacity];
                "#,
            )
            .unwrap(),
            serde_json::json!(["grid", "150px", "0.8"])
        );
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn webkit_truncation_computed_names_use_native_support_and_vendor_prefixes() {
        let dom = parse_html(
            r#"<html><head><style>
              #clamp { display:-webkit-box; -webkit-box-orient:vertical;
                       -webkit-line-clamp:2; overflow:hidden; }
              #legacy { display:-webkit-inline-box; -webkit-box-orient:horizontal; }
            </style></head><body>
              <div id="clamp">one two three four five six seven eight</div>
              <span id="legacy">legacy</span>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(120.0, 200.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                const clamp = getComputedStyle(document.getElementById("clamp"));
                const legacy = getComputedStyle(document.getElementById("legacy"));
                return {
                  supports: [
                    CSS.supports("text-overflow", "ellipsis"),
                    CSS.supports("-webkit-line-clamp", "2"),
                    CSS.supports("-webkit-line-clamp", "0"),
                    CSS.supports("display", "-webkit-box"),
                    CSS.supports("-webkit-box-orient", "vertical")
                  ],
                  clamp: [clamp.display, clamp.webkitLineClamp,
                    clamp.webkitBoxOrient,
                    clamp.getPropertyValue("-webkit-line-clamp")],
                  legacy: [legacy.display, legacy.webkitBoxOrient]
                };
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!({
                "supports": [true, true, false, true, true],
                "clamp": ["flow-root", "2", "vertical", "2"],
                "legacy": ["-webkit-inline-box", "horizontal"]
            })
        );
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn computed_typography_uses_resolved_renderer_values() {
        let dom = parse_html(
            r#"<html><head><style>
                #parent {
                    font-size:20px; line-height:1.5;
                    letter-spacing:-.05em; white-space:pre-wrap;
                    text-align:end
                }
                #child { font-size:10px }
                #zero { letter-spacing:0px; white-space:break-spaces }
            </style></head><body>
                <div id="parent"><span id="child">child</span></div>
                <div id="zero">zero</div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(400.0, 200.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                const sample = id => {
                    const s = getComputedStyle(document.getElementById(id));
                    return [s.lineHeight, s.letterSpacing, s.whiteSpace, s.textAlign];
                };
                return [sample("parent"), sample("child"), sample("zero")];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                ["30px", "-1px", "pre-wrap", "end"],
                ["15px", "-1px", "pre-wrap", "end"],
                ["normal", "normal", "break-spaces", "start"],
            ])
        );
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn computed_style_exposes_cascaded_custom_properties_and_invalidates() {
        let dom = parse_html(
            r#"<html><head><style>
                :root { --inherited-space: 17px; --derived-space: var(--inherited-space); }
                #nav {
                    --r-globalnav-font-size:17px;
                    --local-scale:1.25;
                    font-size:var(--r-globalnav-font-size);
                }
            </style></head><body>
                <nav id="nav"><span id="child"></span></nav>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(400.0, 200.0);
        rt.run_page_init();

        assert_eq!(
            rt.evaluate(
                r#"
                const nav = document.getElementById("nav");
                const child = document.getElementById("child");
                const navStyle = getComputedStyle(nav);
                const childStyle = getComputedStyle(child);
                let enumeratesBase = false;
                for (let i = 0; i < navStyle.length; i++) {
                    if (navStyle.item(i) === "--r-globalnav-font-size")
                        enumeratesBase = true;
                }
                return [
                    navStyle.fontSize,
                    navStyle.getPropertyValue("--r-globalnav-font-size"),
                    parseInt(navStyle.fontSize) /
                        parseInt(navStyle.getPropertyValue("--r-globalnav-font-size")),
                    navStyle.getPropertyValue("--inherited-space"),
                    navStyle.getPropertyValue("--derived-space"),
                    navStyle.getPropertyValue("--local-scale"),
                    childStyle.getPropertyValue("--inherited-space"),
                    childStyle.getPropertyValue("--local-scale"),
                    enumeratesBase
                ];
                "#,
            )
            .unwrap(),
            serde_json::json!(["17px", "17px", 1, "17px", "17px", "1.25", "17px", "1.25", true])
        );

        assert_eq!(
            rt.evaluate(
                r#"
                const nav = document.getElementById("nav");
                const computed = getComputedStyle(nav);
                nav.style.setProperty("--inherited-space", "23px");
                nav.style.fontSize = "19px";
                return [
                    computed.fontSize,
                    computed.getPropertyValue("--inherited-space"),
                    computed.getPropertyValue("--derived-space")
                ];
                "#,
            )
            .unwrap(),
            serde_json::json!(["19px", "23px", "17px"])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn idle_event_loop_flushes_resolved_promise_continuations() {
        let mut rt = setup_runtime("<html><body><div id='state'>pending</div></body></html>");
        rt.execute_script(
            "font-ready",
            "document.fonts.load('normal 1px Example').then(() => {\
                 document.getElementById('state').textContent = 'ready';\
             });",
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("document.getElementById('state').textContent")
                .unwrap(),
            serde_json::json!("ready")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn quiescent_event_loop_does_not_wait_for_analytics_interval() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "quiescent-long-interval",
            "setInterval(() => { globalThis.__analyticsTicks = (globalThis.__analyticsTicks || 0) + 1; }, 1000);",
        )
        .unwrap();

        let started = std::time::Instant::now();
        rt.run_event_loop_until_quiescent(1_000, 50).await.unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_millis(400),
            "a future analytics interval must not consume the full settle budget"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn fixed_duration_event_loop_yields_from_continuously_ready_tasks() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "fixed-duration-continuously-ready",
            "globalThis.__fixedTicks = 0;\
             setInterval(() => { __fixedTicks++; }, 0);",
        )
        .unwrap();

        let started = std::time::Instant::now();
        rt.run_event_loop_bounded(40).await.unwrap();
        let elapsed = started.elapsed();

        assert!(
            elapsed < std::time::Duration::from_millis(300),
            "a continuously-ready queue must return between tasks instead of waiting for the watchdog: {elapsed:?}",
        );
        assert!(
            rt.evaluate("globalThis.__fixedTicks > 0")
                .unwrap()
                .as_bool()
                .unwrap_or(false),
            "the cooperative fixed wait must still execute queued tasks",
        );
        assert_eq!(
            rt.evaluate(
                "(document.body.setAttribute('data-after-fixed-wait', 'usable'), \
                 document.body.getAttribute('data-after-fixed-wait'))",
            )
            .unwrap(),
            serde_json::json!("usable"),
            "the isolate must remain usable after the fixed wait",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn short_observation_deadline_does_not_terminate_the_active_task() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "task-crossing-observation-deadline",
            "globalThis.__longTaskCompleted = false;\
             setTimeout(() => {\
               const end = performance.now() + 600;\
               while (performance.now() < end) {}\
               __longTaskCompleted = true;\
             }, 0);",
        )
        .unwrap();

        let started = std::time::Instant::now();
        rt.run_event_loop_bounded(20).await.unwrap();
        let elapsed = started.elapsed();

        assert!(
            elapsed >= std::time::Duration::from_millis(500)
                && elapsed < std::time::Duration::from_millis(1_500),
            "capture must wait for the active task boundary without becoming unbounded: {elapsed:?}",
        );
        assert_eq!(
            rt.evaluate("globalThis.__longTaskCompleted").unwrap(),
            serde_json::json!(true),
            "a screenshot/readiness deadline must not terminate valid page work",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn adaptive_observation_deadline_does_not_terminate_the_active_task() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "adaptive-task-crossing-observation-deadline",
            "globalThis.__adaptiveLongTaskCompleted = false;\
             setTimeout(() => {\
               const end = performance.now() + 600;\
               while (performance.now() < end) {}\
               __adaptiveLongTaskCompleted = true;\
             }, 0);",
        )
        .unwrap();

        let started = std::time::Instant::now();
        rt.run_event_loop_until_quiescent(20, 10).await.unwrap();
        let elapsed = started.elapsed();

        assert!(
            elapsed >= std::time::Duration::from_millis(500)
                && elapsed < std::time::Duration::from_millis(1_500),
            "adaptive settle must wait for the active task boundary: {elapsed:?}",
        );
        assert_eq!(
            rt.evaluate("globalThis.__adaptiveLongTaskCompleted")
                .unwrap(),
            serde_json::json!(true),
            "adaptive readiness must not terminate valid page work",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn quiescent_event_loop_yields_from_continuously_ready_non_visual_work() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "quiescent-continuously-ready",
            "setInterval(() => {\
                 globalThis.__schedulerTicks = (globalThis.__schedulerTicks || 0) + 1;\
             }, 0);",
        )
        .unwrap();

        let started = std::time::Instant::now();
        rt.run_event_loop_until_quiescent(2_000, 150)
            .await
            .unwrap();
        let elapsed = started.elapsed();

        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "a continuously-ready non-visual scheduler pinned adaptive settle: {elapsed:?}"
        );
        assert!(
            rt.evaluate("globalThis.__schedulerTicks > 0")
                .unwrap()
                .as_bool()
                .unwrap_or(false),
            "the cooperative policy must still drive scheduler work"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn quiescent_event_loop_bounds_a_single_unyielding_callback_drain() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "quiescent-unyielding-task",
            "setTimeout(() => { while (true) {} }, 0);",
        )
        .unwrap();

        let started = std::time::Instant::now();
        rt.run_event_loop_until_quiescent(2_000, 150)
            .await
            .unwrap();
        let elapsed = started.elapsed();

        assert!(
            elapsed >= std::time::Duration::from_millis(SYNCHRONOUS_TASK_FLOOR_MS)
                && elapsed
                    < std::time::Duration::from_millis(
                        SYNCHRONOUS_TASK_FLOOR_MS + 1_500,
                    ),
            "one synchronous callback drain escaped the bounded task allowance: {elapsed:?}"
        );
        assert_eq!(
            rt.evaluate(
                "(document.body.setAttribute('data-after-watchdog', 'usable'), \
                  document.body.getAttribute('data-after-watchdog'))",
            )
            .unwrap(),
            serde_json::json!("usable"),
            "the per-turn watchdog must leave the isolate reusable",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn quiescent_event_loop_retains_delayed_network_and_dom_update() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let client = std::sync::Arc::new(obscura_net::ObscuraHttpClient::with_full_options(
            std::sync::Arc::new(obscura_net::CookieJar::new()),
            None,
            true,
        ));
        let in_flight = rt.state.borrow().page_in_flight.clone();
        in_flight.store(1, std::sync::atomic::Ordering::SeqCst);
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(80));
            in_flight.store(0, std::sync::atomic::Ordering::SeqCst);
        });
        rt.set_http_client(client);
        rt.execute_script(
            "quiescent-delayed-work",
            "setInterval(() => {}, 1000);\
             setTimeout(() => document.body.setAttribute('data-ready', 'ready'), 40);",
        )
        .unwrap();

        rt.run_event_loop_until_quiescent(1_000, 150).await.unwrap();
        assert_eq!(
            rt.evaluate("document.body.getAttribute('data-ready')")
                .unwrap(),
            serde_json::json!("ready"),
        );
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

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn quiescent_event_loop_allows_fetch_hydration_within_network_grace() {
        let (mut rt, accepted) =
            delayed_fetch_runtime(std::time::Duration::from_millis(700));
        rt.execute_script(
            "quiescent-fetch-hydration",
            "fetch('/hydrate').then(response => response.text()).then(text => {\
                 document.body.setAttribute('data-ready', text);\
             });",
        )
        .unwrap();

        let started = std::time::Instant::now();
        rt.run_event_loop_until_quiescent(3_000, 150)
            .await
            .unwrap();
        let elapsed = started.elapsed();

        accepted
            .recv_timeout(std::time::Duration::from_millis(100))
            .expect("fixture fetch was not issued");
        assert!(
            elapsed >= std::time::Duration::from_millis(650),
            "settle returned before the delayed response: {elapsed:?}"
        );
        assert!(
            elapsed < std::time::Duration::from_millis(1_500),
            "completed hydration should only pay its following quiet window: {elapsed:?}"
        );
        assert_eq!(
            rt.evaluate("document.body.getAttribute('data-ready')")
                .unwrap(),
            serde_json::json!("hydrated"),
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn quiescent_event_loop_bounds_a_hanging_page_request() {
        let (mut rt, accepted) = delayed_fetch_runtime(std::time::Duration::from_secs(3));
        rt.execute_script(
            "quiescent-hanging-fetch",
            "fetch('/analytics').catch(() => {});",
        )
        .unwrap();

        let started = std::time::Instant::now();
        rt.run_event_loop_until_quiescent(4_000, 150)
            .await
            .unwrap();
        let elapsed = started.elapsed();

        accepted
            .recv_timeout(std::time::Duration::from_millis(100))
            .expect("fixture fetch was not issued");
        assert!(
            elapsed >= std::time::Duration::from_millis(900),
            "pending page work must receive the network grace: {elapsed:?}"
        );
        assert!(
            elapsed < std::time::Duration::from_millis(1_700),
            "a hanging request consumed more than its bounded grace: {elapsed:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn quiescent_event_loop_gives_post_grace_dom_activity_a_quiet_window() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.state
            .borrow()
            .page_in_flight
            .store(1, std::sync::atomic::Ordering::SeqCst);
        rt.execute_script(
            "quiescent-post-grace-commit",
            "setInterval(() => {}, 1000);\
             setTimeout(() => document.body.setAttribute('data-ready', 'late'), 1100);",
        )
        .unwrap();

        let started = std::time::Instant::now();
        rt.run_event_loop_until_quiescent(4_000, 150)
            .await
            .unwrap();
        let elapsed = started.elapsed();

        assert_eq!(
            rt.evaluate("document.body.getAttribute('data-ready')")
                .unwrap(),
            serde_json::json!("late"),
        );
        assert!(
            elapsed >= std::time::Duration::from_millis(1_200),
            "the late commit did not receive a following quiet window: {elapsed:?}"
        );
        assert!(
            elapsed < std::time::Duration::from_millis(1_800),
            "late observable work escaped the bounded activity tail: {elapsed:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn quiescence_ignores_another_pages_shared_client_request() {
        let client = std::sync::Arc::new(obscura_net::ObscuraHttpClient::with_full_options(
            std::sync::Arc::new(obscura_net::CookieJar::new()),
            None,
            true,
        ));
        client
            .in_flight
            .store(1, std::sync::atomic::Ordering::SeqCst);
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.set_http_client(client);
        rt.execute_script("quiescent-shared-client", "setInterval(() => {}, 1000);")
            .unwrap();

        let started = std::time::Instant::now();
        rt.run_event_loop_until_quiescent(1_000, 50).await.unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_millis(400),
            "an unrelated page request on the shared client must not pin settle"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn quiescent_event_loop_retains_near_term_render_timeout() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "quiescent-render-timeout",
            "setInterval(() => {}, 1000);\
             setTimeout(() => document.body.setAttribute('data-ready', 'ready'), 200);",
        )
        .unwrap();

        let started = std::time::Instant::now();
        rt.run_event_loop_until_quiescent(1_000, 150).await.unwrap();
        assert!(started.elapsed() >= std::time::Duration::from_millis(180));
        assert_eq!(
            rt.evaluate("document.body.getAttribute('data-ready')")
                .unwrap(),
            serde_json::json!("ready"),
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn quiescent_event_loop_bounds_continuous_visual_mutations() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "quiescent-animated-page",
            "let tick=0;setInterval(() =>\
               document.body.setAttribute('data-frame', String(++tick)), 10);",
        )
        .unwrap();

        let started = std::time::Instant::now();
        rt.run_event_loop_until_quiescent(2_000, 150).await.unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_millis(1_000),
            "an animated document must not consume the complete settle budget"
        );
        assert!(
            rt.evaluate("Number(document.body.getAttribute('data-frame')) > 0")
                .unwrap()
                .as_bool()
                .unwrap_or(false),
            "the policy must still pump animation work before capture"
        );
    }

    #[test]
    pub(crate) fn font_face_set_tracks_authored_and_script_created_faces() {
        let mut rt = setup_runtime(
            r#"<html><head><style>
                @font-face {
                    font-family: "Authored One";
                    src: url("https://assets.test/one.woff2") format("woff2");
                    font-weight: 350 650;
                }
                @font-face {
                    font-family: AuthoredTwo;
                    src: url(data:font/woff2;base64,d09GMg==);
                    font-style: italic;
                }
            </style></head><body></body></html>"#,
        );
        let result = rt
            .evaluate(
                r#"(() => {
                    const authored = Array.from(document.fonts);
                    const cssDelete = document.fonts.delete(authored[0]);
                    const dynamic = new FontFace("Dynamic", "url('/dynamic.ttf')", {
                        style: "oblique 12deg",
                        weight: "700",
                        stretch: "condensed",
                        unicodeRange: "U+20-7E",
                        display: "swap"
                    });
                    const addResult = document.fonts.add(dynamic);
                    const visited = [];
                    document.fonts.forEach((value, key, set) => {
                        visited.push(value === key && set === document.fonts);
                    });
                    const afterAdd = [
                        document.fonts.size,
                        document.fonts.has(dynamic),
                        addResult === document.fonts,
                        dynamic.family,
                        dynamic.style,
                        dynamic.weight,
                        dynamic.stretch,
                        dynamic.unicodeRange,
                        dynamic.display,
                        visited.every(Boolean)
                    ];
                    const deleted = document.fonts.delete(dynamic);
                    document.fonts.clear();
                    const bytes = new Uint8Array([0, 1, 2, 253, 254, 255]);
                    const binary = new FontFace("Binary", bytes, { weight: 600 });
                    return [
                        authored.length,
                        authored.map(face => face.family),
                        cssDelete,
                        afterAdd,
                        deleted,
                        document.fonts.size,
                        binary.status,
                        binary.loaded === binary.load()
                    ];
                })()"#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                2,
                ["Authored One", "AuthoredTwo"],
                false,
                [
                    3,
                    true,
                    true,
                    "Dynamic",
                    "oblique 12deg",
                    "700",
                    "condensed",
                    "U+20-7E",
                    "swap",
                    true
                ],
                true,
                2,
                "loaded",
                true
            ])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn font_face_load_updates_status_set_readiness_and_matching() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "font-face-lifecycle",
            r#"
                globalThis.__fontEvents = [];
                const face = new FontFace("Lifecycle", "url('/lifecycle.woff2')", {
                    weight: "700"
                });
                document.fonts.onloading = event => __fontEvents.push([event.type, event.fontfaces.length]);
                document.fonts.onloadingdone = event => __fontEvents.push([event.type, event.fontfaces.length]);
                document.fonts.add(face);
                globalThis.__fontBefore = [
                    face.status,
                    document.fonts.status,
                    document.fonts.check("700 16px Lifecycle")
                ];
                globalThis.__fontLoadResult = "pending";
                document.fonts.load("700 16px Lifecycle").then(faces => {
                    __fontLoadResult = [faces.length, faces[0] === face, face.status,
                        document.fonts.check("700 16px Lifecycle")];
                });
                document.fonts.ready.then(set => {
                    globalThis.__fontReady = set === document.fonts;
                });
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        let result = rt
            .evaluate("return [__fontBefore, __fontLoadResult, __fontReady, __fontEvents];")
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                ["unloaded", "loaded", false],
                [1, true, "loaded", true],
                true,
                [["loading", 1], ["loadingdone", 1]]
            ])
        );
    }

    #[test]
    pub(crate) fn animation_frame_requires_a_callable_callback() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"(() => {
                    try {
                        requestAnimationFrame(null);
                        return [false, ""];
                    } catch (error) {
                        return [error instanceof TypeError, error.name];
                    }
                })()"#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!([true, "TypeError"]));
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn animation_frames_are_ordered_batches_with_rendering_timestamps() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "animation-frame-order",
            r#"
                globalThis.__rafEvents = [];
                globalThis.__rafStamps = [];
                Promise.resolve().then(() => __rafEvents.push("microtask-before"));
                setTimeout(() => __rafEvents.push("timer"), 1);
                requestAnimationFrame((timestamp) => {
                    __rafEvents.push("raf-a");
                    __rafStamps.push(timestamp);
                    Promise.resolve().then(() => __rafEvents.push("microtask-in-raf"));
                    requestAnimationFrame((nextTimestamp) => {
                        __rafEvents.push("raf-next");
                        __rafStamps.push(nextTimestamp);
                    });
                });
                requestAnimationFrame((timestamp) => {
                    __rafEvents.push("raf-b");
                    __rafStamps.push(timestamp);
                });
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(150).await.unwrap();
        let result = rt
            .evaluate(
                r#"[
                    __rafEvents,
                    __rafStamps.length,
                    __rafStamps[0] === __rafStamps[1],
                    __rafStamps[2] > __rafStamps[1]
                ]"#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                [
                    "microtask-before",
                    "timer",
                    "raf-a",
                    "raf-b",
                    "microtask-in-raf",
                    "raf-next"
                ],
                3,
                true,
                true
            ])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn rendering_opportunity_orders_raf_resize_and_intersection_phases() {
        let mut rt = setup_runtime(
            "<html><body><div id='target' style='width:20px;height:20px'></div></body></html>",
        );
        rt.execute_script(
            "rendering-opportunity-order",
            r#"
                globalThis.__renderPhaseOrder = [];
                const target = document.getElementById("target");
                new ResizeObserver(() => __renderPhaseOrder.push("resize")).observe(target);
                new IntersectionObserver(() => __renderPhaseOrder.push("intersection")).observe(target);
                requestAnimationFrame(() => {
                    __renderPhaseOrder.push("raf");
                    target.style.width = "40px";
                });
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("__renderPhaseOrder.slice(0, 3)").unwrap(),
            serde_json::json!(["raf", "resize", "intersection"]),
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn raf_geometry_mutation_reaches_settled_intersection_before_next_frame() {
        let mut rt = setup_runtime(
            "<html><body style='margin:0'><div id='spacer' style='height:150px'></div><div id='target' style='height:20px'></div></body></html>",
        );
        rt.set_viewport(200.0, 100.0);
        rt.execute_script(
            "settle-intersection",
            r#"
                globalThis.__sameFrameOrder = [];
                globalThis.__sameFrameInitial = false;
                const target = document.getElementById("target");
                globalThis.__sameFrameObserver = new IntersectionObserver(entries => {
                    if (!__sameFrameInitial) {
                        __sameFrameInitial = true;
                        return;
                    }
                    if (entries.some(entry => entry.isIntersecting)) {
                        __sameFrameOrder.push("intersection");
                    }
                });
                __sameFrameObserver.observe(target);
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(50).await.unwrap();

        rt.execute_script(
            "mutate-in-animation-frame",
            r#"
                requestAnimationFrame(() => {
                    __sameFrameOrder.push("raf");
                    document.getElementById("spacer").style.height = "0px";
                    requestAnimationFrame(() => __sameFrameOrder.push("next-raf"));
                });
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(80).await.unwrap();

        assert_eq!(
            rt.evaluate("__sameFrameOrder").unwrap(),
            serde_json::json!(["raf", "intersection", "next-raf"]),
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn cancel_animation_frame_removes_pending_and_current_batch_callbacks() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "animation-frame-cancel",
            r#"
                globalThis.__rafEvents = [];
                const pending = requestAnimationFrame(() => __rafEvents.push("pending"));
                cancelAnimationFrame(pending);
                let sameBatch;
                requestAnimationFrame(() => {
                    __rafEvents.push("first");
                    cancelAnimationFrame(sameBatch);
                });
                sameBatch = requestAnimationFrame(() => __rafEvents.push("same-batch"));
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("__rafEvents").unwrap(),
            serde_json::json!(["first"])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn self_requeueing_animation_frame_yields_to_timer_tasks() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "animation-frame-yield",
            r#"
                globalThis.__rafCount = 0;
                globalThis.__rafStopped = false;
                globalThis.__timerAfterAnimation = false;
                let frameId = 0;
                function frame() {
                    __rafCount++;
                    frameId = requestAnimationFrame(frame);
                }
                frameId = requestAnimationFrame(frame);
                setTimeout(() => {
                    cancelAnimationFrame(frameId);
                    __rafStopped = true;
                }, 55);
                setTimeout(() => {
                    __timerAfterAnimation = true;
                }, 65);
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(200).await.unwrap();
        let result = rt
            .evaluate("[__rafCount, __rafStopped, __timerAfterAnimation]")
            .unwrap();
        let values = result.as_array().unwrap();
        let frame_count = values[0].as_u64().unwrap();
        assert!(
            (2..=5).contains(&frame_count),
            "expected a few paced animation frames before cancellation, got {frame_count}"
        );
        assert_eq!(values[1], serde_json::json!(true));
        assert_eq!(values[2], serde_json::json!(true));
    }
