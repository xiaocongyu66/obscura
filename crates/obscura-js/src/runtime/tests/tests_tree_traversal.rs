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
    pub(crate) fn test_document_url() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let url = rt.evaluate("document.URL").unwrap();
        assert_eq!(url, serde_json::json!("http://example.com/test"));
    }

    #[test]
    pub(crate) fn test_query_selector() {
        let mut rt = setup_runtime("<html><body><h1>Hello</h1><p>World</p></body></html>");
        let text = rt
            .evaluate("document.querySelector('h1').textContent")
            .unwrap();
        assert_eq!(text, serde_json::json!("Hello"));
    }

    #[test]
    pub(crate) fn test_query_selector_all() {
        let mut rt = setup_runtime("<ul><li>A</li><li>B</li><li>C</li></ul>");
        let count = rt
            .evaluate("document.querySelectorAll('li').length")
            .unwrap();
        assert_eq!(count.as_f64().unwrap() as i64, 3);
    }

    #[test]
    pub(crate) fn css_supports_matches_capabilities_and_boolean_conditions() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"JSON.stringify([
                    CSS.supports("-webkit-hyphens", "none"),
                    CSS.supports("margin-trim", "inline"),
                    CSS.supports("-moz-orient", "inline"),
                    CSS.supports("color", "rgb(from red r g b)"),
                    CSS.supports("(((-webkit-hyphens:none)) and (not (margin-trim:inline))) or ((-moz-orient:inline) and (not (color:rgb(from red r g b))))"),
                    CSS.supports("display", "grid"),
                    CSS.supports("(display:grid) and (selector(.card > *))"),
                    CSS.supports("not (unknown-engine-prop:value)"),
                    CSS.supports("selector(.card >)"),
                    CSS.supports("selector(:obscura-unknown)"),
                    CSS.supports("selector(.card,)"),
                    CSS.supports("scrollbar-gutter", "stable"),
                    CSS.supports("scrollbar-gutter", "floating"),
                    CSS.supports("color", "light-dark(rgb(1, 2, 3), color-mix(in srgb, white 50%, black))"),
                    CSS.supports("(color:light-dark(red, light-dark(white, black)))"),
                    CSS.supports("color", "light-dark(red)"),
                    CSS.supports("color", "light-dark(red, rgb(1, 2, 3)"),
                    CSS.supports("border", "2px dashed red"),
                    CSS.supports("border-width", "10%"),
                    CSS.supports("word-break", "break-all"),
                    CSS.supports("filter", "blur(2px)"),
                    CSS.supports("content", "attr(data-label)"),
                    CSS.supports("display", "grid;"),
                    CSS.supports("flex-flow", "column"),
                    CSS.supports("flex-flow", "wrap column"),
                    CSS.supports("flex-flow", "column wrap"),
                    CSS.supports("flex-flow", "row column"),
                    CSS.supports("flex-flow", "nowrap wrap-reverse"),
                    CSS.supports("(flex-flow:column)")
                ])"#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!("[false,false,false,false,false,true,true,true,false,false,false,true,false,true,true,false,false,true,false,true,false,true,false,true,true,true,false,false,true]")
        );
    }

    #[test]
    pub(crate) fn test_get_element_by_id() {
        let mut rt = setup_runtime(r#"<div id="test">Content</div>"#);
        let tag = rt
            .evaluate("document.getElementById('test').tagName")
            .unwrap();
        assert_eq!(tag, serde_json::json!("DIV"));
    }

    #[test]
    pub(crate) fn attributes_named_node_map_is_live() {
        let mut rt = setup_runtime(r#"<div id="test" class="card" data-state="ready"></div>"#);
        let result = rt
            .evaluate(
                r#"
                const element = document.getElementById("test");
                const attributes = element.attributes;
                const sameObject = attributes === element.attributes;
                const firstName = attributes[0].name;
                let removed = 0;
                while (attributes.length) {
                    element.removeAttributeNode(attributes[0]);
                    removed++;
                    if (removed > 10) throw new Error("NamedNodeMap is not live");
                }
                return {
                    sameObject,
                    namedNodeMap: attributes instanceof NamedNodeMap,
                    firstName,
                    removed,
                    length: attributes.length,
                    hasAttributes: element.hasAttributes(),
                };
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!({
                "sameObject": true,
                "namedNodeMap": true,
                "firstName": "id",
                "removed": 3,
                "length": 0,
                "hasAttributes": false,
            })
        );
    }

    #[test]
    pub(crate) fn script_created_attribute_reads_stay_coherent_across_mutation_apis() {
        let mut rt = setup_runtime(r#"<html><body></body></html>"#);
        let result = rt
            .evaluate(
                r#"
                (() => {
                    const element = document.createElement("DIV");
                    const initial = element.getAttribute("data-state");
                    element.setAttribute("DATA-STATE", "ready");
                    const ordinary = [
                        element.getAttribute("data-state"),
                        element.getAttribute("DATA-STATE"),
                    ];
                    element.setAttributeNS(null, "data-state", "namespaced");
                    const namespaced = element.getAttribute("data-state");
                    element.removeAttributeNS(null, "data-state");
                    const removed = element.getAttribute("data-state");
                    return { initial, ordinary, namespaced, removed };
                })()
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!({
                "initial": null,
                "ordinary": ["ready", "ready"],
                "namespaced": "namespaced",
                "removed": null,
            })
        );
    }

    #[test]
    pub(crate) fn structural_cache_tracks_detach_reparent_and_rejected_mutations() {
        let mut rt = setup_runtime(r#"<html><body></body></html>"#);
        let result = rt
            .evaluate(
                r#"
                (() => {
                    const host = document.createElement("div");
                    const child = document.createElement("span");
                    const text = document.createTextNode("hello");
                    const fresh = [host.parentNode, host.isConnected, text.parentNode, text.isConnected];

                    host.appendChild(child);
                    const detachedTree = [child.parentNode === host, host.isConnected, child.isConnected];
                    document.body.appendChild(host);
                    const connectedTree = [host.isConnected, child.isConnected, child.parentNode === host];

                    const other = document.createElement("section");
                    document.body.appendChild(other);
                    const afterUnrelatedMutation = [host.parentNode === document.body, child.isConnected];

                    other.appendChild(child);
                    const reparented = [child.parentNode === other, child.isConnected, host.firstChild === null];

                    let wrongReference = "";
                    try { other.insertBefore(document.createElement("b"), host); }
                    catch (error) { wrongReference = error.name; }
                    let wrongReplacement = "";
                    try { other.replaceChild(document.createElement("i"), host); }
                    catch (error) { wrongReplacement = error.name; }
                    let cycle = "";
                    try { child.appendChild(other); }
                    catch (error) { cycle = error.name; }

                    document.body.removeChild(other);
                    const removedTree = [other.parentNode, other.isConnected, child.isConnected, child.parentNode === other];
                    document.body.appendChild(other);
                    const reattachedTree = [other.isConnected, child.isConnected];
                    return {
                        fresh,
                        detachedTree,
                        connectedTree,
                        afterUnrelatedMutation,
                        reparented,
                        wrongReference,
                        wrongReplacement,
                        cycle,
                        removedTree,
                        reattachedTree,
                    };
                })()
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!({
                "fresh": [null, false, null, false],
                "detachedTree": [true, false, false],
                "connectedTree": [true, true, true],
                "afterUnrelatedMutation": [true, true],
                "reparented": [true, true, true],
                "wrongReference": "NotFoundError",
                "wrongReplacement": "NotFoundError",
                "cycle": "HierarchyRequestError",
                "removedTree": [null, false, false, true],
                "reattachedTree": [true, true],
            })
        );
    }

    #[test]
    pub(crate) fn element_scroll_methods_update_scroll_offsets() {
        let mut rt = setup_runtime(
            r#"<div id="scroller" style="width:100px;height:100px;overflow:auto">
                   <div style="width:300px;height:300px"></div>
               </div>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const element = document.getElementById("scroller");
                element.scrollTo({left: 12, top: 20, behavior: "smooth"});
                element.scrollBy(3, -5);
                element.scroll({left: 7});
                return {
                    left: element.scrollLeft,
                    top: element.scrollTop,
                    methods: [
                        typeof element.scroll,
                        typeof element.scrollTo,
                        typeof element.scrollBy,
                    ],
                };
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!({
                "left": 7,
                "top": 15,
                "methods": ["function", "function", "function"],
            })
        );
    }

    #[test]
    pub(crate) fn document_fragment_get_element_by_id_searches_descendants() {
        let mut rt = setup_runtime(r#"<div id="target">document</div>"#);
        let result = rt
            .evaluate(
                r#"
                (() => {
                    const frag = document.createDocumentFragment();
                    const section = document.createElement('section');
                    section.innerHTML = '<div><span id="target">fragment</span></div><p id="a.b">literal</p>';
                    frag.appendChild(section);

                    const dup = document.createDocumentFragment();
                    const deepParent = document.createElement('div');
                    deepParent.innerHTML = '<span id="dup">deep</span>';
                    const shallow = document.createElement('p');
                    shallow.id = 'dup';
                    shallow.textContent = 'shallow';
                    dup.appendChild(deepParent);
                    dup.appendChild(shallow);

                    return [
                        frag.getElementById('target').textContent,
                        frag.getElementById('missing') === null,
                        frag.getElementById('a.b').textContent,
                        frag.getElementById(123) === null,
                        dup.getElementById('dup').textContent,
                    ];
                })()
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!(["fragment", true, "literal", true, "deep"])
        );
    }

    /// Issue #461: FILTER_REJECT must prune the rejected node's whole subtree,
    /// while FILTER_SKIP only skips the node and leaves descendants eligible.
    /// Collapsing both into "not accepted" let a TreeWalker yield nodes from
    /// inside a subtree the page explicitly rejected.
    #[test]
    pub(crate) fn tree_walker_filter_reject_prunes_the_whole_subtree() {
        let mut rt = setup_runtime(r#"<div id="root"><section><p>deep</p></section><a></a></div>"#);
        rt.run_page_init();
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                function walk(verdict) {
                    const w = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT, {
                        acceptNode(node) {
                            return node.tagName === 'SECTION' ? verdict : NodeFilter.FILTER_ACCEPT;
                        }
                    });
                    const seen = [];
                    let node;
                    while ((node = w.nextNode())) seen.push(node.tagName);
                    return seen;
                }
                return [walk(NodeFilter.FILTER_REJECT), walk(NodeFilter.FILTER_SKIP)];
                "#,
            )
            .unwrap();
        // REJECT drops <p> with its <section> parent; SKIP drops only <section>.
        assert_eq!(result, serde_json::json!([["A"], ["P", "A"]]));
    }

    /// Issue #462: previousNode() must walk reverse document order until a node
    /// is accepted, not give up as soon as the first candidate is filtered out.
    #[test]
    pub(crate) fn previous_node_walks_reverse_document_order() {
        let mut rt = setup_runtime(r#"<div id="root"><a><b></b></a><c></c></div>"#);
        rt.run_page_init();
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                const w = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT, {
                    acceptNode(node) {
                        return node.tagName === 'B'
                            ? NodeFilter.FILTER_SKIP
                            : NodeFilter.FILTER_ACCEPT;
                    }
                });
                const forward = [];
                let node;
                while ((node = w.nextNode())) forward.push(node.tagName);
                const backward = [];
                while ((node = w.previousNode())) backward.push(node.tagName);
                return [forward, backward];
                "#,
            )
            .unwrap();
        // From <c>, the previous sibling's deepest last child <b> is skipped, so
        // the walk must keep going up to <a> instead of returning null.
        assert_eq!(result, serde_json::json!([["A", "C"], ["A"]]));
    }

    /// Issue #462: a backward walk must retrace a forward walk exactly, and stop
    /// at the root without ever returning it.
    #[test]
    pub(crate) fn previous_node_retraces_a_full_forward_walk() {
        let mut rt = setup_runtime(
            r#"<div id="root"><section><p>one</p><span></span></section><a><b></b></a></div>"#,
        );
        rt.run_page_init();
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                const w = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT);
                const forward = [];
                let node;
                while ((node = w.nextNode())) forward.push(node.tagName);
                const backward = [];
                while ((node = w.previousNode())) backward.push(node.tagName);
                backward.reverse();
                // previousNode never yields root, and never yields the node the
                // forward walk ended on, so compare against forward minus its last.
                // A failed traversal leaves currentNode untouched (DOM 6.1), so
                // it stays on the last node previousNode did return.
                return [forward, backward, w.currentNode.tagName];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                ["SECTION", "P", "SPAN", "A", "B"],
                ["SECTION", "P", "SPAN", "A"],
                "SECTION"
            ])
        );
    }

    /// Issue #462: FILTER_REJECT prunes a subtree in the backward direction too
    /// — the descent into a rejected node's last children must stop.
    #[test]
    pub(crate) fn previous_node_honours_filter_reject_subtree_pruning() {
        let mut rt =
            setup_runtime(r#"<div id="root"><a></a><section><p>deep</p></section><c></c></div>"#);
        rt.run_page_init();
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                const w = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT, {
                    acceptNode(node) {
                        return node.tagName === 'SECTION'
                            ? NodeFilter.FILTER_REJECT
                            : NodeFilter.FILTER_ACCEPT;
                    }
                });
                while (w.nextNode()) { /* advance to the last accepted node */ }
                const backward = [];
                let node;
                while ((node = w.previousNode())) backward.push(node.tagName);
                return backward;
                "#,
            )
            .unwrap();
        // <p> lives inside the rejected <section>, so the backward walk from <c>
        // must jump straight to <a>.
        assert_eq!(result, serde_json::json!(["A"]));
    }

    /// Issue #461: NodeIterator has no subtree pruning — DOM 6.2 says
    /// FILTER_REJECT behaves as FILTER_SKIP there. The shared walker must not
    /// Issue #475: parentNode() must never surface a node above `root`. With
    /// currentNode at root, the old guard stepped to root's own parent and
    /// returned it — escaping the walker's subtree entirely.
    #[test]
    pub(crate) fn tree_walker_parent_node_does_not_escape_above_root() {
        let mut rt = setup_runtime(r#"<div id="root"><a></a></div>"#);
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                const w = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT);
                const escaped = w.parentNode();
                return [escaped, w.currentNode.id];
                "#,
            )
            .unwrap();
        // No parent within the subtree, and currentNode stays put at root.
        assert_eq!(result, serde_json::json!([null, "root"]));
    }

    /// Issue #475: when the accepted ancestor is `root` itself, parentNode()
    /// returns it and moves currentNode there — the old `parent !== root` guard
    /// wrongly excluded it.
    #[test]
    pub(crate) fn tree_walker_parent_node_can_return_the_root() {
        let mut rt = setup_runtime(r#"<div id="root"><a></a></div>"#);
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                const w = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT);
                w.currentNode = root.querySelector('a');
                const p = w.parentNode();
                return [p ? p.id : null, w.currentNode === root];
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!(["root", true]));
    }

    /// Issue #475: parentNode() climbs past a skipped ancestor to the first
    /// accepted one, instead of stopping at the immediate parent.
    #[test]
    pub(crate) fn tree_walker_parent_node_climbs_past_skipped_ancestors() {
        let mut rt =
            setup_runtime(r#"<div id="root"><main id="m"><section><a></a></section></main></div>"#);
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                const w = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT, {
                    acceptNode(n) {
                        return n.tagName === 'SECTION'
                            ? NodeFilter.FILTER_SKIP
                            : NodeFilter.FILTER_ACCEPT;
                    }
                });
                w.currentNode = root.querySelector('a');
                const p = w.parentNode();
                return p ? p.id : null;
                "#,
            )
            .unwrap();
        // <a>'s parent <section> is skipped, so <main> is the first accepted
        // ancestor — not null, and not the immediate <section>.
        assert_eq!(result, serde_json::json!("m"));
    }

    /// leak TreeWalker's pruning into it.
    #[test]
    pub(crate) fn node_iterator_treats_filter_reject_as_skip() {
        let mut rt = setup_runtime(r#"<div id="root"><section><p>deep</p></section><a></a></div>"#);
        rt.run_page_init();
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                const it = document.createNodeIterator(root, NodeFilter.SHOW_ELEMENT, {
                    acceptNode(node) {
                        return node.tagName === 'SECTION'
                            ? NodeFilter.FILTER_REJECT
                            : NodeFilter.FILTER_ACCEPT;
                    }
                });
                const seen = [];
                let node;
                while ((node = it.nextNode())) seen.push(node.tagName);
                return seen;
                "#,
            )
            .unwrap();
        // The rejected <section> is skipped but not pruned, so <p> still shows.
        // The leading root is #467: an iterator yields the node it is rooted at.
        assert_eq!(result, serde_json::json!(["DIV", "P", "A"]));
    }

    /// Issue #467: a NodeIterator starts *before* its root, so the first
    /// nextNode() returns the root itself. Aliasing createTreeWalker silently
    /// dropped exactly the element the iterator was rooted at.
    #[test]
    pub(crate) fn node_iterator_yields_the_root_node_first() {
        let mut rt = setup_runtime(r#"<div id="root"><a></a></div>"#);
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                const it = document.createNodeIterator(root, NodeFilter.SHOW_ELEMENT);
                const seen = [];
                let node;
                while ((node = it.nextNode())) seen.push(node.tagName);
                return seen;
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!(["DIV", "A"]));
    }

    /// Issue #467: the NodeIterator interface surface, and that TreeWalker-only
    /// members are not exposed on it.
    #[test]
    pub(crate) fn node_iterator_exposes_its_own_interface() {
        let mut rt = setup_runtime(r#"<div id="root"><a></a></div>"#);
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                const it = document.createNodeIterator(root, NodeFilter.SHOW_ELEMENT);
                const before = [it.referenceNode === root, it.pointerBeforeReferenceNode];
                it.nextNode();
                return [
                    before,
                    typeof it.detach,
                    it.detach() === undefined,
                    typeof it.previousNode,
                    it.root === root,
                    it.whatToShow,
                    // TreeWalker-only members must not leak onto a NodeIterator.
                    typeof it.currentNode,
                    typeof it.firstChild,
                    typeof it.parentNode,
                    // The pointer advanced past the root it just returned.
                    [it.referenceNode.tagName, it.pointerBeforeReferenceNode],
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                [true, true],
                "function",
                true,
                "function",
                true,
                1,
                "undefined",
                "undefined",
                "undefined",
                ["DIV", false]
            ])
        );
    }

    /// Issue #467: previousNode() retraces the iterator, and the root is the
    /// last node it yields going backwards.
    #[test]
    pub(crate) fn node_iterator_previous_node_retraces_the_walk() {
        let mut rt = setup_runtime(r#"<div id="root"><a><b></b></a><c></c></div>"#);
        let result = rt
            .evaluate(
                r#"
                const root = document.getElementById('root');
                const it = document.createNodeIterator(root, NodeFilter.SHOW_ELEMENT);
                const forward = [];
                let node;
                while ((node = it.nextNode())) forward.push(node.tagName);
                const backward = [];
                while ((node = it.previousNode())) backward.push(node.tagName);
                return [forward, backward];
                "#,
            )
            .unwrap();
        // Forward ends on <c>; going back re-yields <c> (the pointer sits after
        // it), then the rest in reverse, root included.
        assert_eq!(
            result,
            serde_json::json!([["DIV", "A", "B", "C"], ["C", "B", "A", "DIV"]])
        );
    }

    /// Issue #463: `<template>` contents are parsed into the node's
    /// `template_contents` document, but no op exposed it, so `.content` handed
    /// back a fabricated empty fragment and the parsed markup was unreachable.
    #[test]
    pub(crate) fn template_content_exposes_parsed_markup() {
        let mut rt = setup_runtime(
            r#"<body><template id="t"><p class="row">a</p><p class="row">b</p></template></body>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const t = document.getElementById('t');
                return [
                    t.content.childNodes.length,
                    t.content.querySelectorAll('.row').length,
                    t.content.firstElementChild.textContent,
                    t.innerHTML,
                    t.content.nodeType,
                    t.content instanceof DocumentFragment,
                    // Identity is stable: frameworks stash `.content` and reuse it.
                    t.content === t.content,
                    // The children stay off the element itself, per the HTML spec.
                    t.childNodes.length,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                2,
                2,
                "a",
                r#"<p class="row">a</p><p class="row">b</p>"#,
                11,
                true,
                true,
                0
            ])
        );
    }

    /// Setting innerHTML on the <html> element parses in the "before head"
    /// insertion mode, which synthesizes head and body. The importer must keep
    /// both; it previously returned the synthesized body and dropped the head
    /// (so a <title>/<meta> assigned this way vanished).
    #[test]
    pub(crate) fn documentelement_inner_html_keeps_head_and_body() {
        let mut rt = setup_runtime("<html><head></head><body></body></html>");
        let v = rt
            .evaluate(
                "(function(){ document.documentElement.innerHTML = '<head><title>T</title></head><body><p>hi</p></body>'; \
                 var t = document.querySelector('title'); var p = document.querySelector('p'); \
                 return (t ? t.textContent : 'no-title') + '|' + (p ? p.textContent : 'no-p'); })()",
            )
            .unwrap();
        assert_eq!(v, serde_json::json!("T|hi"));
    }

    /// Regression guard: innerHTML on an ordinary element still imports the
    /// parsed nodes directly (no head/body is synthesized for a div context),
    /// so the fix above must not change the common case.
    #[test]
    pub(crate) fn ordinary_element_inner_html_imports_content_directly() {
        let mut rt = setup_runtime("<html><body><div id=\"d\"></div></body></html>");
        let v = rt
            .evaluate(
                "(function(){ var d=document.getElementById('d'); d.innerHTML='<span>a</span><span>b</span>'; \
                 return d.children.length + '|' + d.textContent; })()",
            )
            .unwrap();
        assert_eq!(v, serde_json::json!("2|ab"));
    }

    /// Issue #463: the same must hold for a template that arrives via innerHTML
    /// rather than the initial document parse — that is how most frameworks
    /// inject templates.
    #[test]
    pub(crate) fn template_content_works_for_templates_added_via_inner_html() {
        let mut rt = setup_runtime(r#"<body><div id="host"></div></body>"#);
        let result = rt
            .evaluate(
                r#"
                const host = document.getElementById('host');
                host.innerHTML = '<template id="t2"><li class="item">x</li></template>';
                const t = document.getElementById('t2');
                const stamped = t.content.cloneNode(true);
                host.appendChild(stamped);
                return [
                    t.content.childNodes.length,
                    t.content.querySelector('.item').textContent,
                    host.querySelectorAll('li.item').length,
                ];
                "#,
            )
            .unwrap();
        // cloneNode(true) of the content is the canonical stamping idiom.
        assert_eq!(result, serde_json::json!([1, "x", 1]));
    }

    /// Issue #463: a template built with createElement has no parsed contents,
    /// so `.content` must allocate a backing fragment on demand and round-trip
    /// through innerHTML.
    #[test]
    pub(crate) fn template_content_round_trips_for_created_templates() {
        let mut rt = setup_runtime(r#"<body></body>"#);
        let result = rt
            .evaluate(
                r#"
                const t = document.createElement('template');
                t.innerHTML = '<span class="s">hi</span>';
                return [
                    t.content.childNodes.length,
                    t.content.querySelector('.s').textContent,
                    t.innerHTML,
                    t.childNodes.length,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([1, "hi", r#"<span class="s">hi</span>"#, 0])
        );
    }

    /// Issue #463: serializing a `<template>` must emit its contents, or the
    /// markup silently disappears from outerHTML/innerHTML round-trips — and
    /// `cloneNode(true)`, which round-trips through outer_html, yields an empty
    /// template.
    #[test]
    pub(crate) fn template_contents_survive_serialization_and_clone() {
        let mut rt =
            setup_runtime(r#"<body><template id="t"><li class="item">x</li></template></body>"#);
        let result = rt
            .evaluate(
                r#"
                const t = document.getElementById('t');
                const clone = t.cloneNode(true);
                return [
                    t.outerHTML,
                    document.body.innerHTML,
                    clone.content.childNodes.length,
                    clone.content.querySelector('.item').textContent,
                    // The clone's contents are its own, not shared with the original.
                    (clone.content.firstElementChild === t.content.firstElementChild),
                ];
                "#,
            )
            .unwrap();
        let expected = r#"<template id="t"><li class="item">x</li></template>"#;
        assert_eq!(
            result,
            serde_json::json!([expected, expected, 1, "x", false])
        );
    }

    /// Issue #468: window.scrollTo/scrollBy/scroll were no-op stubs, so the
    /// dominant infinite-scroll idiom never advanced the page offset.
