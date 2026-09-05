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
    pub(crate) fn unavailable_webgl_context_does_not_claim_success() {
        let mut rt = setup_runtime("<html><body><canvas></canvas></body></html>");
        let result = rt
            .evaluate(
                r#"
                (() => {
                    const canvas = document.querySelector('canvas');
                    const fallback = document.createElement('p');
                    if (!canvas.getContext('webgl')) {
                        fallback.textContent = 'static fallback';
                        document.body.appendChild(fallback);
                    }
                    return [
                        canvas.getContext('webgl'),
                        canvas.getContext('webgl2'),
                        canvas.getContext('experimental-webgl'),
                        fallback.isConnected,
                        fallback.textContent,
                    ];
                })()
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([null, null, null, true, "static fallback"])
        );
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn canvas_2d_live_backing_paints_immediately_with_scaling_clips_and_effects() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0;width:64px;height:40px;background:#0000ff">
                <div style="position:absolute;left:4px;top:4px;width:18px;height:14px;overflow:hidden">
                  <canvas id="paint" width="2" height="1"
                    style="display:block;width:20px;height:10px;border:2px solid #ffff00;opacity:.5"></canvas>
                </div>
                <canvas id="blank" width="4" height="4"
                  style="position:absolute;left:30px;top:4px;width:10px;height:10px"></canvas>
                <canvas id="padding" width="2" height="1"
                  style="position:absolute;left:30px;top:20px;width:10px;height:4px;padding:2px 2px 2px 4px;border:1px solid #ffff00;background:#00ffff"></canvas>
                <div style="position:absolute;z-index:2;left:10px;top:7px;width:4px;height:4px;background:#ff00ff"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_url("http://example.test/canvas");
        rt.set_viewport(64.0, 40.0);
        rt.run_page_init();

        let blank_api = rt
            .evaluate(
                r#"(() => {
                    const blank = document.getElementById('blank');
                    const encoded = blank.toDataURL();
                    const blankContext = blank.getContext('2d');
                    const pixels = blankContext.getImageData(0, 0, 4, 4).data;
                    blankContext.fillStyle = '#ff0000'; blankContext.fillRect(0, 0, 1, 1);
                    blank.width = 6;
                    const resetPixel = blankContext.getImageData(0, 0, 1, 1).data;
                    const untouched = document.createElement('canvas');
                    const defaultEncoded = untouched.toDataURL();
                    const defaultPixels = untouched.getContext('2d').getImageData(0, 0, 1, 1).data;
                    return [
                      blank instanceof HTMLCanvasElement,
                      typeof document.createElement('div').getContext,
                      blank.width, blank.height, blank.getContext('2d') === blankContext,
                      encoded.startsWith('data:image/png;base64,'),
                      atob(encoded.split(',')[1]).charCodeAt(25) === 6,
                      Array.from(pixels).every((value, index) => index % 4 !== 3 || value === 0),
                      untouched.width, untouched.height,
                      defaultEncoded.startsWith('data:image/png;base64,'),
                      Array.from(defaultPixels),
                      Array.from(resetPixel),
                    ];
                })()"#,
            )
            .expect("transparent default canvas API");
        assert_eq!(
            blank_api,
            serde_json::json!([
                true,
                "undefined",
                6,
                4,
                true,
                true,
                true,
                true,
                300,
                150,
                true,
                [0, 0, 0, 0],
                [0, 0, 0, 0]
            ])
        );

        // Prepare layout before drawing so the assertions below prove canvas
        // damage retains layout, while the following capture proves pixels
        // are read from the live backing immediately after the script task.
        {
            let mut state = rt.state.borrow_mut();
            ensure_resolved_scroll(&mut state).expect("initial resolved canvas scroll");
        }
        let prepared_address = {
            let state = rt.state.borrow();
            state.prepared_render.as_ref().unwrap() as *const _ as usize
        };
        let activity_before = rt.activity_generation();
        rt.execute_script(
            "canvas-fill",
            r#"const canvas = document.getElementById('paint');
               const ctx = canvas.getContext('2d');
               ctx.fillStyle = '#ff0000'; ctx.fillRect(0, 0, 1, 1);
               ctx.fillStyle = '#00ff00'; ctx.fillRect(1, 0, 1, 1);
               const padding = document.getElementById('padding').getContext('2d');
               padding.fillStyle = '#ff0000'; padding.fillRect(0, 0, 1, 1);
               padding.fillStyle = '#00ff00'; padding.fillRect(1, 0, 1, 1);"#,
        )
        .expect("fill live canvas backing");
        assert!(rt.activity_generation() > activity_before);
        assert_eq!(
            rt.state.borrow().prepared_render.as_ref().unwrap() as *const _ as usize,
            prepared_address,
            "canvas damage must not invalidate retained layout"
        );

        let pixmap = {
            let mut state = rt.state.borrow_mut();
            ensure_resolved_scroll(&mut state).expect("resolved canvas scroll");
            let ObscuraState {
                dom,
                prepared_render,
                render_resources,
                resolved_scroll,
                canvas_surfaces,
                ..
            } = &mut *state;
            let (_, scroll) = resolved_scroll.as_ref().expect("scroll snapshot");
            let canvas_surfaces = RuntimeCanvasSurfaceSource(canvas_surfaces);
            obscura_render::paint_prepared_with_scroll_and_surface_color_and_canvas_surfaces(
                dom.as_ref().expect("canvas DOM"),
                prepared_render.as_mut().expect("prepared canvas layout"),
                render_resources,
                scroll,
                [255, 255, 255, 255],
                &canvas_surfaces,
            )
            .expect("canvas pixmap")
        };

        let red_half = pixmap.pixel(7, 8).expect("scaled red canvas pixel");
        assert!(red_half.red() > 100 && red_half.blue() > 100 && red_half.green() < 20);
        // Bilinear filtering blends at the exact source-pixel transition
        // (x=16), so sample several CSS pixels into the green half.
        let green_half = pixmap.pixel(19, 8).expect("scaled green canvas pixel");
        assert!(
            green_half.green() > 45 && green_half.blue() > 100 && green_half.red() < 20,
            "pixel at (19, 8) was rgba({}, {}, {}, {})",
            green_half.red(),
            green_half.green(),
            green_half.blue(),
            green_half.alpha()
        );
        let clipped = pixmap.pixel(23, 8).expect("outside overflow clip");
        assert_eq!((clipped.red(), clipped.green(), clipped.blue()), (0, 0, 255));
        let blank = pixmap.pixel(34, 8).expect("transparent blank canvas");
        assert_eq!((blank.red(), blank.green(), blank.blue()), (0, 0, 255));
        let overlay = pixmap.pixel(11, 8).expect("higher z-index overlay");
        assert!(overlay.red() > 240 && overlay.blue() > 240 && overlay.green() < 20);
        let border = pixmap.pixel(4, 8).expect("canvas border above content");
        assert!(border.red() > 100 && border.green() > 100 && border.blue() > 100);
        let padding = pixmap.pixel(33, 23).expect("canvas padding pixel");
        assert_eq!(
            (padding.red(), padding.green(), padding.blue()),
            (0, 255, 255),
            "canvas pixels must not cover authored padding"
        );
        let padded_content = pixmap.pixel(36, 23).expect("padded canvas content pixel");
        assert!(
            padded_content.red() > 220
                && padded_content.green() < 40
                && padded_content.blue() < 40,
            "canvas bitmap must start at the CSS content-box origin"
        );

    }

    #[test]
    pub(crate) fn test_script_execution() {
        let mut rt = setup_runtime("<ul><li>A</li><li>B</li></ul>");
        rt.execute_script(
            "test",
            r#"
            globalThis.__result = [];
            document.querySelectorAll('li').forEach(function(el) {
                globalThis.__result.push(el.textContent);
            });
        "#,
        )
        .unwrap();
        let result = rt.evaluate("globalThis.__result").unwrap();
        assert_eq!(result, serde_json::json!(["A", "B"]));
    }

    #[test]
    pub(crate) fn page_var_declarations_do_not_collide_with_dom_interfaces() {
        let mut rt = setup_runtime("<html><body></body></html>");

        rt.execute_script(
            "legacy-node-guard",
            "if (!window.Node) { var Node = {}; } globalThis.__legacyNodeRan = true;",
        )
        .unwrap();
        assert_eq!(
            rt.evaluate("globalThis.__legacyNodeRan").unwrap(),
            serde_json::json!(true)
        );

        rt.execute_script(
            "page-element",
            "var Element = function PageElement() {}; globalThis.__createdTag = document.createElement('div').tagName;",
        )
        .unwrap();
        assert_eq!(
            rt.evaluate("globalThis.__createdTag").unwrap(),
            serde_json::json!("DIV")
        );
    }

    #[test]
    pub(crate) fn dynamic_script_status_bridge_is_hidden_and_idle() {
        let mut rt = setup_runtime("<html><body></body></html>");
        assert!(!rt.has_pending_dynamic_scripts());
        assert!(!rt.has_pending_load_delaying_scripts());
        assert_eq!(rt.next_pending_timeout_delay_ms(), None);
        assert_eq!(
            rt.evaluate("typeof __dynScriptBusy").unwrap(),
            serde_json::json!("undefined")
        );
        assert_eq!(
            rt.evaluate(
                "Object.getOwnPropertyNames(globalThis).includes('__obscura_hasPendingDynamicScripts')"
            )
            .unwrap(),
            serde_json::json!(false)
        );
        assert_eq!(
            rt.evaluate(
                "Reflect.ownKeys(globalThis).includes('__obscura_hasPendingDynamicScripts')"
            )
            .unwrap(),
            serde_json::json!(false)
        );
        assert_eq!(
            rt.evaluate(
                "Reflect.ownKeys(globalThis).includes('__obscura_hasPendingLoadDelayingScripts')"
            )
            .unwrap(),
            serde_json::json!(false)
        );
        assert_eq!(
            rt.evaluate(
                "Object.getOwnPropertyNames(globalThis).includes('__obscura_nextPendingTimeoutDelay')"
            )
            .unwrap(),
            serde_json::json!(false)
        );
        assert_eq!(
            rt.evaluate(
                "Reflect.ownKeys(globalThis).includes('__obscura_nextPendingTimeoutDelay')"
            )
            .unwrap(),
            serde_json::json!(false)
        );
    }

    /// Regression test for #147: a TypeError in one script must not poison
    /// the runtime so that subsequent scripts (or DOM queries) collapse to
    /// empty. The reporter saw `--dump text` return 1 byte after offside.js
    /// crashed; that cascade should never happen.
    #[test]
    pub(crate) fn script_typeerror_does_not_poison_subsequent_execution() {
        let mut rt = setup_runtime("<html><body><p id=hit>BODY_TEXT</p></body></html>");

        // 1. First script throws the same flavor of error offside.js produced
        //    (`Cannot read properties of undefined (reading 'classList')`).
        let err = rt
            .execute_script("buggy", "var x; x.classList.add('y');")
            .unwrap_err();
        assert!(
            err.contains("classList") || err.contains("undefined"),
            "expected classList/undefined error, got: {}",
            err
        );

        // 2. The runtime must still be usable: a follow-up script runs.
        rt.execute_script("ok", "globalThis.__after_error = 'still alive';")
            .unwrap();
        let result = rt.evaluate("globalThis.__after_error").unwrap();
        assert_eq!(result, serde_json::json!("still alive"));

        // 3. DOM queries still work after the script error.
        let text = rt
            .evaluate("document.querySelector('#hit').textContent")
            .unwrap();
        assert_eq!(text, serde_json::json!("BODY_TEXT"));
    }

    /// Regression test for #355: an explicit `throw` in one inline <script> must
    /// not stop later independent <script>s from running. Each <script> executes
    /// as its own `execute_script` call, mirroring how page.rs runs them, so a
    /// thrown error is reported but the next script still runs.
    #[test]
    pub(crate) fn thrown_error_in_one_script_does_not_stop_later_scripts() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script("s1", "globalThis.__ran1 = true;")
            .unwrap();
        let err = rt
            .execute_script(
                "s2",
                "throw new Error('only one instance of babel-polyfill is allowed');",
            )
            .unwrap_err();
        assert!(
            err.contains("babel-polyfill"),
            "expected the thrown message, got: {}",
            err
        );
        rt.execute_script("s3", "globalThis.__ran3 = true;")
            .unwrap();
        let ran = rt
            .evaluate("JSON.stringify([globalThis.__ran1 === true, globalThis.__ran3 === true])")
            .unwrap();
        assert_eq!(ran, serde_json::json!("[true,true]"));
    }

    /// Regression test for #356: the `in` operator and `Object.keys` must work on
    /// `el.style` (CSSStyleDeclaration) and `el.dataset` (DOMStringMap), `_props`
    /// must not leak, and cssText must serialize dashed names with a trailing
    /// semicolon.
    #[test]
    pub(crate) fn style_and_dataset_support_in_operator_and_keys() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"(() => {
                    const el = document.createElement('div');
                    el.style.color = 'red';
                    el.style.fontSize = '14px';
                    el.dataset.foo = 'bar';
                    const keys = Object.keys(el.style);
                    return JSON.stringify({
                        colorInStyle: 'color' in el.style,
                        objectFitInStyle: 'object-fit' in el.style,
                        keysHasSet: keys.includes('color') && keys.includes('fontSize'),
                        noPropsLeak: !keys.includes('_props'),
                        fooInDataset: 'foo' in el.dataset,
                        datasetKeys: Object.keys(el.dataset),
                        cssText: el.style.cssText,
                        length: el.style.length,
                        getByDash: el.style.getPropertyValue('font-size'),
                        reflectedAttribute: el.getAttribute('style')
                    });
                })()"#,
            )
            .unwrap();
        let p: serde_json::Value = serde_json::from_str(result.as_str().unwrap()).unwrap();
        assert_eq!(p["colorInStyle"], true);
        assert_eq!(p["objectFitInStyle"], true);
        assert_eq!(p["keysHasSet"], true);
        assert_eq!(p["noPropsLeak"], true);
        assert_eq!(p["fooInDataset"], true);
        assert_eq!(p["datasetKeys"], serde_json::json!(["foo"]));
        assert_eq!(p["cssText"], "color: red; font-size: 14px;");
        assert_eq!(p["length"], 2);
        assert_eq!(p["getByDash"], "14px");
        assert_eq!(p["reflectedAttribute"], "color: red; font-size: 14px;");
    }

    #[test]
    pub(crate) fn dom_string_map_is_exposed_and_backs_dataset() {
        let mut rt = setup_runtime(r#"<div id="x" data-foo="bar"></div>"#);
        let result = rt
            .evaluate(
                r#"(() => {
                    const dataset = document.getElementById("x").dataset;
                    const interface = window.DOMStringMap;
                    const descriptor = Object.getOwnPropertyDescriptor(window, "DOMStringMap");
                    let illegalConstructor = false;
                    if (interface) {
                        try { new interface(); }
                        catch (error) { illegalConstructor = error instanceof TypeError; }
                    }
                    return JSON.stringify({
                        type: typeof interface,
                        instance: !!interface && dataset instanceof interface,
                        prototype: !!interface && Object.getPrototypeOf(dataset) === interface.prototype,
                        constructor: !!interface && dataset.constructor === interface,
                        tag: Object.prototype.toString.call(dataset),
                        enumerable: descriptor ? descriptor.enumerable : "missing",
                        illegalConstructor,
                        value: dataset.foo,
                    });
                })()"#,
            )
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(result.as_str().unwrap()).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "type": "function",
                "instance": true,
                "prototype": true,
                "constructor": true,
                "tag": "[object DOMStringMap]",
                "enumerable": false,
                "illegalConstructor": true,
                "value": "bar",
            })
        );
    }

    #[test]
    pub(crate) fn style_declaration_reflects_and_removes_parsed_attributes() {
        let mut rt = setup_runtime(
            "<html><body><div id='icon' style='font-size: 0px; color: red'></div></body></html>",
        );
        let result = rt
            .evaluate(
                r#"(() => {
                    const el = document.getElementById('icon');
                    const before = [el.style.fontSize, el.style.color, el.style.length];
                    const removed = el.style.removeProperty('font-size');
                    return JSON.stringify({
                        before,
                        removed,
                        after: el.style.cssText,
                        attribute: el.getAttribute('style')
                    });
                })()"#,
            )
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(result.as_str().unwrap()).unwrap();
        assert_eq!(value["before"], serde_json::json!(["0px", "red", 2]));
        assert_eq!(value["removed"], "0px");
        assert_eq!(value["after"], "color: red;");
        assert_eq!(value["attribute"], "color: red;");
    }

    #[test]
    pub(crate) fn select_add_and_option_text_update_the_live_dom() {
        let mut rt = setup_runtime("<html><body><select id='language'></select></body></html>");
        let result = rt
            .evaluate(
                r#"(() => {
                    const select = document.getElementById('language');
                    const english = document.createElement('option');
                    english.value = 'en';
                    english.text = 'English';
                    english.selected = true;
                    select.add(english);
                    const greek = document.createElement('option');
                    greek.value = 'el';
                    greek.text = 'Greek';
                    select.add(greek, 0);
                    return JSON.stringify({
                        labels: [...select.options].map(option => option.textContent),
                        selectedIndex: select.selectedIndex,
                        value: select.value,
                        html: select.outerHTML
                    });
                })()"#,
            )
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(result.as_str().unwrap()).unwrap();
        assert_eq!(value["labels"], serde_json::json!(["Greek", "English"]));
        assert_eq!(value["selectedIndex"], 1);
        assert_eq!(value["value"], "en");
        assert!(value["html"]
            .as_str()
            .unwrap()
            .contains(r#"<option value="en" selected="">English</option>"#));
    }

    /// Regression for #105: `element.querySelector` and `querySelectorAll`
    /// must scope to the receiver's subtree, not the whole document.
    #[test]
    pub(crate) fn element_query_selector_is_scoped_to_subtree() {
        let mut rt = setup_runtime(
            r#"<div id="a"><span class="x">in a</span></div><div id="b"><span class="x">in b</span></div>"#,
        );
        let text = rt
            .evaluate("document.getElementById('a').querySelector('.x').textContent")
            .unwrap();
        assert_eq!(text, serde_json::json!("in a"));

        let count_in_a = rt
            .evaluate("document.getElementById('a').querySelectorAll('.x').length")
            .unwrap();
        assert_eq!(count_in_a.as_f64().unwrap() as i64, 1);

        // Document-scoped query still sees both.
        let count_doc = rt
            .evaluate("document.querySelectorAll('.x').length")
            .unwrap();
        assert_eq!(count_doc.as_f64().unwrap() as i64, 2);
    }

    #[test]
    pub(crate) fn document_evaluate_exposes_basic_xpath_result() {
        let mut rt = setup_runtime("");

        let exposed = rt
            .evaluate("`${typeof XPathResult}:${typeof Document.prototype.evaluate}:${XPathResult.FIRST_ORDERED_NODE_TYPE}`")
            .unwrap();
        assert_eq!(exposed, serde_json::json!("function:function:9"));
    }

    /// Regression for #105: `document.forms` / `images` / `links` must be
    /// live, not hardcoded `[]`. jQuery 1.x's submit-event setup iterates
    /// `document.forms` and crashes when it's empty for pages that have forms.
    #[test]
    pub(crate) fn document_forms_images_links_are_live() {
        let mut rt =
            setup_runtime(r#"<form></form><form></form><img><a href="x">l</a><a>no-href</a>"#);
        assert_eq!(
            rt.evaluate("document.forms.length")
                .unwrap()
                .as_f64()
                .unwrap() as i64,
            2
        );
        assert_eq!(
            rt.evaluate("document.images.length")
                .unwrap()
                .as_f64()
                .unwrap() as i64,
            1
        );
        assert_eq!(
            rt.evaluate("document.links.length")
                .unwrap()
                .as_f64()
                .unwrap() as i64,
            1
        );
    }
