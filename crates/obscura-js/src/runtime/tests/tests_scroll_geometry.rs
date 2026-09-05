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

    #[cfg(not(feature = "render"))]
    #[test]
    pub(crate) fn window_scroll_methods_move_the_page_offset() {
        let mut rt = setup_runtime(r#"<html><body><div id="d"></div></body></html>"#);
        let result = rt
            .evaluate(
                r#"
                const scrolled = window.scrollTo(0, 500);
                const afterTo = [window.scrollX, window.scrollY];
                window.scrollBy(0, 200);
                const afterBy = [window.pageXOffset, window.pageYOffset];
                window.scrollTo({ left: 10, top: 40 });
                const afterOptions = [window.scrollX, window.scrollY];
                window.scroll(5, 5);
                const afterScroll = [window.scrollX, window.scrollY];
                // Negative offsets clamp to 0, as they do for elements.
                window.scrollTo(0, -100);
                return [afterTo, afterBy, afterOptions, afterScroll, window.scrollY];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([[0, 500], [0, 700], [10, 40], [5, 5], 0])
        );
    }

    /// Issue #468: the page offset is one value, readable and writable through
    /// either `window.scrollY` or `document.scrollingElement.scrollTop`.
    #[cfg(not(feature = "render"))]
    #[test]
    pub(crate) fn window_scroll_offset_is_shared_with_the_scrolling_element() {
        let mut rt = setup_runtime(r#"<html><body><div id="d"></div></body></html>"#);
        let result = rt
            .evaluate(
                r#"
                const isDocEl = document.scrollingElement === document.documentElement;
                window.scrollTo(0, 300);
                // Written through the window, read through the element...
                const viaElement = document.scrollingElement.scrollTop;
                // ...and the reverse.
                document.scrollingElement.scrollTop = 90;
                return [isDocEl, viaElement, window.scrollY, window.pageYOffset];
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!([true, 300, 90, 90]));
    }

    /// Issue #468: a scroll event must reach listeners on both the window and
    /// the document — that is the signal lazy loaders wait for.
    #[cfg(not(feature = "render"))]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn window_scroll_fires_a_scroll_event() {
        let mut rt = setup_runtime(r#"<html><body><div id="d"></div></body></html>"#);
        let result = rt
            .evaluate_for_cdp(
                r#"
                new Promise(resolve => {
                    let win = 0, doc = 0;
                    window.addEventListener('scroll', () => win++);
                    document.addEventListener('scroll', () => doc++);
                    window.scrollBy(0, 400);
                    setTimeout(() => resolve([win, doc, window.scrollY]), 5);
                })
                "#,
                true,
                true,
            )
            .await
            .unwrap();
        assert_eq!(result.value.unwrap(), serde_json::json!([1, 1, 400]));
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn rendered_window_scroll_clamps_and_geometry_is_viewport_relative() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="wide" style="width:600px;height:700px"></div>
                <div id="target" style="width:20px;height:300px"></div>
                <div id="fixed" style="position:fixed;left:12px;top:14px;width:30px;height:25px">
                    <span id="fixed-child">fixed</span>
                </div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(320.0, 200.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                const target = document.getElementById("target");
                const fixed = document.getElementById("fixed");
                const fixedChild = document.getElementById("fixed-child");
                const before = {
                    target: target.getBoundingClientRect(),
                    fixed: fixed.getBoundingClientRect(),
                    fixedChild: fixedChild.getBoundingClientRect(),
                };
                window.scrollTo(99999, 99999);
                const maxX = document.documentElement.scrollWidth - innerWidth;
                const maxY = document.documentElement.scrollHeight - innerHeight;
                const after = {
                    target: target.getBoundingClientRect(),
                    fixed: fixed.getBoundingClientRect(),
                    fixedChild: fixedChild.getBoundingClientRect(),
                };
                return [
                    innerWidth, innerHeight,
                    document.documentElement.clientWidth,
                    document.documentElement.clientHeight,
                    document.documentElement.scrollWidth,
                    document.documentElement.scrollHeight,
                    window.scrollX, window.scrollY,
                    document.scrollingElement.scrollLeft,
                    document.scrollingElement.scrollTop,
                    maxX, maxY,
                    Math.abs(after.target.left - (before.target.left - maxX)) < 0.01,
                    Math.abs(after.target.top - (before.target.top - maxY)) < 0.01,
                    Math.abs(after.fixed.left - before.fixed.left) < 0.01,
                    Math.abs(after.fixed.top - before.fixed.top) < 0.01,
                    Math.abs(after.fixedChild.left - before.fixedChild.left) < 0.01,
                    Math.abs(after.fixedChild.top - before.fixedChild.top) < 0.01,
                ];
                "#,
            )
            .unwrap();
        let values = result.as_array().expect("array");
        assert_eq!(
            &values[0..4],
            &serde_json::json!([320, 200, 320, 200]).as_array().unwrap()[..]
        );
        let scroll_width = values[4].as_f64().expect("scrollWidth");
        let scroll_height = values[5].as_f64().expect("scrollHeight");
        assert!(scroll_width >= 600.0, "scrollWidth was {scroll_width}");
        assert!(scroll_height >= 1000.0, "scrollHeight was {scroll_height}");
        assert_eq!(values[6], values[10]);
        assert_eq!(values[7], values[11]);
        assert_eq!(values[8], values[10]);
        assert_eq!(values[9], values[11]);
        assert!(values[12..]
            .iter()
            .all(|value| value == &serde_json::json!(true)));
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn nested_scroll_metrics_geometry_pixels_and_relayout_share_one_state() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="outer" style="box-sizing:border-box;width:120px;height:100px;
                     border:4px solid red;overflow:hidden;position:relative;background:red">
                  <div id="inner" style="width:220px;height:200px;overflow:hidden;
                       position:relative;background:blue">
                    <div id="target" style="position:absolute;left:300px;top:280px;
                         width:30px;height:20px;background:lime"></div>
                  </div>
                </div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(360.0, 240.0);
        rt.run_page_init();

        let top = rt
            .screenshot_prepared((360.0, 240.0), Some("about:blank"))
            .expect("top screenshot");
        let result = rt
            .evaluate(
                r#"
                const outer = document.getElementById('outer');
                const inner = document.getElementById('inner');
                const target = document.getElementById('target');
                const before = {
                  outer: outer.getBoundingClientRect(),
                  inner: inner.getBoundingClientRect(),
                  target: target.getBoundingClientRect(),
                };
                outer.scrollTo(9999, 9999);
                inner.scrollTo(9999, 9999);
                const after = {
                  outer: outer.getBoundingClientRect(),
                  inner: inner.getBoundingClientRect(),
                  target: target.getBoundingClientRect(),
                };
                const first = [target.getBoundingClientRect().left, target.getBoundingClientRect().top];
                outer.scrollTo(0, 0); inner.scrollTo(0, 0);
                outer.scrollTo(9999, 9999); inner.scrollTo(9999, 9999);
                const repeated = [target.getBoundingClientRect().left, target.getBoundingClientRect().top];
                return {
                  outerMetrics: [outer.clientWidth, outer.clientHeight, outer.scrollWidth, outer.scrollHeight],
                  innerMetrics: [inner.clientWidth, inner.clientHeight, inner.scrollWidth, inner.scrollHeight],
                  offsets: [outer.scrollLeft, outer.scrollTop, inner.scrollLeft, inner.scrollTop],
                  outerDelta: [after.outer.left - before.outer.left, after.outer.top - before.outer.top],
                  innerDelta: [after.inner.left - before.inner.left, after.inner.top - before.inner.top],
                  targetDelta: [after.target.left - before.target.left, after.target.top - before.target.top],
                  repeated: [first[0] === repeated[0], first[1] === repeated[1]],
                };
                "#,
            )
            .expect("nested scroll state");
        assert_eq!(
            result["outerMetrics"],
            serde_json::json!([112, 92, 220, 200])
        );
        assert_eq!(
            result["innerMetrics"],
            serde_json::json!([220, 200, 330, 300])
        );
        assert_eq!(result["offsets"], serde_json::json!([108, 108, 110, 100]));
        assert_eq!(result["outerDelta"], serde_json::json!([0, 0]));
        assert_eq!(result["innerDelta"], serde_json::json!([-108, -108]));
        assert_eq!(result["targetDelta"], serde_json::json!([-218, -208]));
        assert_eq!(result["repeated"], serde_json::json!([true, true]));

        let scrolled = rt
            .screenshot_prepared((360.0, 240.0), Some("about:blank"))
            .expect("scrolled screenshot");
        let scrolled_repeat = rt
            .screenshot_prepared((360.0, 240.0), Some("about:blank"))
            .expect("repeat screenshot");
        assert_ne!(top, scrolled, "nested scroll must move painted pixels");
        assert_eq!(
            scrolled, scrolled_repeat,
            "capture must not accumulate movement"
        );

        let retained = rt
            .evaluate(
                r#"
                const outer = document.getElementById('outer');
                const inner = document.getElementById('inner');
                outer.setAttribute('data-relayout', '1');
                return [outer.scrollLeft, outer.scrollTop, inner.scrollLeft, inner.scrollTop];
                "#,
            )
            .expect("retained offsets");
        assert_eq!(retained, serde_json::json!([108, 108, 110, 100]));

        let reclamped = rt
            .evaluate(
                r#"
                const outer = document.getElementById('outer');
                const inner = document.getElementById('inner');
                inner.setAttribute('style', 'width:150px;height:120px;overflow:hidden;position:relative;background:blue');
                document.getElementById('target').setAttribute(
                  'style',
                  'position:absolute;left:100px;top:80px;width:30px;height:20px;background:lime'
                );
                return [outer.scrollLeft, outer.scrollTop, inner.scrollLeft, inner.scrollTop];
                "#,
            )
            .expect("reclamped offsets");
        assert_eq!(reclamped, serde_json::json!([38, 28, 0, 0]));

        rt.evaluate("(function(){ document.getElementById('outer').remove(); document.documentElement.getBoundingClientRect(); return true; })()")
            .expect("remove scroller");
        assert!(
            rt.state.borrow().element_scroll_offsets.is_empty(),
            "removed scroll containers must be pruned after relayout"
        );
    }

    /// Chromium 150 oracle for CSSOM scrolling overflow. Visible and clip
    /// boxes expose descendant overflow but cannot move; an actual scrolling
    /// box includes trailing padding. A clip boundary suppresses propagation
    /// only on its clipped axis, and ordinary inline boxes expose zero metrics.
    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn element_scroll_metrics_match_chromium_overflow_oracles() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
              <style>
                .box { width:100px;height:80px;padding:10px;border:2px solid;position:absolute }
                .child { width:200px;height:150px }
              </style>
              <div id="visible" class="box" style="overflow:visible;top:0"><div class="child"></div></div>
              <div id="clip" class="box" style="overflow:clip;top:150px"><div class="child"></div></div>
              <div id="hidden" class="box" style="overflow:hidden;top:300px"><div class="child"></div></div>
              <div id="outer" style="width:100px;height:80px;overflow:visible;position:absolute;top:450px">
                <div id="axis" style="width:150px;height:120px;overflow-x:visible;overflow-y:clip">
                  <div style="width:300px;height:250px"></div>
                </div>
              </div>
              <div id="f1" style="width:10px;overflow:visible"><div style="width:100.1px;height:1px"></div></div>
              <div id="f2" style="width:10px;overflow:visible"><div style="width:100.6px;height:1px"></div></div>
              <span id="inline">long inline text</span>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(420.0, 700.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                const visible = document.getElementById('visible');
                const clip = document.getElementById('clip');
                const hidden = document.getElementById('hidden');
                const outer = document.getElementById('outer');
                const axis = document.getElementById('axis');
                const inline = document.getElementById('inline');
                visible.scrollTo(99, 99);
                clip.scrollTo(99, 99);
                hidden.scrollTo(99, 99);
                return {
                  visible: [visible.scrollWidth, visible.scrollHeight, visible.scrollLeft, visible.scrollTop],
                  clip: [clip.scrollWidth, clip.scrollHeight, clip.scrollLeft, clip.scrollTop],
                  hidden: [hidden.scrollWidth, hidden.scrollHeight, hidden.scrollLeft, hidden.scrollTop],
                  axis: [outer.scrollWidth, outer.scrollHeight, axis.scrollWidth, axis.scrollHeight],
                  fractional: [document.getElementById('f1').scrollWidth, document.getElementById('f2').scrollWidth],
                  inline: [inline.scrollWidth, inline.scrollHeight, inline.clientWidth, inline.clientHeight],
                };
                "#,
            )
            .expect("overflow oracle metrics");
        assert_eq!(result["visible"], serde_json::json!([210, 160, 0, 0]));
        assert_eq!(result["clip"], serde_json::json!([210, 160, 0, 0]));
        assert_eq!(result["hidden"], serde_json::json!([220, 170, 99, 70]));
        assert_eq!(result["axis"], serde_json::json!([300, 120, 300, 250]));
        assert_eq!(result["fractional"], serde_json::json!([100, 101]));
        assert_eq!(result["inline"], serde_json::json!([0, 0, 0, 0]));
    }

    /// Chromium quantizes effective scrolling ranges and assigned offsets to
    /// the current device-pixel grid. At the renderer's present 1x scale a
    /// 100.4px area cannot move a 100px scrollport, while 100.6px rounds to a
    /// one-pixel range and assigning `.5` moves geometry and paint by 1px.
    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn fractional_scroll_ranges_quantize_geometry_and_pixels_at_one_x() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
              <div id="low" style="width:100px;height:40px;overflow:auto;position:absolute;top:0">
                <div style="width:100.4px;height:40px;position:relative;background:white">
                  <div id="lowChild" style="position:absolute;left:40px;top:5px;width:10px;height:25px;background:red"></div>
                </div>
              </div>
              <div id="high" style="width:100px;height:40px;overflow:auto;position:absolute;top:60px">
                <div style="width:100.6px;height:40px;position:relative;background:white">
                  <div id="highChild" style="position:absolute;left:40px;top:5px;width:10px;height:25px;background:blue"></div>
                </div>
              </div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(160.0, 120.0);
        rt.run_page_init();

        let initial = rt
            .screenshot_prepared((160.0, 120.0), Some("about:blank"))
            .expect("initial fractional screenshot");
        let low = rt
            .evaluate(
                r#"
                const low = document.getElementById('low');
                const child = document.getElementById('lowChild');
                const before = child.getBoundingClientRect();
                low.scrollLeft = 999;
                const after = child.getBoundingClientRect();
                return [low.scrollWidth, low.clientWidth, low.scrollLeft, after.left - before.left];
                "#,
            )
            .expect("low fractional range");
        assert_eq!(low, serde_json::json!([100, 100, 0, 0]));
        let after_low = rt
            .screenshot_prepared((160.0, 120.0), Some("about:blank"))
            .expect("low fractional screenshot");
        assert_eq!(
            initial, after_low,
            "a rounded-zero range cannot move pixels"
        );

        let high = rt
            .evaluate(
                r#"
                const high = document.getElementById('high');
                const child = document.getElementById('highChild');
                const before = child.getBoundingClientRect();
                high.scrollLeft = .5;
                const after = child.getBoundingClientRect();
                return [high.scrollWidth, high.clientWidth, high.scrollLeft, after.left - before.left];
                "#,
            )
            .expect("high fractional range");
        assert_eq!(high, serde_json::json!([101, 100, 1, -1]));
        let after_high = rt
            .screenshot_prepared((160.0, 120.0), Some("about:blank"))
            .expect("high fractional screenshot");
        assert_ne!(after_low, after_high, "the quantized pixel must repaint");

        let root_dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                 <div id="wide" style="width:100.6px;height:20px"></div>
               </body></html>"#,
        );
        let mut root_rt = ObscuraJsRuntime::new();
        root_rt.set_dom(root_dom);
        root_rt.set_viewport(100.0, 60.0);
        root_rt.run_page_init();
        let root = root_rt
            .evaluate(
                r#"
                const wide = document.getElementById('wide');
                const before = wide.getBoundingClientRect();
                window.scrollTo(.5, 0);
                const after = wide.getBoundingClientRect();
                const high = [document.documentElement.scrollWidth, window.scrollX, after.left - before.left];
                wide.style.width = '100.4px';
                window.scrollTo(999, 0);
                return { high, low: [document.documentElement.scrollWidth, window.scrollX] };
                "#,
            )
            .expect("root fractional range");
        assert_eq!(root["high"], serde_json::json!([101, 1, -1]));
        assert_eq!(root["low"], serde_json::json!([100, 0]));
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn element_scroll_offsets_follow_chromium_box_and_dom_lifecycles() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
              <div id="first"><div id="scroller" style="width:100px;height:80px;overflow:auto">
                <div style="width:250px;height:200px"></div>
              </div></div>
              <div id="second"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(360.0, 240.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                const scroller = document.getElementById('scroller');
                const first = document.getElementById('first');
                const second = document.getElementById('second');
                scroller.scrollTo(70, 60);
                const initial = [scroller.scrollLeft, scroller.scrollTop];
                scroller.style.overflow = 'visible';
                const visible = [scroller.scrollLeft, scroller.scrollTop];
                scroller.style.overflow = 'auto';
                const restoredStyle = [scroller.scrollLeft, scroller.scrollTop];
                scroller.style.display = 'none';
                const noBox = [scroller.scrollWidth, scroller.scrollHeight, scroller.scrollLeft, scroller.scrollTop];
                scroller.scrollTo(5, 5);
                scroller.style.display = 'block';
                const restoredDisplay = [scroller.scrollLeft, scroller.scrollTop];
                second.appendChild(scroller);
                const moved = [scroller.scrollLeft, scroller.scrollTop];
                scroller.scrollTo(40, 30);
                second.removeChild(scroller);
                first.appendChild(scroller);
                const reattached = [scroller.scrollLeft, scroller.scrollTop];
                scroller.scrollTo(20, 10);
                first.textContent = 'replacement';
                document.body.appendChild(scroller);
                const textReplacement = [scroller.scrollLeft, scroller.scrollTop];
                const detached = document.createElement('div');
                detached.style.cssText = 'width:100px;height:80px;overflow:auto';
                let recomputes = 0;
                globalThis.__obscura_recompute_intersections = () => { recomputes++; };
                detached.scrollTo(30, 20);
                scroller.scrollTo(11, 12);
                return {
                  initial, visible, restoredStyle, noBox, restoredDisplay,
                  moved, reattached, textReplacement,
                  detached: [detached.scrollWidth, detached.scrollHeight, detached.scrollLeft, detached.scrollTop],
                  atomic: [scroller.scrollLeft, scroller.scrollTop, recomputes],
                };
                "#,
            )
            .expect("scroll lifecycle state");
        assert_eq!(result["initial"], serde_json::json!([70, 60]));
        assert_eq!(result["visible"], serde_json::json!([0, 0]));
        assert_eq!(result["restoredStyle"], serde_json::json!([70, 60]));
        assert_eq!(result["noBox"], serde_json::json!([0, 0, 0, 0]));
        assert_eq!(result["restoredDisplay"], serde_json::json!([70, 60]));
        assert_eq!(result["moved"], serde_json::json!([0, 0]));
        assert_eq!(result["reattached"], serde_json::json!([0, 0]));
        assert_eq!(result["textReplacement"], serde_json::json!([0, 0]));
        assert_eq!(result["detached"], serde_json::json!([0, 0, 0, 0]));
        assert_eq!(result["atomic"], serde_json::json!([11, 12, 1]));
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn fixed_panels_scroll_locally_and_transformed_descendants_remain_supported() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0;height:1800px">
              <div id="modal" style="position:fixed;left:20px;top:20px;width:140px;height:120px;background:red">
                <div id="panel" style="width:100px;height:80px;overflow:hidden;position:relative;background:blue">
                  <div id="fixedTarget" style="position:absolute;left:180px;top:160px;width:20px;height:20px;background:lime"></div>
                </div>
              </div>
              <div id="transformedScroller" style="position:absolute;top:300px;width:100px;height:80px;overflow:hidden">
                <div id="transformedTarget" style="width:240px;height:180px;transform:scale(1.1)"></div>
              </div>
              <div style="position:absolute;top:600px;transform:scale(1.2)">
                <div id="affineAncestorScroller" style="width:100px;height:80px;overflow:hidden">
                  <div style="width:240px;height:180px"></div>
                </div>
              </div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(360.0, 240.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                const modal = document.getElementById('modal');
                const panel = document.getElementById('panel');
                const fixedTarget = document.getElementById('fixedTarget');
                const transformedScroller = document.getElementById('transformedScroller');
                const transformedTarget = document.getElementById('transformedTarget');
                const affineAncestorScroller = document.getElementById('affineAncestorScroller');
                const before = {
                  modal: modal.getBoundingClientRect(),
                  fixedTarget: fixedTarget.getBoundingClientRect(),
                  transformedTarget: transformedTarget.getBoundingClientRect(),
                };
                panel.scrollTo(60, 50);
                transformedScroller.scrollTo(50, 40);
                affineAncestorScroller.scrollTo(50, 40);
                window.scrollTo(0, 500);
                const after = {
                  modal: modal.getBoundingClientRect(),
                  fixedTarget: fixedTarget.getBoundingClientRect(),
                  transformedTarget: transformedTarget.getBoundingClientRect(),
                };
                return {
                  modalDelta: [after.modal.left - before.modal.left, after.modal.top - before.modal.top],
                  fixedDelta: [after.fixedTarget.left - before.fixedTarget.left, after.fixedTarget.top - before.fixedTarget.top],
                  transformedDelta: [after.transformedTarget.left - before.transformedTarget.left, after.transformedTarget.top - before.transformedTarget.top],
                  offsets: [panel.scrollLeft, panel.scrollTop, transformedScroller.scrollLeft, transformedScroller.scrollTop, affineAncestorScroller.scrollLeft, affineAncestorScroller.scrollTop],
                };
                "#,
            )
            .expect("fixed and transformed scroll state");
        assert_eq!(result["modalDelta"], serde_json::json!([0, 0]));
        assert_eq!(result["fixedDelta"], serde_json::json!([-60, -50]));
        assert_eq!(result["transformedDelta"], serde_json::json!([-50, -540]));
        assert_eq!(result["offsets"], serde_json::json!([60, 50, 50, 40, 0, 0]));
    }

    /// CSSOM View exposes the viewport through the standards-mode root, but
    /// ordinary elements (including body) report their padding box. Modern
    /// animation libraries commonly measure a fixed 100vh sentinel through
    /// clientHeight; the old synthetic 100x20 fallback collapsed all of their
    /// viewport-relative trigger ranges.
    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn rendered_client_metrics_use_the_live_padding_box() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="tracker"
                     style="position:fixed;top:0;width:100%;height:100vh"></div>
                <div id="box"
                     style="box-sizing:content-box;width:100.4px;height:50.6px;
                            padding:5px 8.2px 6px 7.2px;
                            border-style:solid;
                            border-width:2px 4.1px 3px 3.1px"></div>
                <div style="height:900px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(320.0, 200.0);
        rt.run_page_init();

        let initial = rt
            .evaluate(
                r#"
                const tracker = document.getElementById("tracker");
                const box = document.getElementById("box");
                return {
                    root: [
                        document.documentElement.clientWidth,
                        document.documentElement.clientHeight
                    ],
                    body: [document.body.clientWidth, document.body.clientHeight],
                    tracker: [
                        tracker.clientWidth, tracker.clientHeight,
                        tracker.offsetWidth, tracker.offsetHeight
                    ],
                    box: [
                        box.clientWidth, box.clientHeight,
                        box.getBoundingClientRect().width,
                        box.getBoundingClientRect().height
                    ]
                };
                "#,
            )
            .unwrap();
        assert_eq!(initial["root"], serde_json::json!([320, 200]));
        assert_eq!(initial["body"], serde_json::json!([320, 967]));
        assert_eq!(initial["tracker"], serde_json::json!([320, 200, 320, 200]));
        assert_eq!(initial["box"][0], serde_json::json!(116));
        assert_eq!(initial["box"][1], serde_json::json!(62));
        assert!((initial["box"][2].as_f64().unwrap() - 123.0).abs() < 0.05);
        assert_eq!(initial["box"][3], serde_json::json!(67));

        // Attribute-backed inline-style changes invalidate the retained
        // render. Borders do not change the padding box; padding does.
        let mutated = rt
            .evaluate(
                r#"
                const tracker = document.getElementById("tracker");
                const box = document.getElementById("box");
                tracker.style.height = "50vh";
                box.style.borderLeftWidth = "13px";
                box.style.paddingLeft = "17px";
                return [
                    tracker.clientHeight,
                    box.clientWidth,
                    box.getBoundingClientRect().width
                ];
                "#,
            )
            .unwrap();
        assert_eq!(mutated[0], serde_json::json!(100));
        assert_eq!(mutated[1], serde_json::json!(126));
        assert_eq!(mutated[2], serde_json::json!(143));

        // A later CDP/emulation viewport update invalidates the layout too;
        // both the root special case and an ordinary 100vh box are live.
        rt.set_viewport(640.0, 360.0);
        assert_eq!(
            rt.evaluate(
                r#"const tracker = document.getElementById("tracker");
                return [
                    document.documentElement.clientWidth,
                    document.documentElement.clientHeight,
                    tracker.clientWidth,
                    tracker.clientHeight
                ]"#,
            )
            .unwrap(),
            serde_json::json!([640, 360, 640, 180])
        );
    }

    /// CSSOM View distinguishes "no associated CSS box" from a real box whose
    /// dimensions happen to be zero. Blink and Gecko return an all-zero
    /// bounding rect and no client rects for display:none/detached elements;
    /// a laid-out zero-size box still contributes one client rect.
    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn rendered_cssom_rects_distinguish_no_box_from_zero_size_box() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="hidden" style="display:none;width:80px;height:40px"></div>
                <div id="zero" style="display:block;width:0;height:0"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(320.0, 200.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                const hidden = document.getElementById("hidden");
                const detached = document.createElement("div");
                detached.style.cssText = "display:block;width:90px;height:50px";
                const zero = document.getElementById("zero");
                const sample = element => {
                    const rect = element.getBoundingClientRect();
                    const rects = element.getClientRects();
                    return {
                        rect: [
                            rect.x, rect.y, rect.width, rect.height,
                            rect.top, rect.right, rect.bottom, rect.left
                        ],
                        rectCount: rects.length,
                        firstWidth: rects.length ? rects[0].width : null,
                    };
                };
                return {
                    hidden: sample(hidden),
                    detached: sample(detached),
                    zero: sample(zero),
                };
                "#,
            )
            .unwrap();

        for name in ["hidden", "detached"] {
            assert_eq!(
                result[name]["rect"],
                serde_json::json!([0, 0, 0, 0, 0, 0, 0, 0]),
                "{name} must expose the CSSOM View no-box bounding rect"
            );
            assert_eq!(
                result[name]["rectCount"],
                serde_json::json!(0),
                "{name} must expose an empty client rect list"
            );
            assert_eq!(result[name]["firstWidth"], serde_json::Value::Null);
        }
        assert_eq!(result["zero"]["rect"][2], serde_json::json!(0));
        assert_eq!(result["zero"]["rect"][3], serde_json::json!(0));
        assert_eq!(
            result["zero"]["rectCount"],
            serde_json::json!(1),
            "a real zero-size layout box must not be mistaken for no box"
        );
        assert_eq!(result["zero"]["firstWidth"], serde_json::json!(0));
    }

    #[cfg(not(feature = "render"))]
    #[test]
    pub(crate) fn non_render_cssom_rects_keep_compatibility_geometry() {
        let mut rt = setup_runtime(r#"<html><body><div id="box"></div></body></html>"#);
        let result = rt
            .evaluate(
                r#"
                const box = document.getElementById("box");
                const detached = document.createElement("div");
                return [box, detached].map(element => {
                    const rect = element.getBoundingClientRect();
                    return [rect.width, rect.height, element.getClientRects().length];
                });
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!([[100, 20, 1], [100, 20, 1]]));
    }

    /// Chromium 150 reference (800x513 CSS-pixel viewport):
    /// top=[60,20,20,-267], bottom=[448,448,231,31,-496] at the sampled
    /// root scroll offsets. This keeps sticky distinct from fixed positioning,
    /// verifies subtree movement, bottom-only sticking, and the containing
    /// block's lower boundary without depending on a live site.
    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn root_scroll_sticky_geometry_matches_chromium_constraints() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div style="height:40px"></div>
                <div id="cb" style="box-sizing:border-box;height:900px;padding:10px 12px;border:4px solid #333">
                    <div id="top" style="box-sizing:border-box;position:sticky;top:20px;height:60px;margin:6px">
                        <div id="top-child" style="height:12px"></div>
                    </div>
                    <div style="height:500px"></div>
                    <div id="bottom" style="box-sizing:border-box;position:sticky;bottom:15px;height:50px;margin:5px"></div>
                </div>
                <div style="height:700px"></div>
                <div id="fixed" style="position:fixed;left:600px;top:20px;width:60px;height:60px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(800.0, 513.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                const top = document.getElementById("top");
                const child = document.getElementById("top-child");
                const bottom = document.getElementById("bottom");
                const fixed = document.getElementById("fixed");
                const sample = y => {
                    window.scrollTo(0, y);
                    return [
                        window.scrollY,
                        top.getBoundingClientRect().top,
                        child.getBoundingClientRect().top,
                        bottom.getBoundingClientRect().top,
                        fixed.getBoundingClientRect().top,
                    ];
                };
                return [sample(0), sample(100), sample(400), sample(600), sample(9999)];
                "#,
            )
            .unwrap();
        let rows = result.as_array().expect("rows");
        let number =
            |row: usize, column: usize| rows[row].as_array().unwrap()[column].as_f64().unwrap();
        let close = |actual: f64, expected: f64| {
            assert!(
                (actual - expected).abs() < 0.05,
                "expected {expected}, got {actual}"
            );
        };

        close(number(0, 1), 60.0);
        close(number(0, 3), 448.0);
        close(number(1, 1), 20.0);
        close(number(1, 2), 20.0);
        close(number(1, 3), 448.0);
        close(number(2, 1), 20.0);
        close(number(2, 3), 231.0);
        close(number(3, 1), 20.0);
        close(number(3, 3), 31.0);
        close(number(4, 0), 1127.0);
        close(number(4, 1), -267.0);
        close(number(4, 2), -267.0);
        close(number(4, 3), -496.0);
        for row in 0..rows.len() {
            close(number(row, 4), 20.0);
        }
    }

    /// Chromium 150 horizontal reference for the same constraint algorithm:
    /// the sticky subtree pins at x=20, remains distinct from fixed, then
    /// leaves with its 500px containing block at the right boundary.
    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn root_scroll_sticky_supports_the_inline_axis() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div style="box-sizing:border-box;margin-left:40px;width:500px;height:100px;padding:10px;border:4px solid">
                    <div id="sticky" style="box-sizing:border-box;position:sticky;left:20px;width:60px;height:30px;margin:6px">
                        <div id="child" style="width:10px;height:10px"></div>
                    </div>
                </div>
                <div style="width:1600px;height:600px"></div>
                <div id="fixed" style="position:fixed;left:20px;top:100px;width:60px;height:30px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(800.0, 513.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                const sticky = document.getElementById("sticky");
                const child = document.getElementById("child");
                const fixed = document.getElementById("fixed");
                const sample = x => {
                    window.scrollTo(x, 0);
                    return [
                        window.scrollX,
                        sticky.getBoundingClientRect().left,
                        child.getBoundingClientRect().left,
                        fixed.getBoundingClientRect().left,
                    ];
                };
                return [sample(0), sample(100), sample(400), sample(800)];
                "#,
            )
            .unwrap();
        let rows = result.as_array().unwrap();
        let expected = [
            [0.0, 60.0, 60.0, 20.0],
            [100.0, 20.0, 20.0, 20.0],
            [400.0, 20.0, 20.0, 20.0],
            [800.0, -340.0, -340.0, 20.0],
        ];
        for (row, expected) in rows.iter().zip(expected) {
            for (actual, expected) in row.as_array().unwrap().iter().zip(expected) {
                let actual = actual.as_f64().unwrap();
                assert!(
                    (actual - expected).abs() < 0.05,
                    "expected {expected}, got {actual}"
                );
            }
        }
    }
