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
    pub(crate) async fn intersection_observer_tracks_viewport_threshold_crossings() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div style="height:150px"></div>
                <div id="target" style="height:100px"></div>
                <div style="height:300px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();

        let result = rt
            .evaluate_for_cdp(
                r#"
                new Promise(resolve => {
                    const records = [];
                    const target = document.getElementById("target");
                    const observer = new IntersectionObserver(entries => {
                        for (const entry of entries) {
                            records.push([
                                entry.isIntersecting,
                                Math.round(entry.intersectionRatio * 100) / 100,
                                Math.round(entry.boundingClientRect.top),
                                Math.round(entry.intersectionRect.height),
                            ]);
                        }
                    }, { threshold: [0, 0.5, 1] });
                    observer.observe(target);
                    setTimeout(() => window.scrollTo(0, 100), 25);
                    setTimeout(() => window.scrollTo(0, 260), 50);
                    setTimeout(() => resolve(records), 80);
                })
                "#,
                true,
                true,
            )
            .await
            .unwrap();
        assert_eq!(
            result.value.unwrap(),
            serde_json::json!([[false, 0, 150, 0], [true, 0.5, 50, 50], [false, 0, -110, 0],])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn intersection_observer_batches_unique_clip_graph_into_one_native_layout_read() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="root" style="width:200px;height:100px;overflow:hidden">
                    <div id="clip" style="width:150px;height:80px;overflow:auto">
                        <div id="a" style="width:30px;height:20px"></div>
                        <div id="b" style="width:40px;height:20px"></div>
                        <div id="hidden" style="display:none"></div>
                    </div>
                </div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(300.0, 200.0);
        rt.run_page_init();
        rt.execute_script(
            "batch-intersection-observer-clip-graph",
            r#"
                globalThis.__intersectionBulkCalls = 0;
                globalThis.__intersectionBulkSizes = [];
                globalThis.__intersectionLegacyGeometryCalls = 0;
                globalThis.__intersectionComputedStyleCalls = 0;
                const nativeBulk = Deno.core.ops.op_intersection_observer_measurements;
                const nativeGeometry = Deno.core.ops.op_layout_geometry;
                const nativeComputedStyle = Deno.core.ops.op_computed_style;
                Deno.core.ops.op_intersection_observer_measurements = input => {
                    __intersectionBulkCalls++;
                    __intersectionBulkSizes.push(JSON.parse(input).length);
                    return nativeBulk(input);
                };
                Deno.core.ops.op_layout_geometry = (...args) => {
                    __intersectionLegacyGeometryCalls++;
                    return nativeGeometry(...args);
                };
                Deno.core.ops.op_computed_style = (...args) => {
                    __intersectionComputedStyleCalls++;
                    return nativeComputedStyle(...args);
                };

                const root = document.getElementById("root");
                const a = document.getElementById("a");
                const b = document.getElementById("b");
                const hidden = document.getElementById("hidden");
                const detached = document.createElement("div");
                detached.id = "detached";
                globalThis.__intersectionBatchRecords = [];
                const first = new IntersectionObserver(entries => {
                    __intersectionBatchRecords.push(entries.map(entry => [
                        entry.target.id,
                        entry.isIntersecting,
                    ]));
                }, { root });
                const second = new IntersectionObserver(entries => {
                    __intersectionBatchRecords.push(entries.map(entry => [
                        entry.target.id,
                        entry.isIntersecting,
                    ]));
                }, { root });
                first.observe(a);
                first.observe(b);
                first.observe(hidden);
                first.observe(detached);
                // The second observer shares its target, root, and clip ancestor
                // with the first and must not duplicate any native measurements.
                second.observe(b);
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();

        assert_eq!(
            rt.evaluate(
                r#"[
                    __intersectionBulkCalls,
                    __intersectionBulkSizes,
                    __intersectionLegacyGeometryCalls,
                    __intersectionComputedStyleCalls,
                    __intersectionBatchRecords,
                ]"#,
            )
            .unwrap(),
            serde_json::json!([
                1,
                [6],
                0,
                0,
                [
                    [
                        ["a", true],
                        ["b", true],
                        ["hidden", false],
                        ["detached", false],
                    ],
                    [["b", true]],
                ],
            ])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn intersection_observer_delivers_document_batch_before_callback_posted_tasks() {
        let dom = parse_html(
            r#"<html><body><div id="first"></div><div id="second"></div></body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.run_page_init();
        rt.execute_script(
            "intersection-document-delivery-batch",
            r#"
                globalThis.__intersectionDeliveryOrder = [];
                const first = new IntersectionObserver(() => {
                    __intersectionDeliveryOrder.push("first-observer");
                    scheduler.postTask(() => {
                        __intersectionDeliveryOrder.push("callback-posted-task");
                    }, { priority: "user-blocking" });
                });
                const second = new IntersectionObserver(() => {
                    __intersectionDeliveryOrder.push("second-observer");
                });
                first.observe(document.getElementById("first"));
                second.observe(document.getElementById("second"));
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("__intersectionDeliveryOrder").unwrap(),
            serde_json::json!([
                "first-observer",
                "second-observer",
                "callback-posted-task",
            ])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn intersection_observer_element_root_uses_live_padding_box_and_scroll() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="root" style="position:absolute;left:10px;top:20px;width:100px;
                     height:80px;padding:10px;border:5px solid;overflow:auto">
                    <div style="height:100px"></div>
                    <div id="target" style="height:20px"></div>
                </div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(300.0, 200.0);
        rt.run_page_init();

        let result = rt
            .evaluate_for_cdp(
                r#"
                new Promise(resolve => {
                    const records = [];
                    const root = document.getElementById("root");
                    const observer = new IntersectionObserver(entries => {
                        records.push(...entries.map(entry => ({
                            intersecting: entry.isIntersecting,
                            ratio: entry.intersectionRatio,
                            root: [
                                entry.rootBounds.x, entry.rootBounds.y,
                                entry.rootBounds.width, entry.rootBounds.height,
                            ],
                            intersection: [
                                entry.intersectionRect.x, entry.intersectionRect.y,
                                entry.intersectionRect.width, entry.intersectionRect.height,
                            ],
                        })));
                    }, { root, threshold: [0, 1] });
                    observer.observe(document.getElementById("target"));
                    setTimeout(() => { root.scrollTop = 999; }, 25);
                    setTimeout(() => resolve(records), 60);
                })
                "#,
                true,
                true,
            )
            .await
            .unwrap();
        assert_eq!(
            result.value.unwrap(),
            serde_json::json!([
                {
                    "intersecting": false,
                    "ratio": 0,
                    "root": [15, 25, 120, 100],
                    "intersection": [0, 0, 0, 0],
                },
                {
                    "intersecting": true,
                    "ratio": 1,
                    "root": [15, 25, 120, 100],
                    "intersection": [25, 95, 100, 20],
                },
            ])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn intersection_observer_clips_through_intermediate_overflow_ancestors() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="root" style="position:absolute;left:10px;top:20px;
                     width:300px;height:300px;overflow:visible">
                    <div id="clip" style="width:100px;height:100px;overflow:hidden">
                        <div style="height:150px"></div>
                        <div id="target" style="height:20px"></div>
                    </div>
                </div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(400.0, 400.0);
        rt.run_page_init();

        let result = rt
            .evaluate_for_cdp(
                r#"
                new Promise(resolve => {
                    const records = [];
                    const clip = document.getElementById("clip");
                    const observer = new IntersectionObserver(entries => {
                        records.push(...entries.map(entry => [
                            entry.isIntersecting,
                            entry.intersectionRatio,
                            [
                                entry.intersectionRect.x,
                                entry.intersectionRect.y,
                                entry.intersectionRect.width,
                                entry.intersectionRect.height,
                            ],
                        ]));
                    }, {
                        root: document.getElementById("root"),
                        threshold: [0, 1],
                    });
                    observer.observe(document.getElementById("target"));
                    setTimeout(() => { clip.scrollTop = 999; }, 25);
                    setTimeout(() => resolve(records), 60);
                })
                "#,
                true,
                true,
            )
            .await
            .unwrap();
        // Chromium reports the initial target as non-intersecting: although it
        // lies inside the explicit root, the intermediate overflow container
        // clips it. Programmatic scrolling then reveals the complete box.
        assert_eq!(
            result.value.unwrap(),
            serde_json::json!([
                [false, 0, [0, 0, 0, 0]],
                [true, 1, [10, 100, 100, 20]],
            ])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn intersection_observer_initial_geometry_waits_for_one_render_checkpoint() {
        let mut rt = setup_runtime(
            r#"<html><body><div id="first"></div><div id="second"></div></body></html>"#,
        );
        rt.execute_script(
            "intersection-render-checkpoint",
            r#"
                globalThis.__ioOrder = ["sync"];
                globalThis.__ioReads = 0;
                const first = document.getElementById("first");
                const second = document.getElementById("second");
                for (const element of [first, second]) {
                    const nativeRect = element.getBoundingClientRect.bind(element);
                    element.getBoundingClientRect = () => {
                        __ioReads++;
                        return nativeRect();
                    };
                }
                const observer = new IntersectionObserver(
                    () => __ioOrder.push("observer")
                );
                observer.observe(first);
                observer.observe(second);
                Promise.resolve().then(() => __ioOrder.push("microtask"));
                __ioOrder.push("after-observe-" + __ioReads);
            "#,
        )
        .unwrap();

        assert_eq!(
            rt.evaluate("[__ioOrder, __ioReads]").unwrap(),
            serde_json::json!([["sync", "after-observe-0", "microtask"], 0])
        );
        rt.run_event_loop_bounded(100).await.unwrap();
        #[cfg(feature = "render")]
        let expected_geometry_reads = 0;
        #[cfg(not(feature = "render"))]
        let expected_geometry_reads = 2;
        assert_eq!(
            rt.evaluate("[__ioOrder, __ioReads]").unwrap(),
            serde_json::json!([
                ["sync", "after-observe-0", "microtask", "observer"],
                expected_geometry_reads,
            ])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn intersection_observer_honors_root_margin_zero_area_and_no_fake_refires() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0;position:relative">
                <div style="height:110px"></div>
                <div id="margin-target" style="height:10px"></div>
                <div id="zero" style="position:absolute;left:20px;top:50px;width:0;height:0"></div>
                <div id="root" style="height:20px;overflow:auto"></div>
                <div style="height:300px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();

        let result = rt
            .evaluate_for_cdp(
                r#"
                new Promise(resolve => {
                    const marginRecords = [], zeroRecords = [];
                    const marginObserver = new IntersectionObserver(
                        entries => marginRecords.push(...entries.map(entry => [
                            entry.isIntersecting,
                            entry.intersectionRatio,
                            entry.rootBounds.bottom,
                        ])),
                        { rootMargin: "0px 0px 20px", threshold: [0, 1] }
                    );
                    const zeroObserver = new IntersectionObserver(
                        entries => zeroRecords.push(...entries.map(entry => [
                            entry.isIntersecting,
                            entry.intersectionRatio,
                        ]))
                    );
                    marginObserver.observe(document.getElementById("margin-target"));
                    zeroObserver.observe(document.getElementById("zero"));
                    let elementRoot = false;
                    try {
                        const rooted = new IntersectionObserver(() => {}, {
                            root: document.getElementById("root")
                        });
                        elementRoot = rooted.root === document.getElementById("root");
                    } catch (error) {
                        elementRoot = error.name;
                    }
                    setTimeout(() => resolve([
                        marginRecords, zeroRecords, elementRoot,
                        marginObserver.rootMargin, marginObserver.thresholds,
                    ]), 200);
                })
                "#,
                true,
                true,
            )
            .await
            .unwrap();
        assert_eq!(
            result.value.unwrap(),
            serde_json::json!([
                [[true, 1, 120]],
                [[true, 1]],
                true,
                "0px 0px 20px 0px",
                [0, 1],
            ])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn intersection_observer_does_not_refire_while_target_stays_intersecting() {
        let dom = parse_html(
            r#"<html><body>
                <div id="feed"></div>
                <div id="sentinel"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(1280.0, 720.0);
        rt.run_page_init();

        let result = rt
            .evaluate_for_cdp(
                r#"
                new Promise(resolve => {
                    const feed = document.getElementById("feed");
                    let loaded = 0;
                    const observer = new IntersectionObserver(entries => {
                        for (const entry of entries) {
                            if (!entry.isIntersecting) continue;
                            for (let i = 0; i < 10; i++) {
                                const card = document.createElement("div");
                                card.textContent = "Item " + loaded++;
                                feed.appendChild(card);
                            }
                        }
                    });
                    observer.observe(document.getElementById("sentinel"));
                    setTimeout(() => resolve([
                        loaded,
                        feed.querySelectorAll("div").length,
                    ]), 200);
                })
                "#,
                true,
                true,
            )
            .await
            .unwrap();
        assert_eq!(result.value.unwrap(), serde_json::json!([10, 10]));
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn intersection_observer_can_be_reused_after_disconnect() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="stale" style="height:10px"></div>
                <div id="first" style="height:10px"></div>
                <div id="second" style="height:10px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();

        let result = rt
            .evaluate_for_cdp(
                r#"
                new Promise(resolve => {
                    const deliveries = [];
                    const observer = new IntersectionObserver(entries => {
                        deliveries.push(entries.map(entry => entry.target.id));
                        if (deliveries.length === 1) {
                            observer.disconnect();
                            observer.observe(document.getElementById("second"));
                        } else {
                            observer.disconnect();
                            resolve([
                                deliveries,
                                globalThis.__intersectionObservers.length,
                            ]);
                        }
                    });

                    // A pending record from before disconnect must be discarded.
                    observer.observe(document.getElementById("stale"));
                    observer.disconnect();
                    observer.observe(document.getElementById("first"));
                    setTimeout(() => resolve(["timed out"]), 100);
                })
                "#,
                true,
                true,
            )
            .await
            .unwrap();
        assert_eq!(
            result.value.unwrap(),
            serde_json::json!([[["first"], ["second"]], 0,])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn intersection_observer_recomputes_after_style_mutation_and_resize() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="spacer" style="height:150px"></div>
                <div id="target" style="height:20px"></div>
                <div style="height:300px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();
        rt.execute_script(
            "intersection-mutation",
            r#"
                globalThis.__ioRecords = [];
                globalThis.__io = new IntersectionObserver(entries => {
                    __ioRecords.push(...entries.map(entry => [
                        entry.isIntersecting,
                        Math.round(entry.boundingClientRect.top),
                    ]));
                });
                __io.observe(document.getElementById("target"));
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(40).await.unwrap();

        rt.evaluate(r#"document.getElementById("spacer").setAttribute("style", "height:120px")"#)
            .unwrap();
        rt.run_event_loop_bounded(40).await.unwrap();
        assert_eq!(
            rt.evaluate("__ioRecords").unwrap(),
            serde_json::json!([[false, 150]])
        );

        rt.set_viewport(200.0, 160.0);
        rt.run_event_loop_bounded(40).await.unwrap();
        assert_eq!(
            rt.evaluate("__ioRecords").unwrap(),
            serde_json::json!([[false, 150], [true, 120]])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn intersection_observer_recomputes_after_root_scroll() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div style="height:150px"></div>
                <div id="target" style="height:20px"></div>
                <div style="height:300px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();
        rt.execute_script(
            "intersection-root-scroll",
            r#"
                globalThis.__rootScrollIoRecords = [];
                globalThis.__rootScrollIo = new IntersectionObserver(entries => {
                    __rootScrollIoRecords.push(...entries.map(entry => [
                        entry.isIntersecting,
                        Math.round(entry.boundingClientRect.top),
                    ]));
                });
                __rootScrollIo.observe(document.getElementById("target"));
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(40).await.unwrap();
        rt.evaluate("window.scrollTo(0, 100)").unwrap();
        rt.run_event_loop_bounded(40).await.unwrap();
        assert_eq!(
            rt.evaluate("__rootScrollIoRecords").unwrap(),
            serde_json::json!([[false, 150], [true, 50]])
        );
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn scroll_into_view_aligns_the_root_viewport_and_clamps() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div style="height:300px"></div>
                <div id="target" style="height:40px"></div>
                <div style="height:340px"></div>
                <div id="bottom" style="height:20px"></div>
                <div id="fixed" style="position:fixed;top:10px;height:20px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                const target = document.getElementById("target");
                const bottom = document.getElementById("bottom");
                const fixed = document.getElementById("fixed");
                target.scrollIntoView();
                const start = scrollY;
                scrollTo(0, 0);
                target.scrollIntoView({ block: "center" });
                const center = scrollY;
                scrollTo(0, 0);
                target.scrollIntoView({ block: "end" });
                const end = scrollY;
                scrollTo(0, 0);
                target.scrollIntoView({ block: "nearest" });
                const nearestOutside = scrollY;
                scrollTo(0, 250);
                target.scrollIntoView({ block: "nearest" });
                const nearestVisible = scrollY;
                fixed.scrollIntoView();
                const afterFixed = scrollY;
                bottom.scrollIntoView({ block: "start" });
                const clamped = scrollY;
                const max = document.documentElement.scrollHeight - innerHeight;
                return [
                    start, center, end, nearestOutside, nearestVisible,
                    afterFixed, clamped, max,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([300, 270, 240, 240, 250, 250, 600, 600])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn scroll_into_view_emits_events_only_when_the_root_moves() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div style="height:300px"></div>
                <div id="target" style="height:40px"></div>
                <div style="height:300px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();

        rt.evaluate("window.scrollTo(0, 250)").unwrap();
        rt.run_event_loop_bounded(20).await.unwrap();
        let no_op = rt
            .evaluate_for_cdp(
                r#"
                new Promise(resolve => {
                    let win = 0, doc = 0;
                    window.addEventListener("scroll", () => win++);
                    document.addEventListener("scroll", () => doc++);
                    document.getElementById("target").scrollIntoView({ block: "nearest" });
                    setTimeout(() => resolve([win, doc, scrollY]), 5);
                })
                "#,
                true,
                true,
            )
            .await
            .unwrap();
        assert_eq!(no_op.value.unwrap(), serde_json::json!([0, 0, 250]));

        rt.evaluate("window.scrollTo(0, 0)").unwrap();
        rt.run_event_loop_bounded(20).await.unwrap();
        let moved = rt
            .evaluate_for_cdp(
                r#"
                new Promise(resolve => {
                    let win = 0, doc = 0;
                    window.addEventListener("scroll", () => win++);
                    document.addEventListener("scroll", () => doc++);
                    document.getElementById("target").scrollIntoView({ block: "center" });
                    setTimeout(() => resolve([win, doc, scrollY]), 5);
                })
                "#,
                true,
                true,
            )
            .await
            .unwrap();
        assert_eq!(moved.value.unwrap(), serde_json::json!([1, 1, 270]));
    }

    /// Issue #469: FILTER_SKIP leaves a skipped node's children eligible, so
    /// firstChild()/lastChild() must descend into them. FILTER_REJECT must not.
    #[test]
    pub(crate) fn tree_walker_child_movers_descend_on_skip_but_not_on_reject() {
        let mut rt = setup_runtime(r#"<div id="root"><section><a></a><b></b></section></div>"#);
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                function mover(verdict, method) {
                    const w = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT, {
                        acceptNode(node) {
                            return node.tagName === 'SECTION' ? verdict : NodeFilter.FILTER_ACCEPT;
                        }
                    });
                    const found = w[method]();
                    return found ? found.tagName : null;
                }
                return [
                    mover(NodeFilter.FILTER_SKIP, 'firstChild'),
                    mover(NodeFilter.FILTER_SKIP, 'lastChild'),
                    mover(NodeFilter.FILTER_REJECT, 'firstChild'),
                    mover(NodeFilter.FILTER_REJECT, 'lastChild'),
                ];
                "#,
            )
            .unwrap();
        // SKIP descends into <section>; REJECT prunes it and finds nothing else.
        assert_eq!(result, serde_json::json!(["A", "B", null, null]));
    }

    /// Issue #469: nextSibling()/previousSibling() must descend into a skipped
    /// sibling's subtree rather than stepping straight over it.
    #[test]
    pub(crate) fn tree_walker_sibling_movers_descend_into_skipped_siblings() {
        let mut rt = setup_runtime(
            r#"<div id="root"><p id="start"></p><section><a></a></section><q></q></div>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                function mover(verdict, method, from) {
                    const w = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT, {
                        acceptNode(node) {
                            return node.tagName === 'SECTION' ? verdict : NodeFilter.FILTER_ACCEPT;
                        }
                    });
                    w.currentNode = document.getElementById(from);
                    const found = w[method]();
                    return found ? found.tagName : null;
                }
                return [
                    // <section> is skipped, so its child <a> is the next sibling.
                    mover(NodeFilter.FILTER_SKIP, 'nextSibling', 'start'),
                    // Rejected: the subtree is off-limits, so skip past to <q>.
                    mover(NodeFilter.FILTER_REJECT, 'nextSibling', 'start'),
                ];
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!(["A", "Q"]));
    }

    /// Issue #469: the backward sibling mover descends to *last* children.
    #[test]
    pub(crate) fn tree_walker_previous_sibling_descends_to_last_child() {
        let mut rt = setup_runtime(
            r#"<div id="root"><section><a></a><b></b></section><p id="start"></p></div>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                const w = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT, {
                    acceptNode(node) {
                        return node.tagName === 'SECTION'
                            ? NodeFilter.FILTER_SKIP
                            : NodeFilter.FILTER_ACCEPT;
                    }
                });
                w.currentNode = document.getElementById('start');
                const found = w.previousSibling();
                return found ? found.tagName : null;
                "#,
            )
            .unwrap();
        // Reverse order descends to <section>'s last child, not its first.
        assert_eq!(result, serde_json::json!("B"));
    }

    #[test]
    pub(crate) fn append_child_flattens_document_fragment() {
        let mut rt = setup_runtime(r#"<main id="host"></main>"#);
        let result = rt
            .evaluate(
                r#"
                const host = document.getElementById('host');
                const fragment = document.createDocumentFragment();
                const first = document.createElement('article');
                const second = document.createElement('article');
                first.id = 'first';
                second.id = 'second';
                first.className = second.className = 'quote';
                fragment.appendChild(first);
                fragment.appendChild(second);

                const returned = host.appendChild(fragment);
                return [
                    returned === fragment,
                    Array.from(host.children).map(node => node.id),
                    host.querySelectorAll('.quote').length,
                    fragment.childNodes.length,
                    first.parentNode === host,
                    first.parentElement === host,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([true, ["first", "second"], 2, 0, true, true])
        );
    }

    #[test]
    pub(crate) fn insert_before_flattens_document_fragment_in_order() {
        let mut rt = setup_runtime(r#"<main id="host"><article id="last"></article></main>"#);
        let result = rt
            .evaluate(
                r#"
                const host = document.getElementById('host');
                const last = document.getElementById('last');
                const fragment = document.createDocumentFragment();
                const first = document.createElement('article');
                const second = document.createElement('article');
                first.id = 'first';
                second.id = 'second';
                fragment.appendChild(first);
                fragment.appendChild(second);

                const returned = host.insertBefore(fragment, last);
                return [
                    returned === fragment,
                    Array.from(host.children).map(node => node.id),
                    fragment.childNodes.length,
                    first.parentElement === host,
                    second.parentElement === host,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([true, ["first", "second", "last"], 0, true, true])
        );
    }

    #[test]
    pub(crate) fn replace_child_flattens_document_fragment_and_removes_old_child() {
        let mut rt = setup_runtime(
            r#"<main id="host"><article id="old"></article><article id="tail"></article></main>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const host = document.getElementById('host');
                const old = document.getElementById('old');
                const fragment = document.createDocumentFragment();
                const first = document.createElement('article');
                const second = document.createElement('article');
                first.id = 'first';
                second.id = 'second';
                fragment.appendChild(first);
                fragment.appendChild(second);

                const returned = host.replaceChild(fragment, old);
                return [
                    returned === old,
                    Array.from(host.children).map(node => node.id),
                    fragment.childNodes.length,
                    old.parentNode === null,
                    first.parentElement === host,
                    second.parentElement === host,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([true, ["first", "second", "tail"], 0, true, true, true])
        );
    }

    #[test]
    pub(crate) fn test_inner_html() {
        let mut rt = setup_runtime(r#"<div id="x"><p>Hello</p></div>"#);
        let html = rt
            .evaluate("document.getElementById('x').innerHTML")
            .unwrap();
        assert!(html.as_str().unwrap().contains("<p>"));
    }

    #[test]
    pub(crate) fn template_inner_html_preserves_table_fragments() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"
                const template = document.createElement('template');
                template.innerHTML = '<tr><td>first</td><td>second</td></tr>';
                const clone = template.content.firstChild.cloneNode(true);
                return [
                    clone.tagName,
                    clone.firstElementChild.tagName,
                    clone.firstElementChild.children.length,
                    clone.textContent,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!(["TR", "TD", 0, "firstsecond"]));
    }

    #[test]
    pub(crate) fn document_exposes_parent_node_element_children_api() {
        let mut rt = setup_runtime("<html><head></head><body></body></html>");
        let result = rt
            .evaluate(
                "return [document.firstElementChild === document.documentElement,\
                         document.lastElementChild === document.documentElement,\
                         document.children.length, document.childElementCount];",
            )
            .unwrap();
        assert_eq!(result, serde_json::json!([true, true, 1, 1]));
    }

    #[test]
    pub(crate) fn atob_decodes_large_payload_without_argument_stack_overflow() {
        let mut rt = setup_runtime("<html><body></body></html>");
        // 60k four-character groups decode to 180k bytes, comfortably above
        // V8's maximum argument count for a single fromCharCode(...bytes).
        let encoded = "QUFB".repeat(60_000);
        let result = rt.evaluate(&format!("atob('{}').length", encoded)).unwrap();
        assert_eq!(result.as_f64().unwrap() as usize, 180_000);
    }

    #[test]
    pub(crate) fn navigation_api_updates_current_entry_state() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"
                (() => {
                    navigation.updateCurrentEntry({state: {route: 'home'}});
                    const first = navigation.currentEntry;
                    navigation.navigate('/docs', {state: {route: 'docs'}});
                    return [
                        typeof navigation.updateCurrentEntry,
                        first.getState().route,
                        navigation.currentEntry.getState().route,
                        navigation.currentEntry.url,
                    ];
                })()
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!(["function", "home", "docs", "http://example.com/docs"])
        );
    }

    #[test]
    pub(crate) fn inline_stylesheet_cssom_lists_and_rules_are_live_same_objects() {
        let mut rt = setup_runtime(
            r#"<html><head>
                <style id="first">.one { color:red } .two { width:20px }</style>
                <style id="second">.three { display:block }</style>
            </head><body></body></html>"#,
        );
        let result = rt
            .evaluate(
                r#"
                (() => {
                    const list = document.styleSheets;
                    const style = document.getElementById('first');
                    const sheet = style.sheet;
                    const rules = sheet.cssRules;
                    const firstRule = rules[0];
                    const initial = [
                        list === document.styleSheets,
                        list.length,
                        list[0] === sheet,
                        list.item(0) === sheet,
                        list.item(9),
                        sheet.ownerNode === style,
                        rules === sheet.cssRules,
                        rules.length,
                        rules.item(0) === firstRule,
                        firstRule instanceof CSSRule,
                        firstRule instanceof CSSStyleRule,
                        firstRule.type,
                        firstRule.selectorText,
                        firstRule.style.color,
                        firstRule.parentStyleSheet === sheet,
                    ];

                    sheet.insertRule('.middle { height: 30px; }', 1);
                    const inserted = [
                        rules.length,
                        rules[0] === firstRule,
                        rules[1].selectorText,
                        style.textContent.includes('.middle'),
                    ];
                    sheet.deleteRule(1);

                    const extra = document.createElement('style');
                    document.head.appendChild(extra);
                    const afterAppend = list.length;
                    const emptySheet = extra.sheet;
                    const emptyIdentity = emptySheet === list[2]
                        && emptySheet.cssRules.length === 0;
                    extra.textContent = '.extra { opacity:.5 }';
                    const emptyBecameLive = emptySheet.cssRules.length === 1;
                    extra.remove();
                    const afterRemove = list.length;

                    style.textContent = '.replacement { padding: 4px; }';
                    const reparsed = [
                        style.sheet === sheet,
                        sheet.cssRules === rules,
                        rules.length,
                        rules[0].selectorText,
                        rules[0].style.padding,
                    ];
                    style.remove();
                    const disconnected = style.sheet === null
                        && sheet.ownerNode === null
                        && list.length === 1;
                    document.head.appendChild(style);
                    const reconnected = style.sheet !== sheet
                        && style.sheet.ownerNode === style
                        && list.length === 2;

                    const left = document.createElement('div');
                    const right = document.createElement('div');
                    document.body.append(left, right);
                    const moving = document.createElement('style');
                    moving.textContent = '.moving { color: red }';
                    left.appendChild(moving);
                    const beforeMove = moving.sheet;
                    right.appendChild(moving);
                    const reparented = beforeMove.ownerNode === null
                        && moving.sheet !== beforeMove
                        && moving.sheet.ownerNode === moving;
                    right.remove();
                    left.remove();

                    const bulk = document.createElement('div');
                    document.body.appendChild(bulk);
                    bulk.innerHTML = '<style>.bulk { color: blue }</style><span></span>';
                    const bulkSheet = bulk.querySelector('style').sheet;
                    bulk.innerHTML = '';
                    const innerHTMLDetached = bulkSheet.ownerNode === null;
                    bulk.innerHTML = '<section><style>.text { color: green }</style></section>';
                    const textSheet = bulk.querySelector('style').sheet;
                    bulk.textContent = '';
                    const textContentDetached = textSheet.ownerNode === null;
                    bulk.remove();
                    return {
                        initial, inserted, afterAppend, emptyIdentity, emptyBecameLive,
                        afterRemove, reparsed, disconnected, reconnected, reparented,
                        innerHTMLDetached, textContentDetached,
                    };
                })()
                "#,
            )
            .unwrap();

        assert_eq!(
            result,
            serde_json::json!({
                "initial": [true, 2, true, true, null, true, true, 2, true,
                    true, true, 1, ".one", "red", true],
                "inserted": [3, true, ".middle", true],
                "afterAppend": 3,
                "emptyIdentity": true,
                "emptyBecameLive": true,
                "afterRemove": 2,
                "reparsed": [true, true, 1, ".replacement", "4px"],
                "disconnected": true,
                "reconnected": true,
                "reparented": true,
                "innerHTMLDetached": true,
                "textContentDetached": true,
            })
        );
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn stylesheet_cssom_mutations_update_the_live_cascade() {
        let mut rt = setup_runtime(
            r#"<html style="margin:0"><head>
                <style>#box { width:11px; height:10px }</style>
                </head><body style="margin:0"><div id="box"></div></body></html>"#,
        );
        rt.set_viewport(200.0, 100.0);
        let result = rt
            .evaluate(
                r#"
                (() => {
                    const style = document.createElement('style');
                    style.type = 'text/css';
                    style.setAttribute('data-framer-css', 'true');
                    document.head.appendChild(style);
                    const sheet = style.sheet;
                    const rules = sheet.cssRules;
                    const termius = [
                        !!sheet,
                        rules.length,
                        document.styleSheets[1] === sheet,
                        sheet.ownerNode === style,
                    ];
                    sheet.insertRule('#box { width:73px; height:10px; }', rules.length);
                    termius.push(rules.length);
                    sheet.insertRule('.other { color: blue; }', rules.length);
                    termius.push(rules.length);
                    sheet.insertRule('.semicolon { content: "a;b"; background-image: url("data:image/svg+xml;utf8,<svg/>"); }', rules.length);
                    const semicolonValues = [
                        rules[2].style.content,
                        rules[2].style.backgroundImage,
                    ];
                    let multiRuleSyntaxError = false;
                    try {
                        sheet.insertRule('.invalid-a {} .invalid-b {}', rules.length);
                    } catch (error) {
                        multiRuleSyntaxError = error?.name === 'SyntaxError';
                    }
                    const inserted = document.getElementById('box').getBoundingClientRect().width;
                    rules[0].style.setProperty('width', '91px');
                    const edited = document.getElementById('box').getBoundingClientRect().width;
                    const computed = getComputedStyle(document.getElementById('box')).width;
                    sheet.deleteRule(0);
                    const deleted = document.getElementById('box').getBoundingClientRect().width;
                    sheet.replaceSync('#box { width:64px } .other { color: blue }');
                    const replaced = document.getElementById('box').getBoundingClientRect().width;
                    return [termius, semicolonValues, multiRuleSyntaxError, inserted, edited, computed, deleted,
                            replaced, rules.length, rules[0].selectorText,
                            style.textContent.includes('.other')];
                })()
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                [true, 0, true, true, 1, 2],
                ["\"a;b\"", "url(\"data:image/svg+xml;utf8,<svg/>\")"], true,
                73, 91, "91px", 11, 64, 2, "#box", true
            ])
        );
    }

    #[test]
    pub(crate) fn adopted_stylesheets_materialize_into_the_document() {
        let mut rt =
            setup_runtime("<html><head></head><body><div class=\"card\"></div></body></html>");
        let result = rt
            .evaluate(
                r#"
                (() => {
                    const sheet = new CSSStyleSheet();
                    document.adoptedStyleSheets.push(sheet);
                    sheet.insertRule('.card { display: flex; color: red; }', 0);
                    const node = document.querySelector('style[data-obscura-adopted]');
                    const inserted = node.textContent;
                    sheet.replaceSync('.card { content: "a;b"; background-image: url("data:image/svg+xml;utf8,<svg/>"); }');
                    const preserved = [
                        sheet.cssRules[0].style.content,
                        sheet.cssRules[0].style.backgroundImage,
                        node.textContent.includes('a;b'),
                        node.textContent.includes('svg+xml;utf8'),
                    ];
                    sheet.deleteRule(0);
                    return [
                        document.adoptedStyleSheets.length,
                        document.querySelectorAll('style[data-obscura-adopted]').length,
                        inserted.includes('display: flex'),
                        preserved,
                        node.textContent,
                    ];
                })()
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                1, 1, true,
                ["\"a;b\"", "url(\"data:image/svg+xml;utf8,<svg/>\")", true, true],
                ""
            ])
        );
    }

    #[test]
    pub(crate) fn shadow_stylesheet_lists_and_adoption_are_live_across_roots() {
        let mut rt = setup_runtime(
            "<html><head></head><body><div id='one'></div><div id='two'></div></body></html>",
        );
        let result = rt
            .evaluate(
                r#"
                (() => {
                    const first = document.getElementById('one').attachShadow({ mode: 'open' });
                    const second = document.getElementById('two').attachShadow({ mode: 'open' });
                    const inline = document.createElement('style');
                    inline.textContent = '.local { width: 17px }';
                    first.appendChild(inline);
                    const inlineSheet = inline.sheet;
                    const firstList = first.styleSheets;
                    const firstAdopted = first.adoptedStyleSheets;
                    const secondAdopted = second.adoptedStyleSheets;
                    const documentAdopted = document.adoptedStyleSheets;

                    const shared = new CSSStyleSheet();
                    first.adoptedStyleSheets = [shared];
                    second.adoptedStyleSheets.push(shared);
                    document.adoptedStyleSheets = [shared];

                    const firstNode = first.querySelector('style[data-obscura-adopted]');
                    const secondNode = second.querySelector('style[data-obscura-adopted]');
                    const documentNode = document.querySelector('style[data-obscura-adopted]');
                    const initial = [
                        first.styleSheets === firstList,
                        firstList.length,
                        firstList[0] === inlineSheet,
                        firstList.item(0) === inlineSheet,
                        second.styleSheets === second.styleSheets,
                        second.styleSheets.length,
                        first.adoptedStyleSheets === firstAdopted,
                        second.adoptedStyleSheets === secondAdopted,
                        document.adoptedStyleSheets === documentAdopted,
                        firstAdopted.length,
                        secondAdopted.length,
                        documentAdopted.length,
                        firstNode.parentNode === first,
                        secondNode.parentNode === second,
                        documentNode.parentNode === document.head,
                    ];

                    shared.insertRule('.shared { width: 31px }', 0);
                    const synchronized = [firstNode, secondNode, documentNode]
                        .map(node => node.textContent.includes('width: 31px'));

                    second.adoptedStyleSheets = [];
                    shared.replaceSync('.shared { width: 47px }');
                    const afterRemoval = [
                        second.adoptedStyleSheets === secondAdopted,
                        secondAdopted.length,
                        secondNode.parentNode,
                        second.querySelectorAll('style[data-obscura-adopted]').length,
                        firstNode.textContent.includes('width: 47px'),
                        documentNode.textContent.includes('width: 47px'),
                        secondNode.textContent.includes('width: 31px'),
                    ];

                    inline.remove();
                    const inlineRemoval = [
                        first.styleSheets === firstList,
                        firstList.length,
                        inlineSheet.ownerNode,
                    ];
                    return { initial, synchronized, afterRemoval, inlineRemoval };
                })()
                "#,
            )
            .unwrap();

        assert_eq!(
            result,
            serde_json::json!({
                "initial": [
                    true, 1, true, true, true, 0,
                    true, true, true, 1, 1, 1,
                    true, true, true
                ],
                "synchronized": [true, true, true],
                "afterRemoval": [true, 0, null, 0, true, true, true],
                "inlineRemoval": [true, 0, null],
            })
        );
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn shadow_adopted_stylesheets_apply_and_sync_the_live_cascade() {
        let mut rt = setup_runtime(
            r#"<html style="margin:0"><head>
                <style>.target { width:11px; height:10px }</style>
                </head><body style="margin:0">
                <div id="one"></div><div id="two"></div><div class="target" id="outside"></div>
                </body></html>"#,
        );
        rt.set_viewport(200.0, 100.0);
        let result = rt
            .evaluate(
                r#"
                (() => {
                    const first = document.getElementById('one').attachShadow({ mode: 'open' });
                    const second = document.getElementById('two').attachShadow({ mode: 'open' });
                    first.innerHTML = '<style>.target { width:18px; height:10px }</style><div class="target"></div>';
                    second.innerHTML = '<style>.target { width:23px; height:10px }</style><div class="target"></div>';
                    const firstTarget = first.querySelector('.target');
                    const secondTarget = second.querySelector('.target');
                    const outside = document.getElementById('outside');
                    const widths = () => [firstTarget, secondTarget, outside]
                        .map(node => node.getBoundingClientRect().width);

                    const inline = widths();
                    const shared = new CSSStyleSheet();
                    shared.replaceSync('.target { width:42px; height:10px }');
                    first.adoptedStyleSheets = [shared];
                    second.adoptedStyleSheets = [shared];
                    document.adoptedStyleSheets = [shared];
                    const adopted = widths();

                    shared.cssRules[0].style.setProperty('width', '67px');
                    const mutated = widths();

                    second.adoptedStyleSheets = [];
                    document.adoptedStyleSheets = [];
                    const selectivelyRemoved = widths();

                    shared.replaceSync('.target { width:81px; height:10px }');
                    const remainingRootUpdated = widths();
                    return [
                        inline, adopted, mutated, selectivelyRemoved, remainingRootUpdated,
                        first.styleSheets.length,
                        second.styleSheets.length,
                    ];
                })()
                "#,
            )
            .unwrap();

        assert_eq!(
            result,
            serde_json::json!([
                [18, 23, 11],
                [42, 42, 42],
                [67, 67, 67],
                [67, 23, 11],
                [81, 23, 11],
                1,
                1,
            ])
        );
    }
