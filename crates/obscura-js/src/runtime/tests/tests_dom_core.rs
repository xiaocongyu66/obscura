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
    pub(crate) fn performance_now_is_monotonic_under_bursty_calls() {
        let mut rt = setup_runtime("<html><body></body></html>");
        // Hammer performance.now() so many calls land in the same millisecond and
        // the wall clock rolls over repeatedly; the value must never go backwards.
        let violations = rt
            .evaluate(
                "(function(){var prev=-Infinity, bad=0; for(var i=0;i<500000;i++){var t=performance.now(); if(t<prev) bad++; prev=t;} return bad;})()",
            )
            .unwrap();
        assert_eq!(
            violations.as_f64(),
            Some(0.0),
            "performance.now() went backwards"
        );
    }

    #[test]
    pub(crate) fn performance_now_does_not_outrun_elapsed_time() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let lead = rt
            .evaluate(
                "(function(){for(var i=0;i<500000;i++)performance.now(); return performance.now()-(Date.now()-performance.timeOrigin);})()",
            )
            .unwrap();
        assert!(
            lead.as_f64().unwrap() <= 1.0,
            "performance.now() advanced ahead of elapsed time: {lead}"
        );
    }

    #[test]
    pub(crate) fn time_origin_never_lands_in_the_future() {
        // __obscura_init deletes itself, so a realm yields one draw of the
        // origin jitter. Build a fresh runtime per draw, do not hoist this out.
        for _ in 0..40 {
            let mut rt = setup_runtime("<html><body></body></html>");
            let skew = rt
                .evaluate("performance.timeOrigin - Date.now()")
                .unwrap()
                .as_f64()
                .unwrap();
            assert!(
                skew <= 0.0,
                "performance.timeOrigin is {skew} ms ahead of Date.now()"
            );
        }
    }

    #[test]
    pub(crate) fn childnode_helpers_coerce_non_string_primitives_to_text() {
        let mut rt =
            setup_runtime(r#"<html><body><div id="p"><span id="t">x</span></div></body></html>"#);
        let before = rt
            .evaluate("(function(){var t=document.getElementById('t'); t.before(5); return t.previousSibling ? t.previousSibling.textContent : 'NULL';})()")
            .unwrap();
        assert_eq!(before, serde_json::json!("5"));
        let after = rt
            .evaluate("(function(){var t=document.getElementById('t'); t.after(true); return t.nextSibling ? t.nextSibling.textContent : 'NULL';})()")
            .unwrap();
        assert_eq!(after, serde_json::json!("true"));
        let replaced = rt
            .evaluate("(function(){var t=document.getElementById('t'); t.replaceWith(42); return document.getElementById('p').textContent;})()")
            .unwrap();
        assert!(
            replaced.as_str().unwrap().contains("42"),
            "replaceWith(42) should leave text '42': {replaced}"
        );
    }

    #[test]
    pub(crate) fn replace_state_without_url_preserves_current_location() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let path = rt
            .evaluate(
                "(function(){history.pushState({}, '', '/dashboard'); history.replaceState({scroll:1}); return location.pathname;})()",
            )
            .unwrap();
        assert_eq!(path, serde_json::json!("/dashboard"));
    }

    #[test]
    pub(crate) fn push_state_without_url_preserves_current_location() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let path = rt
            .evaluate(
                "(function(){history.pushState({}, '', '/a'); history.pushState({b:1}); return location.pathname;})()",
            )
            .unwrap();
        assert_eq!(path, serde_json::json!("/a"));
    }

    #[test]
    pub(crate) fn history_exposes_the_web_platform_constructor_and_prototype() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"(function(){
                    const original = history.replaceState;
                    History.prototype.replaceState.call(history, {ok:true}, "", "/prototype");
                    let illegal = false;
                    try { new History(); } catch (error) { illegal = error instanceof TypeError; }
                    return {
                        instance: history instanceof History,
                        prototype: Object.getPrototypeOf(history) === History.prototype,
                        method: original === History.prototype.replaceState,
                        tag: Object.prototype.toString.call(history),
                        path: location.pathname,
                        illegal,
                    };
                })()"#,
            )
            .unwrap();
        assert_eq!(result["instance"], serde_json::json!(true));
        assert_eq!(result["prototype"], serde_json::json!(true));
        assert_eq!(result["method"], serde_json::json!(true));
        assert_eq!(result["tag"], serde_json::json!("[object History]"));
        assert_eq!(result["path"], serde_json::json!("/prototype"));
        assert_eq!(result["illegal"], serde_json::json!(true));
    }

    #[test]
    pub(crate) fn style_attribute_parses_into_style_object() {
        // Inline styles present in the parsed HTML must be visible via el.style.*
        let mut rt = setup_runtime(
            r#"<html><body><div id="d" style="color: red; display: none">hi</div></body></html>"#,
        );
        assert_eq!(
            rt.evaluate("document.getElementById('d').style.color")
                .unwrap(),
            serde_json::json!("red")
        );
        assert_eq!(
            rt.evaluate("document.getElementById('d').style.display")
                .unwrap(),
            serde_json::json!("none")
        );
    }

    #[test]
    pub(crate) fn set_style_attribute_updates_style_object() {
        let mut rt = setup_runtime(r#"<html><body><div id="d">hi</div></body></html>"#);
        let margin = rt
            .evaluate(
                "(function(){var e=document.getElementById('d'); e.setAttribute('style','margin: 5px'); return e.style.margin;})()",
            )
            .unwrap();
        assert_eq!(margin, serde_json::json!("5px"));
    }

    #[test]
    pub(crate) fn null_namespace_style_attribute_stays_in_sync() {
        let mut rt = setup_runtime(r#"<html><body><div id="d">hi</div></body></html>"#);
        let result = rt
            .evaluate(
                "(function(){var e=document.getElementById('d'); e.setAttributeNS(null,'style','color: green'); var before=e.style.color; e.removeAttributeNS(null,'style'); return before+'|'+e.style.color+'|'+String(e.getAttribute('style'));})()",
            )
            .unwrap();
        assert_eq!(result, serde_json::json!("green||null"));
    }

    #[test]
    pub(crate) fn setting_style_property_updates_the_attribute_and_serialization() {
        let mut rt = setup_runtime(r#"<html><body><div id="d">hi</div></body></html>"#);
        let attr = rt
            .evaluate(
                "(function(){var e=document.getElementById('d'); e.style.color='blue'; return e.getAttribute('style');})()",
            )
            .unwrap();
        assert_eq!(attr, serde_json::json!("color: blue;"));
        let html = rt
            .evaluate("document.getElementById('d').outerHTML")
            .unwrap();
        assert!(
            html.as_str().unwrap().contains("color: blue"),
            "outerHTML should carry the style set via el.style: {html}"
        );
    }

    #[test]
    pub(crate) fn style_object_reflects_external_attribute_change() {
        // A later setAttribute('style', …) must supersede an earlier value read
        // through el.style (the declaration re-syncs from the attribute).
        let mut rt =
            setup_runtime(r#"<html><body><div id="d" style="color: red">hi</div></body></html>"#);
        let color = rt
            .evaluate(
                "(function(){var e=document.getElementById('d'); e.style.color; e.setAttribute('style','color: green'); return e.style.color;})()",
            )
            .unwrap();
        assert_eq!(color, serde_json::json!("green"));
    }

    #[test]
    pub(crate) fn clone_node_deep_preserves_context_sensitive_elements() {
        // A <tr> is not a valid child of <div>, so cloning through a throwaway
        // <div>.innerHTML dropped it and returned null. A structural clone keeps it.
        let mut rt = setup_runtime("<html><body></body></html>");
        let tag = rt
            .evaluate("(document.createElement('tr').cloneNode(true) || {}).tagName || 'NULL'")
            .unwrap();
        assert_eq!(tag, serde_json::json!("TR"));
        let td = rt
            .evaluate("(document.createElement('td').cloneNode(true) || {}).tagName || 'NULL'")
            .unwrap();
        assert_eq!(td, serde_json::json!("TD"));
    }

    #[test]
    pub(crate) fn clone_node_deep_copies_children_and_attributes() {
        let mut rt = setup_runtime(r#"<html><body><ul id="l"><li class="a">one</li><li class="b">two</li></ul></body></html>"#);
        let out = rt
            .evaluate(
                "(function(){var c=document.getElementById('l').cloneNode(true); return c.children.length + '|' + c.children[0].className + '|' + c.children[1].textContent;})()",
            )
            .unwrap();
        assert_eq!(out, serde_json::json!("2|a|two"));
    }

    #[test]
    pub(crate) fn clone_node_deep_preserves_table_rows() {
        let mut rt = setup_runtime(
            r#"<html><body><table id="t"><tbody><tr><td>1</td><td>2</td></tr></tbody></table></body></html>"#,
        );
        // Navigate the detached clone directly (querySelector does not traverse
        // detached subtrees). tbody > tr > (td, td).
        let out = rt
            .evaluate(
                "(function(){var tb=document.querySelector('#t tbody').cloneNode(true); var tr=tb.children[0]; return tr.tagName + '|' + tr.children.length + '|' + tr.children[1].textContent;})()",
            )
            .unwrap();
        assert_eq!(out, serde_json::json!("TR|2|2"));
    }

    #[test]
    pub(crate) fn clone_node_shallow_copies_attributes_without_children() {
        let mut rt = setup_runtime(r#"<html><body><div id="d" data-x="7"><span>kid</span></div></body></html>"#);
        let out = rt
            .evaluate(
                "(function(){var c=document.getElementById('d').cloneNode(false); return c.getAttribute('data-x') + '|' + c.childNodes.length;})()",
            )
            .unwrap();
        assert_eq!(out, serde_json::json!("7|0"));
    }

    #[test]
    pub(crate) fn clone_node_copies_js_assigned_inline_styles() {
        let mut rt = setup_runtime("<html><body><div id='d'></div></body></html>");
        let out = rt
            .evaluate(
                "(function(){var d=document.getElementById('d');d.style.color='red';d.style.fontSize='12px';var c=d.cloneNode(false);return c.style.color+'|'+c.style.fontSize+'|'+c.style.cssText;})()",
            )
            .unwrap();
        assert_eq!(out, serde_json::json!("red|12px|color: red; font-size: 12px;"));
    }

    #[test]
    pub(crate) fn clone_node_deep_copies_template_content() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let out = rt
            .evaluate(
                "(function(){var t=document.createElement('template');t.content.appendChild(document.createElement('option')).textContent='choice';var c=t.cloneNode(true);return c.content.childNodes.length+'|'+c.content.firstChild.tagName+'|'+c.content.firstChild.textContent;})()",
            )
            .unwrap();
        assert_eq!(out, serde_json::json!("1|OPTION|choice"));
    }

    #[test]
    pub(crate) fn insert_adjacent_html_parses_table_fragments() {
        let mut rt = setup_runtime(
            r#"<html><body><table id="t"><tbody id="tb"></tbody></table></body></html>"#,
        );
        let out = rt
            .evaluate("(function(){var tb=document.getElementById('tb'); tb.insertAdjacentHTML('beforeend','<tr><td>1</td><td>2</td></tr>'); var tr=tb.firstElementChild; return tr ? (tr.tagName+':'+tr.children.length) : 'NULL';})()")
            .unwrap();
        assert_eq!(out, serde_json::json!("TR:2"));
    }

    #[test]
    pub(crate) fn insert_adjacent_html_position_is_case_insensitive() {
        let mut rt = setup_runtime(r#"<html><body><div id="host"><span>base</span></div></body></html>"#);
        let out = rt
            .evaluate("(function(){var h=document.getElementById('host'); h.insertAdjacentHTML('BeforeEnd','<b>x</b>'); return h.lastElementChild ? h.lastElementChild.tagName : 'NULL';})()")
            .unwrap();
        assert_eq!(out, serde_json::json!("B"));
    }

    #[test]
    pub(crate) fn insert_adjacent_html_rejects_invalid_position() {
        let mut rt = setup_runtime(r#"<html><body><div id="host"></div></body></html>"#);
        let out = rt
            .evaluate("(function(){var h=document.getElementById('host'); try { h.insertAdjacentHTML('nope','<b>x</b>'); return 'no-throw'; } catch(e){ return e.name; }})()")
            .unwrap();
        assert_eq!(out, serde_json::json!("SyntaxError"));
    }

    #[test]
    pub(crate) fn insert_adjacent_html_keeps_leading_comments_in_table_contexts() {
        let mut rt = setup_runtime(
            r#"<html><body><table><tbody id="tb"><tr id="row"></tr></tbody></table></body></html>"#,
        );
        let out = rt
            .evaluate(
                "(function(){var tb=document.getElementById('tb');tb.insertAdjacentHTML('beforeend','<!--m--><tr><td>v</td></tr>');var row=document.getElementById('row');row.insertAdjacentHTML('beforeend','<!--n--><td>x</td>');return Array.from(tb.childNodes).map(function(n){return n.nodeName}).join('|')+';'+Array.from(row.childNodes).map(function(n){return n.nodeName}).join('|');})()",
            )
            .unwrap();
        assert_eq!(out, serde_json::json!("TR|#comment|TR;#comment|TD"));
    }

    #[test]
    pub(crate) fn insert_adjacent_html_uses_the_insertion_element_as_context() {
        let mut rt = setup_runtime(
            r#"<html><body><div id="d"></div><table id="table"><tbody id="tb"></tbody></table></body></html>"#,
        );
        let out = rt
            .evaluate(
                "(function(){var d=document.getElementById('d');d.insertAdjacentHTML('beforeend','<tr><td>v</td></tr>');var table=document.getElementById('table');table.insertAdjacentHTML('beforeend','<tr><td>x</td></tr>');var tb=document.getElementById('tb');tb.insertAdjacentHTML('beforeend','<tr><td>y</td></tr>tail');return d.firstChild.nodeName+':'+d.textContent+';'+table.lastElementChild.tagName+';'+Array.from(tb.childNodes).map(function(n){return n.nodeName+(n.data?':'+n.data:'')}).join('|');})()",
            )
            .unwrap();
        assert_eq!(out, serde_json::json!("#text:v;TBODY;TR|#text:tail"));
    }

    #[test]
    pub(crate) fn set_attribute_ns_is_retrievable_by_namespace_and_local_name() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let v = rt
            .evaluate("(function(){var s=document.createElementNS('http://www.w3.org/2000/svg','svg'); s.setAttributeNS('http://www.w3.org/1999/xlink','xlink:href','#g'); return s.getAttributeNS('http://www.w3.org/1999/xlink','href');})()")
            .unwrap();
        assert_eq!(v, serde_json::json!("#g"));
    }

    #[test]
    pub(crate) fn remove_attribute_ns_removes_by_namespace() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let v = rt
            .evaluate("(function(){var s=document.createElementNS('http://www.w3.org/2000/svg','svg'); s.setAttributeNS('http://www.w3.org/1999/xlink','xlink:href','#g'); s.removeAttributeNS('http://www.w3.org/1999/xlink','href'); return s.getAttributeNS('http://www.w3.org/1999/xlink','href');})()")
            .unwrap();
        assert_eq!(v, serde_json::json!(null));
    }

    #[test]
    pub(crate) fn get_attribute_ns_reads_plain_attributes_with_null_namespace() {
        // Backward-compat: getAttributeNS(null, name) still reads a plain attr.
        let mut rt = setup_runtime(r#"<html><body><div id="d" title="hi"></div></body></html>"#);
        let v = rt
            .evaluate("document.getElementById('d').getAttributeNS(null,'title')")
            .unwrap();
        assert_eq!(v, serde_json::json!("hi"));
    }

    #[test]
    pub(crate) fn namespaced_attribute_keeps_its_qualified_name() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let v = rt
            .evaluate("(function(){var s=document.createElementNS('http://www.w3.org/2000/svg','svg');s.setAttributeNS('http://www.w3.org/1999/xlink','xlink:href','#g');return s.getAttribute('xlink:href')+'|'+s.getAttributeNames()[0]+'|'+s.outerHTML;})()")
            .unwrap();
        assert_eq!(v, serde_json::json!("#g|xlink:href|<svg xlink:href=\"#g\"></svg>"));
    }

    #[test]
    pub(crate) fn parsed_xlink_attribute_is_available_through_both_apis() {
        let mut rt = setup_runtime(
            r##"<html><body><svg><use id="u" xlink:href="#icon"></use></svg></body></html>"##,
        );
        let v = rt
            .evaluate("(function(){var u=document.getElementById('u');return u.getAttribute('xlink:href')+'|'+u.getAttributeNS('http://www.w3.org/1999/xlink','href')+'|'+u.getAttributeNames().join(',');})()")
            .unwrap();
        assert_eq!(v, serde_json::json!("#icon|#icon|id,xlink:href"));
    }

    #[test]
    pub(crate) fn set_attribute_updates_a_parsed_namespaced_attribute_in_place() {
        // setAttribute matched the stored attribute by local name only, so a
        // parsed `xlink:href` (prefix=xlink, local=href) was never found by the
        // qualified name "xlink:href": the update was pushed as a *second*
        // attribute, getAttribute kept returning the stale original, and the
        // element serialized `xlink:href` twice.
        let mut rt = setup_runtime(
            r##"<html><body><svg><use id="u" xlink:href="#a"></use></svg></body></html>"##,
        );
        let v = rt
            .evaluate("(function(){var u=document.getElementById('u');u.setAttribute('xlink:href','#b');var dup=(u.outerHTML.match(/xlink:href/g)||[]).length;return u.getAttribute('xlink:href')+'|'+u.getAttributeNS('http://www.w3.org/1999/xlink','href')+'|'+u.getAttributeNames().join(',')+'|'+dup;})()")
            .unwrap();
        assert_eq!(v, serde_json::json!("#b|#b|id,xlink:href|1"));
    }

    #[test]
    pub(crate) fn set_attribute_ns_validates_namespace_constraints() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let v = rt
            .evaluate("(function(){var e=document.createElement('div'),out=[];for(const args of [[null,'x:y'],['urn:test','a:b:c'],['urn:test','xml:lang'],['urn:test','xmlns:x']]){try{e.setAttributeNS(args[0],args[1],'v');out.push('none')}catch(err){out.push(err.name)}}return out.join('|');})()")
            .unwrap();
        assert_eq!(
            v,
            serde_json::json!(
                "NamespaceError|InvalidCharacterError|NamespaceError|NamespaceError"
            )
        );
    }

    #[test]
    pub(crate) fn dom_parser_flags_malformed_xml_with_parsererror() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let has_err = rt
            .evaluate("(function(){var d=new DOMParser().parseFromString('<a><b></a>','application/xml'); return d.querySelector('parsererror') ? true : false;})()")
            .unwrap();
        assert_eq!(has_err, serde_json::json!(true));
    }

    #[test]
    pub(crate) fn dom_parser_accepts_well_formed_xml() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let ok = rt
            .evaluate("(function(){var d=new DOMParser().parseFromString('<root><child>x</child></root>','application/xml'); return d.querySelector('parsererror') ? 'ERR' : 'OK';})()")
            .unwrap();
        assert_eq!(ok, serde_json::json!("OK"));
    }

    #[test]
    pub(crate) fn dom_parser_html_never_gets_parsererror() {
        // HTML parsing is tolerant and must never synthesize a parsererror.
        let mut rt = setup_runtime("<html><body></body></html>");
        let ok = rt
            .evaluate("(function(){var d=new DOMParser().parseFromString('<div><p>hi</a>','text/html'); return d.querySelector('parsererror') ? 'ERR' : 'OK';})()")
            .unwrap();
        assert_eq!(ok, serde_json::json!("OK"));
    }

    #[test]
    pub(crate) fn custom_element_upgrade_runs_class_constructor_on_existing_element() {
        let mut rt = setup_runtime(
            r#"<html><body><svelte-like id="component"></svelte-like></body></html>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const before = document.getElementById("component");
                class SvelteLike extends HTMLElement {
                    constructor() {
                        super();
                        this.$$s = [];
                        this.attachShadow({ mode: "open" });
                    }
                    connectedCallback() {
                        for (const subscription of this.$$s) subscription();
                        this.$$s.push(() => {});
                        this.shadowRoot.textContent = "ready";
                    }
                }
                customElements.define("svelte-like", SvelteLike);
                return [
                    document.getElementById("component") === before,
                    before instanceof SvelteLike,
                    before.constructor === SvelteLike,
                    before.$$s.length,
                    before.shadowRoot && before.shadowRoot.textContent
                ];
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!([true, true, true, 1, "ready"]));
    }

    #[test]
    pub(crate) fn shadow_root_children_expose_parent_siblings_and_composed_root() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"
                const host = document.createElement("lit-host");
                document.body.appendChild(host);
                const root = host.attachShadow({ mode: "open" });
                const start = document.createComment("start");
                const end = document.createComment("end");
                root.appendChild(start);
                root.appendChild(end);

                const text = document.createTextNode("rendered");
                start.parentNode.insertBefore(text, end);
                const inserted = [
                    start.parentNode === root,
                    start.nextSibling === text,
                    text.previousSibling === start,
                    text.nextSibling === end,
                    end.previousSibling === text,
                    root.contains(text),
                    text.getRootNode() === root,
                    text.getRootNode({ composed: true }) === document,
                    root.getRootNode({ composed: true }) === document,
                    root.isConnected,
                    text.isConnected,
                    root.textContent
                ];

                root.removeChild(text);
                const removed = [
                    text.parentNode === null,
                    start.nextSibling === end,
                    end.previousSibling === start
                ];

                document.body.appendChild(start);
                const moved = [
                    start.parentNode === document.body,
                    start.getRootNode() === document,
                    root.firstChild === end
                ];

                root.innerHTML = "<span id='inside'>inside</span>";
                const inside = root.firstChild;
                const parsed = [
                    inside.parentNode === root,
                    inside.getRootNode() === root,
                    root.textContent,
                    inside.matches("span#inside"),
                    root.querySelector("span#inside") === inside,
                    root.querySelectorAll("span#inside").length === 1
                ];

                const a = document.createElement("a");
                const b = document.createElement("b");
                const c = document.createElement("i");
                root.replaceChildren(a, b, c);
                root.insertBefore(a, c);
                const movedWithin = Array.from(root.children, el => el.localName);

                const fragment = document.createDocumentFragment();
                const x = document.createElement("x-one");
                const y = document.createElement("x-two");
                fragment.append(x, y);
                root.insertBefore(fragment, c);
                const flattened = [
                    Array.from(root.children, el => el.localName),
                    fragment.childNodes.length,
                    x.parentNode === root,
                    y.parentNode === root
                ];

                root.replaceChild(b, c);
                const replaced = [
                    Array.from(root.children, el => el.localName),
                    c.parentNode === null,
                    b.parentNode === root
                ];

                const detached = document.createElement("detached-node");
                const errors = [];
                for (const operation of [
                    () => root.insertBefore(detached, c),
                    () => root.removeChild(c),
                    () => root.replaceChild(detached, c),
                    () => root.appendChild(root),
                    () => root.appendChild(host)
                ]) {
                    try {
                        operation();
                        errors.push("none");
                    } catch (error) {
                        errors.push(error.name);
                    }
                }
                return [inserted, removed, moved, parsed, movedWithin, flattened, replaced, errors];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                [true, true, true, true, true, true, true, true, true, true, true, "rendered"],
                [true, true, true],
                [true, true, true],
                [true, true, "inside", true, true, true],
                ["b", "a", "i"],
                [["b", "a", "x-one", "x-two", "i"], 0, true, true],
                [["a", "x-one", "x-two", "b"], true, true],
                [
                    "NotFoundError",
                    "NotFoundError",
                    "NotFoundError",
                    "HierarchyRequestError",
                    "HierarchyRequestError"
                ]
            ])
        );
    }

    #[test]
    pub(crate) fn shadow_root_identity_and_children_are_native_tree_backed() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r##"
                const host = document.createElement("native-shadow-host");
                document.body.appendChild(host);
                const root = host.attachShadow({ mode: "open", delegatesFocus: true });
                root.innerHTML = "<section id='inside'><span>native</span></section>";
                const inside = root.querySelector("#inside");
                const records = [];
                const observer = new MutationObserver(batch => records.push(...batch));
                observer.observe(root, { childList: true, subtree: true });
                const added = document.createElement("strong");
                inside.appendChild(added);
                records.push(...observer.takeRecords());

                host._shadowRoot = { mode: "closed" };
                let duplicateError = "none";
                try { host.attachShadow({ mode: "open" }); }
                catch (error) { duplicateError = error.name; }

                class ClosedShadowHost extends HTMLElement {
                    constructor() {
                        super();
                        this.closedRoot = this.attachShadow({ mode: "closed" });
                        this.internals = this.attachInternals();
                    }
                }
                customElements.define("closed-shadow-host", ClosedShadowHost);
                const closedHost = document.createElement("closed-shadow-host");

                return [
                    root instanceof ShadowRoot,
                    root.nodeType,
                    root.nodeName,
                    root.host === host,
                    root.mode,
                    root.delegatesFocus,
                    host.shadowRoot === root,
                    duplicateError,
                    inside.parentNode === root,
                    inside.getRootNode() === root,
                    inside.getRootNode({ composed: true }) === document,
                    root.isConnected,
                    inside.isConnected,
                    host.contains(inside),
                    document.querySelector("#inside") === null,
                    root.querySelector("#inside") === inside,
                    records.length,
                    records[0] && records[0].target === inside,
                    records[0] && records[0].addedNodes[0] === added,
                    closedHost.shadowRoot,
                    closedHost.internals.shadowRoot === closedHost.closedRoot
                ];
                "##,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                true,
                11,
                "#document-fragment",
                true,
                "open",
                true,
                true,
                "NotSupportedError",
                true,
                true,
                true,
                true,
                true,
                false,
                true,
                true,
                1,
                true,
                true,
                null,
                true
            ])
        );
    }

    #[test]
    pub(crate) fn create_element_synchronously_constructs_an_existing_definition() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"
                const testStart = true;
                class CreatedLater extends HTMLElement {
                    constructor() {
                        super();
                        this.constructorState = ["initialized"];
                        this.attachShadow({ mode: "open" });
                        this.shadowRoot.textContent = "constructed";
                    }
                    connectedCallback() {
                        this.constructorState.push("connected");
                    }
                }
                customElements.define("created-later", CreatedLater);
                const element = document.createElement("created-later");
                const foreign = document.createElementNS(
                    "http://www.w3.org/2000/svg", "created-later"
                );
                return [
                    element instanceof CreatedLater,
                    element.constructor === CreatedLater,
                    element.localName,
                    element.constructorState,
                    element.shadowRoot && element.shadowRoot.textContent,
                    element.isConnected,
                    foreign instanceof CreatedLater
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                true,
                true,
                "created-later",
                ["initialized"],
                "constructed",
                false,
                false
            ])
        );
    }

    #[test]
    pub(crate) fn created_foreign_element_keeps_native_qualified_name_through_clone() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let v = rt
            .evaluate(
                "(function(){const ns='http://www.w3.org/2000/svg';const el=document.createElementNS(ns,'linearGradient');const clone=el.cloneNode(true);return [el.namespaceURI,el.localName,el.tagName,el.nodeName,clone.namespaceURI,clone.localName,clone.outerHTML].join('|');})()",
            )
            .unwrap();
        assert_eq!(
            v,
            serde_json::json!(
                "http://www.w3.org/2000/svg|linearGradient|linearGradient|linearGradient|http://www.w3.org/2000/svg|linearGradient|<linearGradient></linearGradient>"
            )
        );
    }

    #[test]
    pub(crate) fn svg_path_uses_the_standard_interface_chain() {
        let mut rt = setup_runtime(
            r#"<html><body><svg><path id="shape" d="M0 0L1 1"></path></svg></body></html>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const parsed = document.getElementById("shape");
                SVGPathElement.prototype.polyfillProbe = () => "path";
                const created = document.createElementNS(
                    "http://www.w3.org/2000/svg", "path"
                );
                const div = document.createElement("div");
                return [
                    parsed.constructor.name,
                    parsed instanceof SVGPathElement,
                    parsed instanceof SVGGeometryElement,
                    parsed instanceof SVGGraphicsElement,
                    parsed instanceof SVGElement,
                    parsed instanceof Element,
                    created instanceof SVGPathElement,
                    Object.getPrototypeOf(SVGPathElement.prototype) === SVGGeometryElement.prototype,
                    Object.getPrototypeOf(SVGGeometryElement.prototype) === SVGGraphicsElement.prototype,
                    Object.getPrototypeOf(SVGGraphicsElement.prototype) === SVGElement.prototype,
                    Object.getPrototypeOf(SVGElement.prototype) === Element.prototype,
                    parsed.polyfillProbe(),
                    typeof div.polyfillProbe
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                "SVGPathElement",
                true,
                true,
                true,
                true,
                true,
                true,
                true,
                true,
                true,
                true,
                "path",
                "undefined"
            ])
        );
    }

    #[test]
    pub(crate) fn foreign_inner_html_and_contextual_fragments_keep_svg_namespace() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let v = rt
            .evaluate(
                "(function(){const ns='http://www.w3.org/2000/svg';const svg=document.createElementNS(ns,'svg');svg.innerHTML='<linearGradient id=paint></linearGradient>';const range=document.createRange();range.selectNodeContents(svg);const fragment=range.createContextualFragment('<circle></circle>');const circle=fragment.firstElementChild;return [svg.firstElementChild.namespaceURI,svg.firstElementChild.localName,circle.namespaceURI,circle.localName].join('|');})()",
            )
            .unwrap();
        assert_eq!(
            v,
            serde_json::json!(
                "http://www.w3.org/2000/svg|linearGradient|http://www.w3.org/2000/svg|circle"
            )
        );
    }

    #[test]
    pub(crate) fn throwing_custom_element_constructor_marks_upgrade_failed_without_connecting() {
        let mut rt = setup_runtime(
            r#"<html><body><throws-during-upgrade id="target"></throws-during-upgrade></body></html>"#,
        );
        let result = rt
            .evaluate(
                r#"
                let constructorCalls = 0;
                let connectedCalls = 0;
                class ThrowsDuringUpgrade extends HTMLElement {
                    constructor() {
                        super();
                        constructorCalls++;
                        throw new Error("expected constructor failure");
                    }
                    connectedCallback() {
                        connectedCalls++;
                    }
                }
                customElements.define("throws-during-upgrade", ThrowsDuringUpgrade);
                const element = document.getElementById("target");
                customElements.upgrade(document);
                return [
                    constructorCalls,
                    connectedCalls,
                    element.__customUpgradeFailed === true
                ];
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!([1, 0, true]));
    }

    #[test]
    pub(crate) fn test_document_title() {
        let mut rt = setup_runtime("<html><head><title>Test</title></head><body></body></html>");
        let title = rt.evaluate("document.title").unwrap();
        assert_eq!(title, serde_json::json!("Test"));

        let result = rt
            .evaluate(
                r#"
                (function() {
                  document.title = "A <new> title";
                  return [
                    document.title,
                    document.querySelector("head > title").textContent,
                    document.querySelectorAll("title").length
                  ];
                })()
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!(["A <new> title", "A <new> title", 1])
        );

        let normalized = rt
            .evaluate(
                r#"
                (function() {
                  document.querySelector("title").textContent = "  live\n\tDOM   title  ";
                  return document.title;
                })()
                "#,
            )
            .unwrap();
        assert_eq!(normalized, serde_json::json!("live DOM title"));
    }

    #[test]
    pub(crate) fn document_title_setter_creates_missing_title_element() {
        let mut rt = setup_runtime("<html><body><main>content</main></body></html>");
        let result = rt
            .evaluate(
                r#"
                (function() {
                  document.title = "Created";
                  return [
                    document.title,
                    document.head.tagName,
                    document.head.firstElementChild.tagName,
                    document.head.firstElementChild.textContent,
                    document.documentElement.firstElementChild === document.head
                  ];
                })()
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!(["Created", "HEAD", "TITLE", "Created", true])
        );

        let detached = rt
            .evaluate(
                r#"
                (function() {
                  const doc = document.implementation.createHTMLDocument();
                  doc.title = "  Detached   title  ";
                  return [doc.title, doc.querySelector("title").textContent, doc.referrer];
                })()
                "#,
            )
            .unwrap();
        assert_eq!(
            detached,
            serde_json::json!(["Detached title", "  Detached   title  ", ""])
        );
    }

    #[test]
    pub(crate) fn document_referrer_has_explicit_navigation_state() {
        let mut rt = setup_runtime("<html><body></body></html>");
        assert_eq!(
            rt.evaluate("document.referrer").unwrap(),
            serde_json::json!("")
        );

        rt.set_referrer("https://source.example/path?q=1");
        assert_eq!(
            rt.evaluate("document.referrer").unwrap(),
            serde_json::json!("https://source.example/path?q=1")
        );
    }

    #[test]
    pub(crate) fn global_window_has_browser_constructor_identity() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                "return [window === self, self.constructor === Window,\
                         window instanceof Window, self.document === document,\
                         self.location === location, self.history === history,\
                         self.navigator === navigator];",
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([true, true, true, true, true, true, true])
        );
    }

    #[test]
    pub(crate) fn window_named_access_exposes_ids_and_eligible_names() {
        let mut rt = setup_runtime(
            r#"<html><body>
                <script id="payload" type="application/json">{"ready":true}</script>
                <div id="duplicate"></div><span id="duplicate"></span>
                <form name="login"></form><img name="hero">
                <div name="not-exposed"></div>
            </body></html>"#,
        );
        let result = rt
            .evaluate(
                r#"
                return [
                    window.payload === document.getElementById("payload"),
                    window.payload.text,
                    window.duplicate instanceof HTMLCollection,
                    window.duplicate.length,
                    window.login === document.querySelector("form"),
                    window.hero === document.querySelector("img"),
                    typeof window["not-exposed"]
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                true,
                "{\"ready\":true}",
                true,
                2,
                true,
                true,
                "undefined"
            ])
        );
    }

    #[test]
    pub(crate) fn window_named_access_tracks_dynamic_ids_and_fragment_parsing() {
        let mut rt = setup_runtime("<html><body><div id='host'></div></body></html>");
        let result = rt
            .evaluate(
                r#"
                const made = document.createElement("section");
                made.id = "dynamicName";
                const detachedIdAbsent = !("dynamicName" in window);
                document.body.appendChild(made);
                const first = window.dynamicName === made;
                made.id = "renamedDynamic";
                const renamed = !("dynamicName" in window)
                    && window.renamedDynamic === made;
                document.body.removeChild(made);
                const removed = !("renamedDynamic" in window);
                document.body.appendChild(made);
                const reattached = window.renamedDynamic === made;
                document.getElementById("host").innerHTML =
                    "<script id='parsedName'>payload</script>";
                const parsed = window.parsedName === document.getElementById("parsedName")
                    && window.parsedName.text === "payload";
                document.getElementById("host").innerHTML = "";
                const subtree = document.createElement("div");
                subtree.innerHTML = "<svg><path id='nestedSvg' name='svgName'></path></svg>";
                const detachedNestedAbsent = !("nestedSvg" in window);
                document.body.appendChild(subtree);
                const nested = window.nestedSvg === subtree.querySelector("path")
                    && typeof window.svgName === "undefined";
                document.body.removeChild(subtree);
                const nestedRemoved = !("nestedSvg" in window);
                document.body.appendChild(subtree);
                const shadowHost = document.createElement("div");
                const shadowRoot = shadowHost.attachShadow({ mode: "open" });
                const shadowChild = document.createElement("span");
                shadowChild.id = "shadowOnly";
                shadowRoot.appendChild(shadowChild);
                document.body.appendChild(shadowHost);
                const originalFetch = window.fetch;
                const collision = document.createElement("div");
                collision.id = "fetch";
                document.body.appendChild(collision);
                document.body.removeChild(collision);
                return [
                    detachedIdAbsent,
                    first,
                    renamed,
                    removed,
                    reattached,
                    parsed,
                    !("parsedName" in window),
                    detachedNestedAbsent,
                    nested,
                    nestedRemoved,
                    window.nestedSvg === subtree.querySelector("path"),
                    !("shadowOnly" in window),
                    window.fetch === originalFetch
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                true, true, true, true, true, true, true, true, true, true, true, true,
                true
            ])
        );
    }
