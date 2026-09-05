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
    #[test]
    pub(crate) fn autocomplete_attribute_retains_prepared_render_until_geometry_flush() {
        let dom = parse_html(
            r#"<html style="margin:0"><head><style>
                input { display:block; width:40px; height:20px }
                input[autocomplete="off"] { width:90px }
            </style></head><body style="margin:0">
                <input id="field" autocomplete="on">
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();

        assert_eq!(
            rt.evaluate("document.getElementById('field').getBoundingClientRect().width")
                .unwrap()
                .as_f64(),
            Some(40.0)
        );
        assert!(rt.state.borrow().prepared_render.is_some());

        rt.evaluate("document.getElementById('field').setAttribute('autocomplete', 'off')")
            .unwrap();
        {
            let state = rt.state.borrow();
            assert!(
                state.prepared_render.is_some(),
                "ordinary selector attributes must retain the prepared render until flush"
            );
            assert!(matches!(
                state.pending_style_mutations.as_slice(),
                [obscura_render::RetainedStyleMutation::Attribute(
                    obscura_render::AttributeStyleMutation { name, old_value, new_value, .. }
                )] if name == "autocomplete"
                    && old_value.as_deref() == Some("on")
                    && new_value.as_deref() == Some("off")
            ));
        }

        assert_eq!(
            rt.evaluate("document.getElementById('field').getBoundingClientRect().width")
                .unwrap()
                .as_f64(),
            Some(90.0),
            "the retained selector invalidation must observe the new attribute value"
        );
        assert!(rt.state.borrow().pending_style_mutations.is_empty());
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn namespaced_attribute_mutations_participate_in_id_and_render_invalidation() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0"><div id="box" class="box" style="height:30px;width:40px"></div></body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();

        assert_eq!(
            rt.evaluate("document.getElementById('box').getBoundingClientRect().height")
                .unwrap()
                .as_f64(),
            Some(30.0)
        );
        assert!(rt.state.borrow().prepared_render.is_some());

        rt.evaluate(
            "document.getElementById('box').setAttributeNS(null, 'class', 'box')",
        )
        .unwrap();
        assert!(
            rt.state.borrow().prepared_render.is_some(),
            "an identical null-namespace attribute must retain layout"
        );

        rt.evaluate(
            "document.getElementById('box').setAttributeNS(null, 'style', 'height:70px;width:40px')",
        )
        .unwrap();
        assert!(
            rt.state.borrow().prepared_render.is_none(),
            "a connected namespace-aware style mutation must invalidate layout"
        );
        assert_eq!(
            rt.evaluate("document.getElementById('box').getBoundingClientRect().height")
                .unwrap()
                .as_f64(),
            Some(70.0)
        );

        let id_result = rt
            .evaluate(
                "(function(){const box=document.getElementById('box');box.setAttributeNS(null,'id','renamed');const found=document.getElementById('renamed')===box;box.removeAttributeNS(null,'id');return found && document.getElementById('renamed')===null;})()",
            )
            .unwrap();
        assert_eq!(id_result, serde_json::json!(true));
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn stylesheet_index_cache_reuses_sources_but_not_live_cascade_or_viewport() {
        let dom = parse_html(
            r#"<html style="margin:0"><head><style id="sheet">
                .a { width:40px; height:20px }
                .b { width:80px; height:20px }
            </style></head><body style="margin:0">
                <div id="box" class="a"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();

        assert_eq!(
            rt.evaluate("document.getElementById('box').getBoundingClientRect().width")
                .unwrap()
                .as_f64(),
            Some(40.0)
        );
        {
            let state = rt.state.borrow();
            assert_eq!(state.stylesheet_cache.miss_count(), 1);
            assert_eq!(state.stylesheet_cache.hit_count(), 0);
            assert!(state.stylesheet_cache.retained_source_bytes() > 0);
        }

        // The compiled selector index is reusable, but matching and cascade
        // must observe the new class on the live connected element.
        rt.evaluate("document.getElementById('box').className = 'b'")
            .unwrap();
        {
            let state = rt.state.borrow();
            assert!(state.prepared_render.is_some());
            assert_eq!(state.pending_style_mutations.len(), 1);
        }
        assert_eq!(
            rt.evaluate("document.getElementById('box').getBoundingClientRect().width")
                .unwrap()
                .as_f64(),
            Some(80.0)
        );
        {
            let state = rt.state.borrow();
            assert_eq!(state.stylesheet_cache.miss_count(), 1);
            assert_eq!(state.stylesheet_cache.hit_count(), 1);
        }

        // Style text is part of the exact key and cannot reuse stale rules.
        rt.evaluate(
            r#"document.getElementById('sheet').textContent =
                '.b{width:120px;height:20px}@media(min-width:250px){.b{width:160px}}'"#,
        )
        .unwrap();
        assert_eq!(
            rt.evaluate("document.getElementById('box').getBoundingClientRect().width")
                .unwrap()
                .as_f64(),
            Some(120.0)
        );
        {
            let state = rt.state.borrow();
            assert_eq!(state.stylesheet_cache.miss_count(), 2);
            assert_eq!(state.stylesheet_cache.hit_count(), 1);
        }

        // Media-query filtering is viewport-dependent, so an exact source hit
        // at a different viewport must still reparse and reindex.
        rt.set_viewport(300.0, 100.0);
        assert_eq!(
            rt.evaluate("document.getElementById('box').getBoundingClientRect().width")
                .unwrap()
                .as_f64(),
            Some(160.0)
        );
        let state = rt.state.borrow();
        assert_eq!(state.stylesheet_cache.miss_count(), 3);
        assert_eq!(state.stylesheet_cache.hit_count(), 1);
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn connected_tree_mutations_queue_retained_styles_until_geometry_flush() {
        let dom = parse_html(
            r#"<html style="margin:0"><head><style>
                .item{display:block;width:40px;height:12px}
                .item:nth-child(2){width:80px}
            </style></head><body style="margin:0">
                <main id="list"><div id="first" class="item"></div></main>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();

        assert_eq!(
            rt.evaluate("document.getElementById('first').getBoundingClientRect().width")
                .unwrap()
                .as_f64(),
            Some(40.0)
        );
        rt.evaluate(
            "const added=document.createElement('div');added.id='added';added.className='item';document.getElementById('list').appendChild(added)",
        )
        .unwrap();
        {
            let state = rt.state.borrow();
            assert!(state.prepared_render.is_some());
            assert!(matches!(
                state.pending_style_mutations.as_slice(),
                [obscura_render::RetainedStyleMutation::Tree(
                    obscura_render::TreeStyleMutation::Insert { .. }
                )]
            ));
        }
        assert_eq!(
            rt.evaluate("document.getElementById('added').getBoundingClientRect().width")
                .unwrap()
                .as_f64(),
            Some(80.0)
        );
        assert!(rt.state.borrow().pending_style_mutations.is_empty());

        rt.evaluate("document.getElementById('added').style.width='65px'")
            .unwrap();
        {
            let state = rt.state.borrow();
            assert!(state.prepared_render.is_some());
            assert!(matches!(
                state.pending_style_mutations.as_slice(),
                [obscura_render::RetainedStyleMutation::Attribute(
                    obscura_render::AttributeStyleMutation { name, .. }
                )] if name == "style"
            ));
        }
        assert_eq!(
            rt.evaluate("document.getElementById('added').getBoundingClientRect().width")
                .unwrap()
                .as_f64(),
            Some(65.0)
        );
        assert_eq!(
            rt.evaluate("(function(){const added=document.getElementById('added');added.style.width='';return added.getBoundingClientRect().width})()")
                .unwrap()
                .as_f64(),
            Some(80.0)
        );

        rt.evaluate(
            "document.getElementById('list').removeChild(document.getElementById('first'))",
        )
        .unwrap();
        {
            let state = rt.state.borrow();
            assert!(state.prepared_render.is_some());
            assert!(matches!(
                state.pending_style_mutations.as_slice(),
                [obscura_render::RetainedStyleMutation::Tree(
                    obscura_render::TreeStyleMutation::Remove { .. }
                )]
            ));
        }
        assert_eq!(
            rt.evaluate("document.getElementById('added').getBoundingClientRect().width")
                .unwrap()
                .as_f64(),
            Some(40.0)
        );
        assert!(rt.state.borrow().pending_style_mutations.is_empty());
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn root_overflow_clip_preserves_cssom_scroll_range() {
        let dom = parse_html(
            r#"<html style="margin:0;height:100%;overflow:hidden">
                <body style="margin:0;height:100%">
                    <main id="main" style="padding-top:48px">
                        <div style="height:5000px"></div>
                    </main>
                </body>
            </html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(900.0, 1000.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                const main = document.getElementById("main");
                const before = main.getBoundingClientRect();
                scrollTo(0, 99999);
                const after = main.getBoundingClientRect();
                return [
                    before.height,
                    document.documentElement.scrollHeight,
                    document.body.scrollHeight,
                    scrollY,
                    after.top,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!([5048, 5048, 5048, 4048, -4048]));
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn rendered_window_scroll_events_require_actual_movement() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div style="height:1000px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(320.0, 200.0);
        rt.run_page_init();

        let moved = rt
            .evaluate_for_cdp(
                r#"
                new Promise(resolve => {
                    let win = 0, doc = 0;
                    window.addEventListener("scroll", () => win++);
                    document.addEventListener("scroll", () => doc++);
                    window.scrollTo(0, 100);
                    setTimeout(() => resolve([win, doc, window.scrollY]), 5);
                })
                "#,
                true,
                true,
            )
            .await
            .unwrap();
        assert_eq!(moved.value.unwrap(), serde_json::json!([1, 1, 100]));

        rt.evaluate("window.scrollTo(0, 99999)").unwrap();
        rt.run_event_loop_bounded(20).await.unwrap();
        let no_op = rt
            .evaluate_for_cdp(
                r#"
                new Promise(resolve => {
                    let win = 0, doc = 0;
                    window.addEventListener("scroll", () => win++);
                    document.addEventListener("scroll", () => doc++);
                    const before = window.scrollY;
                    window.scrollTo(0, 99999);
                    setTimeout(() => resolve([win, doc, before, window.scrollY]), 5);
                })
                "#,
                true,
                true,
            )
            .await
            .unwrap();
        let values = no_op.value.unwrap();
        let values = values.as_array().expect("array");
        assert_eq!(
            &values[0..2],
            &serde_json::json!([0, 0]).as_array().unwrap()[..]
        );
        assert_eq!(values[2], values[3]);
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn fingerprinted_screen_does_not_invent_a_device_scale_factor() {
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(parse_html("<html><body></body></html>"));
        rt.set_viewport(300.0, 200.0);
        // Force the fingerprint seed whose screen-pool entry is 2560x1440.
        // That physical screen must not silently turn a 1x render surface into
        // a 2x devicePixelContentBoxSize surface.
        rt.execute_script(
            "deterministic-high-resolution-screen",
            "Date.now = () => 0; Math.random = () => 2 / 0xFFFFFFFF;",
        )
        .unwrap();
        rt.run_page_init();

        assert_eq!(
            rt.evaluate("[screen.width, screen.height, devicePixelRatio]")
                .unwrap(),
            serde_json::json!([2560, 1440, 1])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn resize_observer_reports_real_boxes_only_when_selected_size_changes() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="target" style="box-sizing:border-box;width:120px;height:80px;
                     padding:5px 7px;border:2px solid black"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(300.0, 200.0);
        rt.run_page_init();
        rt.execute_script(
            "resize-observer-boxes",
            r#"
                globalThis.__resizeRecords = [];
                globalThis.__resizeObserver = new ResizeObserver(entries => {
                    __resizeRecords.push(...entries.map(entry => ({
                        interfaces: [
                            entry instanceof ResizeObserverEntry,
                            entry.contentBoxSize[0] instanceof ResizeObserverSize,
                            entry.borderBoxSize[0] instanceof ResizeObserverSize,
                            entry.devicePixelContentBoxSize[0] instanceof ResizeObserverSize,
                        ],
                        contentRect: [
                            entry.contentRect.x, entry.contentRect.y,
                            entry.contentRect.width, entry.contentRect.height,
                        ],
                        content: [
                            entry.contentBoxSize[0].inlineSize,
                            entry.contentBoxSize[0].blockSize,
                        ],
                        border: [
                            entry.borderBoxSize[0].inlineSize,
                            entry.borderBoxSize[0].blockSize,
                        ],
                        device: [
                            entry.devicePixelContentBoxSize[0].inlineSize,
                            entry.devicePixelContentBoxSize[0].blockSize,
                        ],
                    })));
                });
                __resizeObserver.observe(document.getElementById("target"));
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(40).await.unwrap();
        assert_eq!(
            rt.evaluate("__resizeRecords").unwrap(),
            serde_json::json!([{
                "interfaces": [true, true, true, true],
                "contentRect": [7, 5, 102, 66],
                "content": [102, 66],
                "border": [120, 80],
                "device": [102, 66],
            }])
        );

        // A style mutation still causes a rendering checkpoint, but unchanged
        // observed geometry must not produce a speculative notification.
        rt.evaluate(r#"document.getElementById("target").style.color = "red""#)
            .unwrap();
        rt.run_event_loop_bounded(40).await.unwrap();
        assert_eq!(
            rt.evaluate("__resizeRecords.length").unwrap().as_f64(),
            Some(1.0)
        );

        rt.evaluate(r#"document.getElementById("target").style.width = "140px""#)
            .unwrap();
        rt.run_event_loop_bounded(40).await.unwrap();
        assert_eq!(
            rt.evaluate(
                "__resizeRecords.map(record => [record.content[0], record.border[0]])"
            )
            .unwrap(),
            serde_json::json!([[102, 120], [122, 140]])
        );
        assert_eq!(
            rt.evaluate("__obscura_nextPendingTimeoutDelay()")
                .unwrap()
                .as_f64(),
            Some(-1.0)
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn resize_observer_batches_unique_targets_into_one_native_layout_read() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="a" style="box-sizing:border-box;width:100px;height:30px;padding:2px 3px;border:1px solid"></div>
                <div id="b" style="box-sizing:border-box;width:110px;height:30px;padding:2px 3px;border:1px solid"></div>
                <div id="c" style="box-sizing:border-box;width:120px;height:30px;padding:2px 3px;border:1px solid"></div>
                <div id="d" style="box-sizing:border-box;width:130px;height:30px;padding:2px 3px;border:1px solid"></div>
                <div id="vertical" style="box-sizing:border-box;width:140px;height:30px;padding:2px 3px;border:1px solid;writing-mode:vertical-rl"></div>
                <div id="hidden" style="display:none;width:50px;height:20px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(400.0, 300.0);
        rt.run_page_init();
        rt.execute_script(
            "batch-resize-observer-targets",
            r#"
                globalThis.__resizeBulkCalls = 0;
                globalThis.__resizeBulkSizes = [];
                globalThis.__resizeLegacyGeometryCalls = 0;
                globalThis.__resizeComputedStyleCalls = 0;
                const nativeBulk = Deno.core.ops.op_resize_observer_measurements;
                const nativeGeometry = Deno.core.ops.op_layout_geometry;
                const nativeComputedStyle = Deno.core.ops.op_computed_style;
                Deno.core.ops.op_resize_observer_measurements = input => {
                    __resizeBulkCalls++;
                    __resizeBulkSizes.push(JSON.parse(input).length);
                    return nativeBulk(input);
                };
                Deno.core.ops.op_layout_geometry = (...args) => {
                    __resizeLegacyGeometryCalls++;
                    return nativeGeometry(...args);
                };
                Deno.core.ops.op_computed_style = (...args) => {
                    __resizeComputedStyleCalls++;
                    return nativeComputedStyle(...args);
                };

                globalThis.__resizeBatchRecords = [];
                const detached = document.createElement("div");
                detached.id = "detached";
                detached.style.cssText = "width:60px;height:20px";
                const targets = ["a", "b", "c", "d", "vertical", "hidden"]
                    .map(id => document.getElementById(id));
                targets.push(detached);
                const observer = new ResizeObserver(entries => {
                    __resizeBatchRecords.push(entries.map(entry => [
                        entry.target.id,
                        entry.contentBoxSize[0].inlineSize,
                        entry.contentBoxSize[0].blockSize,
                        entry.borderBoxSize[0].inlineSize,
                        entry.borderBoxSize[0].blockSize,
                    ]));
                });
                for (const target of targets) observer.observe(target);
                // A second observer of an existing target must share the same
                // native measurement rather than adding it to the batch twice.
                globalThis.__duplicateResizeRecords = 0;
                const duplicate = new ResizeObserver(entries => {
                    __duplicateResizeRecords += entries.length;
                });
                duplicate.observe(targets[2], { box: "border-box" });
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();

        assert_eq!(
            rt.evaluate(
                r#"[
                    __resizeBulkCalls,
                    __resizeBulkSizes,
                    __resizeLegacyGeometryCalls,
                    __resizeComputedStyleCalls,
                    __duplicateResizeRecords,
                    __resizeBatchRecords,
                ]"#,
            )
            .unwrap(),
            serde_json::json!([
                1,
                [7],
                0,
                0,
                1,
                [[
                    ["a", 92, 24, 100, 30],
                    ["b", 102, 24, 110, 30],
                    ["c", 112, 24, 120, 30],
                    ["d", 122, 24, 130, 30],
                    ["vertical", 24, 132, 30, 140],
                    ["hidden", 0, 0, 0, 0],
                    ["detached", 0, 0, 0, 0],
                ]],
            ])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn resize_observer_selected_box_and_viewport_lifecycle_match_chromium() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="target" style="box-sizing:border-box;width:50vw;height:40px;
                     padding:4px;border:2px solid"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();
        rt.execute_script(
            "resize-observer-selected-box",
            r#"
                globalThis.__contentWidths = [];
                globalThis.__borderWidths = [];
                const target = document.getElementById("target");
                globalThis.__contentObserver = new ResizeObserver(entries => {
                    __contentWidths.push(entries[0].contentBoxSize[0].inlineSize);
                });
                globalThis.__borderObserver = new ResizeObserver(entries => {
                    __borderWidths.push(entries[0].borderBoxSize[0].inlineSize);
                });
                __contentObserver.observe(target, { box: "content-box" });
                __borderObserver.observe(target, { box: "border-box" });
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(40).await.unwrap();
        assert_eq!(
            rt.evaluate("[__contentWidths, __borderWidths]").unwrap(),
            serde_json::json!([[88], [100]])
        );

        // A viewport update is a rendering update even without a DOM mutation.
        rt.set_viewport(300.0, 100.0);
        rt.run_event_loop_bounded(40).await.unwrap();
        assert_eq!(
            rt.evaluate("[__contentWidths, __borderWidths]").unwrap(),
            serde_json::json!([[88, 138], [100, 150]])
        );

        // With border-box sizing a thicker border shrinks the content box but
        // leaves the selected border box unchanged.
        rt.evaluate(r#"document.getElementById("target").style.borderWidth = "4px""#)
            .unwrap();
        rt.run_event_loop_bounded(40).await.unwrap();
        assert_eq!(
            rt.evaluate("[__contentWidths, __borderWidths]").unwrap(),
            serde_json::json!([[88, 138, 134], [100, 150]])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn scrolling_does_not_remeasure_resize_observer_targets() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0;height:1000px">
                <div id="probe" style="width:40px;height:20px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();
        rt.execute_script(
            "observe-before-scroll",
            r#"
                globalThis.__scrollResizeRecords = 0;
                globalThis.__scrollResizeObserver = new ResizeObserver(entries => {
                    __scrollResizeRecords += entries.length;
                });
                __scrollResizeObserver.observe(document.getElementById("probe"));
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(40).await.unwrap();
        assert_eq!(
            rt.evaluate("__scrollResizeRecords").unwrap().as_f64(),
            Some(1.0)
        );

        rt.execute_script(
            "count-scroll-geometry-reads",
            r#"
                globalThis.__scrollGeometryReads = 0;
                globalThis.__nativeLayoutGeometry = Deno.core.ops.op_layout_geometry;
                Deno.core.ops.op_layout_geometry = (...args) => {
                    __scrollGeometryReads++;
                    return __nativeLayoutGeometry(...args);
                };
                window.scrollTo(0, 50);
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(40).await.unwrap();
        let result = rt
            .evaluate("[scrollY, __scrollGeometryReads, __scrollResizeRecords]")
            .unwrap();
        rt.execute_script(
            "restore-layout-geometry-op",
            "Deno.core.ops.op_layout_geometry = __nativeLayoutGeometry;",
        )
        .unwrap();
        assert_eq!(result, serde_json::json!([50, 0, 1]));
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn resize_observer_disconnect_is_reusable_and_inline_boxes_are_empty() {
        let dom = parse_html(
            r#"<html><body><div id="first" style="width:40px;height:20px"></div>
                <span id="inline" style="padding:8px;border:2px solid">text</span>
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
                    const observer = new ResizeObserver(entries => {
                        deliveries.push(entries.map(entry => [
                            entry.target.id,
                            entry.contentRect.width,
                            entry.borderBoxSize[0].inlineSize,
                        ]));
                        if (deliveries.length === 1) {
                            observer.disconnect();
                            observer.observe(document.getElementById("inline"));
                        } else {
                            observer.disconnect();
                            resolve([deliveries, __resizeObservers.length]);
                        }
                    });
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
            serde_json::json!([[[["first", 40, 40]], [["inline", 0, 0]]], 0])
        );

        assert_eq!(
            rt.evaluate(
                r#"[
                    (() => { try { new ResizeObserver(null); } catch (e) { return e.name; } })(),
                    (() => { try { new ResizeObserver(() => {}).observe(document); } catch (e) { return e.name; } })(),
                    (() => { try { new ResizeObserver(() => {}).observe(document.body, {box:"margin-box"}); } catch (e) { return e.name; } })(),
                ]"#,
            )
            .unwrap(),
            serde_json::json!(["TypeError", "TypeError", "TypeError"])
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn resize_observer_self_resize_is_depth_bounded_without_timer_spin() {
        let dom = parse_html(
            r#"<html><body><div id="target" style="width:40px;height:20px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();
        rt.execute_script(
            "resize-observer-loop-limit",
            r#"
                globalThis.__resizeCallbacks = 0;
                globalThis.__resizeLoopErrors = 0;
                addEventListener("error", event => {
                    if (event.message === "ResizeObserver loop completed with undelivered notifications.") {
                        __resizeLoopErrors++;
                    }
                });
                const target = document.getElementById("target");
                globalThis.__loopingResizeObserver = new ResizeObserver(() => {
                    __resizeCallbacks++;
                    target.style.width = (40 + __resizeCallbacks) + "px";
                });
                __loopingResizeObserver.observe(target);
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(40).await.unwrap();
        assert_eq!(
            rt.evaluate(
                "[__resizeCallbacks, __resizeLoopErrors, __obscura_nextPendingTimeoutDelay()]"
            )
            .unwrap(),
            serde_json::json!([1, 1, -1])
        );

        // A later external rendering change starts a fresh bounded cycle; the
        // suppressed same-depth observation did not poison future delivery.
        rt.evaluate(r#"document.getElementById("target").style.width = "60px""#)
            .unwrap();
        rt.run_event_loop_bounded(40).await.unwrap();
        assert_eq!(
            rt.evaluate("[__resizeCallbacks, __resizeLoopErrors]").unwrap(),
            serde_json::json!([2, 2])
        );
    }
