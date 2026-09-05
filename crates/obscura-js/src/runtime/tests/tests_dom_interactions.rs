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

    /// frameworks that probe form field collections work.
    #[test]
    pub(crate) fn html_form_element_exposes_elements_collection() {
        let mut rt = setup_runtime(
            r#"<form id="f"><input name=a><input name=b><textarea></textarea></form>"#,
        );
        let n = rt
            .evaluate("document.getElementById('f').elements.length")
            .unwrap();
        assert_eq!(n.as_f64().unwrap() as i64, 3);
        let is_form = rt
            .evaluate("document.getElementById('f') instanceof HTMLFormElement")
            .unwrap();
        assert_eq!(is_form, serde_json::json!(true));
    }

    /// Regression for #105: `Element.prepend` must actually insert at the
    /// start, not silently no-op.
    #[test]
    pub(crate) fn element_prepend_inserts_at_start() {
        let mut rt = setup_runtime(r#"<div id="c"><span>existing</span></div>"#);
        rt.evaluate(
            r#"
            const c = document.getElementById('c');
            const n = document.createElement('span');
            n.id = 'first';
            c.prepend(n);
            "#,
        )
        .unwrap();
        let first_id = rt
            .evaluate("document.getElementById('c').firstChild.id")
            .unwrap();
        assert_eq!(first_id, serde_json::json!("first"));
        let count = rt
            .evaluate("document.getElementById('c').childNodes.length")
            .unwrap();
        assert_eq!(count.as_f64().unwrap() as i64, 2);
    }

    /// Regression for #105: `isEqualNode` compares structure, not identity.
    /// Framework diff algorithms rely on this.
    #[test]
    pub(crate) fn is_equal_node_does_structural_compare() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"
                const a = document.createElement('div'); a.setAttribute('class', 'x'); a.innerHTML = '<span>hi</span>';
                const b = document.createElement('div'); b.setAttribute('class', 'x'); b.innerHTML = '<span>hi</span>';
                const c = document.createElement('div'); c.innerHTML = '<span>bye</span>';
                return [a.isEqualNode(b), a.isEqualNode(c), a.isSameNode(b)];
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!([true, false, false]));
    }

    /// Regression for the long-standing insert_before arg-order bug noted
    /// in CLAUDE.md: bootstrap.js was passing (parent, new, ref) but `_dom`
    /// forwards only two args, silently dropping `ref`. With the fix,
    /// `insertBefore` actually inserts.
    #[test]
    pub(crate) fn insert_before_inserts_node_at_correct_position() {
        let mut rt =
            setup_runtime(r#"<div id="p"><span id="b">b</span><span id="c">c</span></div>"#);
        let order = rt
            .evaluate(
                r#"
                const p = document.getElementById('p');
                const a = document.createElement('span');
                a.id = 'a';
                p.insertBefore(a, document.getElementById('b'));
                return Array.from(p.children).map(e => e.id).join(',');
                "#,
            )
            .unwrap();
        assert_eq!(order, serde_json::json!("a,b,c"));
    }

    #[test]
    pub(crate) fn test_console_log() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script("test", "console.log('Hello from V8!')")
            .unwrap();
    }

    #[test]
    pub(crate) fn test_location() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let href = rt.evaluate("location.href").unwrap();
        assert_eq!(href, serde_json::json!("http://example.com/test"));
    }

    #[test]
    pub(crate) fn test_button_click_dispatches_listener() {
        let mut rt = setup_runtime(r#"<button id="go">Go</button>"#);
        let result = rt
            .evaluate(
                r#"
            const button = document.getElementById('go');
            button.addEventListener('click', () => { button.dataset.clicked = 'yes'; });
            button.click();
            return button.dataset.clicked;
        "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!("yes"));
    }

    #[test]
    pub(crate) fn test_dispatch_mouse_event_runs_listener() {
        let mut rt = setup_runtime(r#"<button id="go">Go</button>"#);
        let result = rt
            .evaluate(
                r#"
            const button = document.getElementById('go');
            let count = 0;
            button.addEventListener('click', () => { count += 1; });
            button.dispatchEvent(new MouseEvent('click', { bubbles: true }));
            return count;
        "#,
            )
            .unwrap();
        assert_eq!(result.as_f64().unwrap() as i64, 1);
    }

    #[test]
    pub(crate) fn test_location_href_assignment_updates_navigation_state() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let href = rt
            .evaluate("const next = '/next'; location.href = next; return location.href;")
            .unwrap();
        assert_eq!(href, serde_json::json!("http://example.com/next"));
        assert_eq!(
            rt.take_pending_navigation(),
            Some((
                "http://example.com/next".to_string(),
                "GET".to_string(),
                "".to_string()
            ))
        );
    }

    #[test]
    pub(crate) fn test_location_navigation_coerces_url_objects() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let hrefs = rt
            .evaluate(
                r#"(() => {
                    location.href = new URL('/from-href', location.href);
                    const href = location.href;
                    location.assign(new URL('/from-assign', location.href));
                    const assigned = location.href;
                    location.replace(new URL('/from-replace', location.href));
                    return [href, assigned, location.href];
                })()"#,
            )
            .unwrap();
        assert_eq!(
            hrefs,
            serde_json::json!([
                "http://example.com/from-href",
                "http://example.com/from-assign",
                "http://example.com/from-replace"
            ])
        );
        assert_eq!(
            rt.take_pending_navigation(),
            Some((
                "http://example.com/from-replace".to_string(),
                "GET".to_string(),
                "".to_string()
            ))
        );
    }

    #[test]
    pub(crate) fn test_submit_button_click_handler_can_prevent_default_and_navigate() {
        let mut rt =
            setup_runtime(r#"<form><button type="submit" id="submit">Submit</button></form>"#);
        let href = rt
            .evaluate(
                r#"
            const form = document.querySelector('form');
            form.addEventListener('submit', (event) => {
                event.preventDefault();
                location.href = '/submitted';
            });
            document.getElementById('submit').click();
            return location.href;
        "#,
            )
            .unwrap();
        assert_eq!(href, serde_json::json!("http://example.com/submitted"));
        assert_eq!(
            rt.take_pending_navigation(),
            Some((
                "http://example.com/submitted".to_string(),
                "GET".to_string(),
                "".to_string()
            ))
        );
    }

    #[test]
    pub(crate) fn test_navigator() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let ua = rt.evaluate("navigator.userAgent").unwrap();
        assert!(
            ua.as_str().unwrap().contains("Chrome"),
            "UA should contain Chrome: {}",
            ua
        );
        let wd = rt.evaluate("navigator.webdriver").unwrap();
        assert_eq!(wd, serde_json::json!(false));
        let plugins = rt.evaluate("navigator.plugins.length").unwrap();
        assert!(plugins.as_f64().unwrap() > 0.0, "Should have plugins");
        let chrome = rt.evaluate("typeof window.chrome").unwrap();
        assert_eq!(chrome, serde_json::json!("object"));
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn test_call_function_on_no_args() {
        let mut rt = setup_runtime("<html><head><title>Test</title></head><body></body></html>");
        let result = rt
            .call_function_on("() => document.title", None, &[], true)
            .await
            .unwrap();
        assert_eq!(result.value.unwrap(), serde_json::json!("Test"));
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn test_call_function_on_with_args() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let args = vec![
            serde_json::json!({"value": 10}),
            serde_json::json!({"value": 20}),
        ];
        let result = rt
            .call_function_on("(a, b) => a + b", None, &args, true)
            .await
            .unwrap();
        assert_eq!(result.value.unwrap().as_f64().unwrap() as i64, 30);
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn test_call_function_on_with_string_args() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let args = vec![
            serde_json::json!({"value": "hello"}),
            serde_json::json!({"value": " world"}),
        ];
        let result = rt
            .call_function_on("(a, b) => a + b", None, &args, true)
            .await
            .unwrap();
        assert_eq!(result.value.unwrap(), serde_json::json!("hello world"));
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn test_call_function_on_with_object_args() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let args = vec![serde_json::json!({"value": {"name": "test", "count": 5}})];
        let result = rt
            .call_function_on("(obj) => obj.name + ':' + obj.count", None, &args, true)
            .await
            .unwrap();
        assert_eq!(result.value.unwrap(), serde_json::json!("test:5"));
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn test_call_function_on_return_object() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .call_function_on("() => ({a: 1, b: 2})", None, &[], true)
            .await
            .unwrap();
        assert_eq!(result.value.unwrap(), serde_json::json!({"a": 1, "b": 2}));
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn test_call_function_on_object_ref_preserves_methods() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .call_function_on(
                "() => ({ items: [1,2,3], getLen: function() { return this.items.length; } })",
                None,
                &[],
                false,
            )
            .await
            .unwrap();
        let oid = result.object_id.unwrap();

        let result2 = rt
            .call_function_on(
                "function() { return this.getLen(); }",
                Some(&oid),
                &[],
                true,
            )
            .await
            .unwrap();
        assert_eq!(result2.value.unwrap().as_f64().unwrap() as i64, 3);
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn test_evaluate_for_cdp_detects_node() {
        let mut rt = setup_runtime("<html><body><h1>Hello</h1></body></html>");
        let result = rt
            .evaluate_for_cdp("document.querySelector('h1')", false, false)
            .await
            .unwrap();
        assert_eq!(result.subtype.as_deref(), Some("node"));
        assert_eq!(result.js_type, "object");
        assert!(result.object_id.is_some());
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn test_evaluate_for_cdp_detects_document() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt.evaluate_for_cdp("document", false, false).await.unwrap();
        assert_eq!(result.subtype.as_deref(), Some("node"));
        assert_eq!(result.class_name, "HTMLDocument");
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn test_evaluate_for_cdp_awaits_resolved_promise() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate_for_cdp("Promise.resolve(42)", true, true)
            .await
            .unwrap();
        assert_eq!(result.value.unwrap().as_f64().unwrap() as i64, 42);
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn test_evaluate_for_cdp_awaits_timer_promise() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate_for_cdp(
                "new Promise(resolve => setTimeout(() => resolve('done'), 1))",
                true,
                true,
            )
            .await
            .unwrap();
        assert_eq!(result.value.unwrap().as_str().unwrap(), "done");
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn test_evaluate_for_cdp_can_await_beyond_legacy_five_second_cap() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let started = std::time::Instant::now();
        let result = rt
            .evaluate_for_cdp_with_timeout(
                "new Promise(resolve => setTimeout(() => resolve('after-five'), 5100))",
                true,
                true,
                6000,
            )
            .await
            .unwrap();
        assert_eq!(result.value.unwrap().as_str(), Some("after-five"));
        assert!(
            started.elapsed() >= std::time::Duration::from_secs(5),
            "long promise resolved before its timer deadline"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn test_call_function_on_for_cdp_reports_unsettled_promise_timeout() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let error = rt
            .call_function_on_for_cdp_with_timeout(
                "() => new Promise(() => {})",
                None,
                &[],
                true,
                true,
                25,
            )
            .await
            .unwrap_err();
        assert!(
            error.contains("did not settle within 25ms"),
            "unexpected timeout error: {error}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn test_evaluate_for_cdp_awaits_async_function() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate_for_cdp("(async () => 'async-ok')()", true, true)
            .await
            .unwrap();
        assert_eq!(result.value.unwrap().as_str().unwrap(), "async-ok");
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn test_evaluate_for_cdp_reports_promise_rejection() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let err = rt
            .evaluate_for_cdp("Promise.reject(new Error('boom'))", true, true)
            .await
            .unwrap_err();
        assert!(err.contains("boom"));
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn test_call_function_on_dom_interaction() {
        let mut rt = setup_runtime(r#"<div id="items"><span>A</span><span>B</span></div>"#);
        let args = vec![serde_json::json!({"value": "span"})];
        let result = rt
            .call_function_on(
                "(sel) => document.querySelectorAll(sel).length",
                None,
                &args,
                true,
            )
            .await
            .unwrap();
        assert_eq!(result.value.unwrap().as_f64().unwrap() as i64, 2);
    }

    #[test]
    pub(crate) fn test_inner_html_setter() {
        let mut rt = setup_runtime(r#"<div id="target"><p>Old</p></div>"#);
        rt.execute_script(
            "test",
            r#"
            var el = document.getElementById('target');
            el.innerHTML = '<strong>Bold</strong><em>Italic</em>';
        "#,
        )
        .unwrap();
        let result = rt
            .evaluate("document.getElementById('target').innerHTML")
            .unwrap();
        let html = result.as_str().unwrap();
        assert!(
            html.contains("<strong>"),
            "innerHTML should contain <strong>, got: {}",
            html
        );
        assert!(
            html.contains("<em>"),
            "innerHTML should contain <em>, got: {}",
            html
        );
        assert!(
            !html.contains("Old"),
            "innerHTML should not contain old content, got: {}",
            html
        );
    }

    #[test]
    pub(crate) fn test_inner_html_with_nested() {
        let mut rt = setup_runtime(r#"<div id="root"></div>"#);
        rt.execute_script(
            "test",
            r#"
            var el = document.getElementById('root');
            el.innerHTML = '<ul><li>A</li><li>B</li><li>C</li></ul>';
        "#,
        )
        .unwrap();
        let count = rt
            .evaluate("document.querySelectorAll('li').length")
            .unwrap();
        assert_eq!(
            count.as_f64().unwrap() as i64,
            3,
            "Should find 3 li elements after innerHTML set"
        );

        let text = rt
            .evaluate("document.querySelector('li').textContent")
            .unwrap();
        assert_eq!(text, serde_json::json!("A"));
    }

    #[test]
    pub(crate) fn test_input_value() {
        let mut rt = setup_runtime(
            r#"<form><input id="name" type="text" value="initial"><textarea id="bio">old text</textarea></form>"#,
        );
        let val = rt
            .evaluate("document.getElementById('name').value")
            .unwrap();
        assert_eq!(val, serde_json::json!("initial"));
        rt.execute_script(
            "test",
            "document.getElementById('name').value = 'new value';",
        )
        .unwrap();
        let val2 = rt
            .evaluate("document.getElementById('name').value")
            .unwrap();
        assert_eq!(val2, serde_json::json!("new value"));
        let bio = rt.evaluate("document.getElementById('bio').value").unwrap();
        assert_eq!(bio, serde_json::json!("old text"));
    }

    #[test]
    pub(crate) fn test_sequential_runtime_swap() {
        let mut rt1 = setup_runtime("<html><body><h1>Page1</h1></body></html>");
        let title1 = rt1
            .evaluate("document.querySelector('h1').textContent")
            .unwrap();
        assert_eq!(title1, serde_json::json!("Page1"));

        let dom1 = rt1.take_dom();
        drop(rt1);

        let mut rt2 = setup_runtime("<html><body><h1>Page2</h1></body></html>");
        let title2 = rt2
            .evaluate("document.querySelector('h1').textContent")
            .unwrap();
        assert_eq!(title2, serde_json::json!("Page2"));
        drop(rt2);

        if let Some(dom) = dom1 {
            let mut rt1b = ObscuraJsRuntime::new();
            rt1b.set_dom(dom);
            rt1b.set_url("http://example.com");
            rt1b.set_title("Page1");
            rt1b.run_page_init();
            let title1b = rt1b
                .evaluate("document.querySelector('h1').textContent")
                .unwrap();
            assert_eq!(title1b, serde_json::json!("Page1"));
        }
    }

    #[test]
    pub(crate) fn test_checkbox_checked() {
        let mut rt = setup_runtime(r#"<input id="cb" type="checkbox" checked>"#);
        let checked = rt
            .evaluate("document.getElementById('cb').checked")
            .unwrap();
        assert_eq!(checked, serde_json::json!(true));
        rt.execute_script("test", "document.getElementById('cb').checked = false;")
            .unwrap();
        let checked2 = rt
            .evaluate("document.getElementById('cb').checked")
            .unwrap();
        assert_eq!(checked2, serde_json::json!(false));
    }

    // Issue #324: React/Preact/Vue install a value tracker by redefining `value`
    // on the element instance so they can tell a real edit from their own
    // controlled write. __obscura_setFieldValue must write through the prototype
    // setter, leaving that per-instance tracker stale, so the following input
    // event reads as a genuine change and onChange fires. A plain assignment
    // keeps the tracker in sync and suppresses onChange.
    #[test]
    pub(crate) fn set_field_value_bypasses_instance_value_wrapper() {
        let mut rt = setup_runtime(r#"<input id="i">"#);
        let result = rt
            .evaluate(
                r#"
                (function(){
                    var el = document.getElementById('i');
                    var d = Object.getOwnPropertyDescriptor(el.constructor.prototype, 'value');
                    var set = d.set, get = d.get, tracked = '' + el.value;
                    Object.defineProperty(el, 'value', {
                        configurable: true,
                        get: function(){ return get.call(this); },
                        set: function(v){ tracked = '' + v; set.call(this, v); },
                    });
                    el.value = 'wrapped';
                    var afterDirect = { value: el.value, tracked: tracked };
                    globalThis.__obscura_setFieldValue(el, 'value', 'native');
                    var afterHelper = { value: el.value, tracked: tracked };
                    return JSON.stringify({ afterDirect: afterDirect, afterHelper: afterHelper });
                })()
                "#,
            )
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(result.as_str().unwrap()).unwrap();
        // Direct assignment keeps tracker == value (the change that suppresses onChange).
        assert_eq!(parsed["afterDirect"]["value"], "wrapped");
        assert_eq!(parsed["afterDirect"]["tracked"], "wrapped");
        // The helper updates the value but leaves the tracker stale, so onChange fires.
        assert_eq!(parsed["afterHelper"]["value"], "native");
        assert_eq!(parsed["afterHelper"]["tracked"], "wrapped");
    }

    // Issue #324: React feature-detects the modern input-event path with
    // `('oninput' in document)`. If the GlobalEventHandlers on* attributes are
    // only on window (not Document/Element), that check fails and React falls
    // back to a legacy change-detection path, so controlled-input onChange never
    // fires. These must be present on document and Element.prototype too.
    #[test]
    pub(crate) fn global_event_handlers_present_on_document_and_element() {
        let mut rt = setup_runtime("<div></div>");
        let result = rt
            .evaluate(
                r#"JSON.stringify({
                    docInput: ('oninput' in document),
                    docChange: ('onchange' in document),
                    docClick: ('onclick' in document),
                    elProtoInput: ('oninput' in Element.prototype),
                    winInput: ('oninput' in window)
                })"#,
            )
            .unwrap();
        let p: serde_json::Value = serde_json::from_str(result.as_str().unwrap()).unwrap();
        assert_eq!(p["docInput"], true);
        assert_eq!(p["docChange"], true);
        assert_eq!(p["docClick"], true);
        assert_eq!(p["elProtoInput"], true);
        assert_eq!(p["winInput"], true);
    }

    #[test]
    pub(crate) fn test_matches_and_closest() {
        let mut rt = setup_runtime(
            r#"<div class="outer"><div class="inner"><span id="target">Hi</span></div></div>"#,
        );
        let matches = rt
            .evaluate("document.getElementById('target').matches('span')")
            .unwrap();
        assert_eq!(matches, serde_json::json!(true));
        let closest = rt
            .evaluate("document.getElementById('target').closest('.outer').className")
            .unwrap();
        assert_eq!(closest, serde_json::json!("outer"));
        let no_match = rt
            .evaluate("document.getElementById('target').closest('.nonexistent')")
            .unwrap();
        assert_eq!(no_match, serde_json::Value::Null);
    }

    #[test]
    pub(crate) fn shallow_element_clone_preserves_interface_attributes_and_isolation() {
        let mut rt = setup_runtime(
            r#"<section id="src" class="source" data-token="original"><span>child</span></section>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const source = document.getElementById('src');
                const clone = source.cloneNode(false);
                clone.className = 'clone';
                source.setAttribute('data-token', 'changed');
                return [
                    clone instanceof Node,
                    clone instanceof Element,
                    clone instanceof HTMLElement,
                    typeof clone.outerHTML,
                    typeof clone.querySelectorAll,
                    clone.tagName,
                    clone.id,
                    clone.className,
                    clone.getAttribute('data-token'),
                    clone.childNodes.length,
                    clone.ownerDocument === document,
                    clone.parentNode === null,
                    clone !== source,
                    source.className,
                    source.getAttribute('data-token'),
                    source.childNodes.length,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                true, true, true, "string", "function", "SECTION", "src", "clone", "original", 0,
                true, true, true, "source", "changed", 1
            ])
        );
    }

    #[test]
    pub(crate) fn deep_document_element_clone_stays_an_independent_html_element() {
        let mut rt = setup_runtime(
            r#"<html lang="en" data-root="original"><head><title>Clone</title></head><body><main id="app" data-state="source"><p class="item">original text</p></main></body></html>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const source = document.documentElement;
                const clone = source.cloneNode(true);
                const cloneItem = clone.querySelector('.item');
                const sourceItem = source.querySelector('.item');
                cloneItem.textContent = 'clone text';
                source.querySelector('#app').setAttribute('data-state', 'changed');
                clone.setAttribute('lang', 'fr');
                return [
                    clone instanceof Element,
                    clone instanceof HTMLElement,
                    clone.tagName,
                    typeof clone.outerHTML,
                    typeof clone.querySelectorAll,
                    clone.querySelectorAll('head, body, main, p').length,
                    clone.ownerDocument === document,
                    clone.parentNode === null,
                    clone !== source,
                    clone.querySelector('body') !== document.body,
                    clone.getAttribute('data-root'),
                    clone.getAttribute('lang'),
                    source.getAttribute('lang'),
                    cloneItem.textContent,
                    sourceItem.textContent,
                    clone.querySelector('#app').getAttribute('data-state'),
                    source.querySelector('#app').getAttribute('data-state'),
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                true,
                true,
                "HTML",
                "string",
                "function",
                4,
                true,
                true,
                true,
                true,
                "original",
                "fr",
                "en",
                "clone text",
                "original text",
                "source",
                "changed"
            ])
        );
    }

    #[test]
    pub(crate) fn test_evaluate_multistatement() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt.evaluate("var x = 5; var y = 10; return x + y;").unwrap();
        assert_eq!(result.as_f64().unwrap() as i64, 15);
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn test_object_ref_as_argument() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let obj = rt
            .call_function_on("() => ({ x: 42 })", None, &[], false)
            .await
            .unwrap();
        let oid = obj.object_id.unwrap();

        let args = vec![serde_json::json!({"objectId": oid})];
        let result = rt
            .call_function_on("(obj) => obj.x * 2", None, &args, true)
            .await
            .unwrap();
        assert_eq!(result.value.unwrap().as_f64().unwrap() as i64, 84);
    }
