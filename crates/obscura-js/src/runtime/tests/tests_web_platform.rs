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

    /// than the undefined path.
    #[test]
    pub(crate) fn element_check_visibility_is_callable() {
        let mut rt = setup_runtime(r#"<div id="x">x</div>"#);
        let result = rt
            .evaluate("document.getElementById('x').checkVisibility({checkOpacity: true})")
            .unwrap();
        assert_eq!(result, serde_json::json!(true));

        let typeof_method = rt
            .evaluate("typeof document.getElementById('x').checkVisibility")
            .unwrap();
        assert_eq!(typeof_method, serde_json::json!("function"));
    }

    /// Playwright's `getByRole` / `getByLabel` locators resolve via ARIA
    /// reflection properties. Without the getters those locators always
    /// fail. Reflect the underlying aria-* attributes.
    #[test]
    pub(crate) fn element_aria_reflection_properties_read_aria_attrs() {
        let mut rt = setup_runtime(
            r#"<button id="b" role="tab" aria-label="Settings" aria-selected="true">x</button>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const el = document.getElementById('b');
                return [el.role, el.ariaLabel, el.ariaSelected];
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!(["tab", "Settings", "true"]));
    }

    /// Setting an ARIA reflection property must write through to the
    /// underlying attribute so frameworks that toggle state via
    /// `el.ariaExpanded = 'true'` actually update the DOM.
    /// Regression: React 18 / mobile SPAs (e.g. goofish.com) call
    /// addEventListener on navigator.connection (NetworkInformation) and
    /// navigator.serviceWorker (ServiceWorkerContainer). Both are EventTargets
    /// in real browsers; missing the method crashed the app bundle with
    /// "addEventListener is not a function".
    #[test]
    pub(crate) fn navigator_eventtarget_stubs_expose_add_event_listener() {
        let mut rt = setup_runtime("<div></div>");
        let result = rt
            .evaluate(
                r#"
                const connection = navigator.connection;
                let calls = 0;
                let receiverMatches = false;
                function listener(event) {
                    calls += 1;
                    receiverMatches = this === connection && event.type === 'change';
                }
                connection.addEventListener('change', listener);
                const dispatchResult = connection.dispatchEvent(new Event('change'));
                connection.removeEventListener('change', listener);
                connection.dispatchEvent(new Event('change'));
                return [
                    typeof connection.addEventListener,
                    typeof connection.removeEventListener,
                    typeof connection.dispatchEvent,
                    typeof navigator.serviceWorker.addEventListener,
                    dispatchResult,
                    calls,
                    receiverMatches,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!(["function", "function", "function", "function", true, 1, true])
        );
    }

    #[test]
    pub(crate) fn text_codec_streams_expose_browser_shape() {
        let mut rt = setup_runtime("<div></div>");
        let result = rt
            .evaluate(
                r#"
                const encoder = new TextEncoderStream();
                const decoder = new TextDecoderStream();
                return {
                    encoder: encoder.encoding,
                    encoderReadable: typeof encoder.readable.getReader,
                    encoderWritable: typeof encoder.writable.getWriter,
                    decoder: decoder.encoding,
                    decoderReadable: typeof decoder.readable.getReader,
                    decoderWritable: typeof decoder.writable.getWriter,
                };
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!({
                "encoder": "utf-8",
                "encoderReadable": "function",
                "encoderWritable": "function",
                "decoder": "utf-8",
                "decoderReadable": "function",
                "decoderWritable": "function",
            })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn text_encoder_stream_pipe_through_delivers_hydration_data() {
        let mut rt = setup_runtime("<div></div>");
        let result = rt
            .evaluate_for_cdp(
                r#"
                (async () => {
                    let sourceController;
                    const source = new ReadableStream({
                        start(controller) { sourceController = controller; },
                    });
                    const encoded = source.pipeThrough(new TextEncoderStream());
                    sourceController.enqueue('["server",{"hydrated":true}]\n');
                    sourceController.close();

                    const decoder = new TextDecoder();
                    let tail = "";
                    const lines = encoded.pipeThrough(new TransformStream({
                        transform(chunk, controller) {
                            const complete = (tail + decoder.decode(chunk, {stream: true})).split("\n");
                            tail = complete.pop() || "";
                            for (const line of complete) controller.enqueue(line);
                        },
                        flush(controller) { if (tail) controller.enqueue(tail); },
                    }));
                    const first = await lines.getReader().read();
                    return JSON.parse(first.value);
                })()
                "#,
                true,
                true,
            )
            .await
            .unwrap();
        assert_eq!(
            result.value.unwrap(),
            serde_json::json!(["server", {"hydrated": true}])
        );
    }

    /// Regression test for #285: DDoS-Guard's challenge calls
    /// `t.insertAdjacentText(...)` and dies with `TypeError: ... is not a
    /// function` because `Element.prototype.insertAdjacentText` was missing.
    /// Verify all four positions place a Text node (NOT parsed HTML) at the
    /// right spot. Tests `insertAdjacentText` exists, is callable, and that
    /// inserted content remains literal text — angle brackets must not be
    /// parsed as markup, which is the whole point of the API.
    #[test]
    pub(crate) fn element_insert_adjacent_text_polyfill() {
        let mut rt = setup_runtime(r#"<div id="p"><span id="t">X</span></div>"#);
        let result = rt
            .evaluate(
                r#"
                const t = document.getElementById('t');
                t.insertAdjacentText('afterbegin', 'AB');
                t.insertAdjacentText('beforeend', 'BE');
                t.insertAdjacentText('beforebegin', 'BB');
                t.insertAdjacentText('afterend', 'AE');
                t.insertAdjacentText('beforeend', '<b>raw</b>');
                return [
                    typeof Element.prototype.insertAdjacentText,
                    document.getElementById('p').textContent,
                    t.getElementsByTagName('b').length,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!(["function", "BBABXBE<b>raw</b>AE", 0])
        );
    }

    /// Regression test for #285: `Element.prototype.insertAdjacentElement`
    /// was missing alongside `insertAdjacentText`. Verify all four positions
    /// place the given element correctly and that the inserted element is
    /// returned (per spec — that's the contract callers rely on for chaining).
    #[test]
    pub(crate) fn element_insert_adjacent_element_polyfill() {
        let mut rt = setup_runtime(r#"<div id="p"><span id="t">X</span></div>"#);
        let result = rt
            .evaluate(
                r#"
                const t = document.getElementById('t');
                const before = document.createElement('b');  before.id = 'before';
                const after  = document.createElement('i');  after.id  = 'after';
                const inside = document.createElement('em'); inside.id = 'inside';
                const last   = document.createElement('u');  last.id   = 'last';
                const r1 = t.insertAdjacentElement('beforebegin', before);
                const r2 = t.insertAdjacentElement('afterend',    after);
                const r3 = t.insertAdjacentElement('afterbegin',  inside);
                const r4 = t.insertAdjacentElement('beforeend',   last);
                const siblings = Array.from(document.getElementById('p').children).map(c => c.id);
                const inT = Array.from(t.children).map(c => c.id);
                return [
                    typeof Element.prototype.insertAdjacentElement,
                    r1 === before && r2 === after && r3 === inside && r4 === last,
                    siblings,
                    inT,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                "function",
                true,
                ["before", "t", "after"],
                ["inside", "last"]
            ])
        );
    }

    #[test]
    pub(crate) fn console_log_error_does_not_trigger_prepare_stack_trace() {
        let mut rt = setup_runtime("<div></div>");
        let result = rt
            .evaluate(
                r#"
            let called = false;
            const saved = Error.prepareStackTrace;
            Error.prepareStackTrace = function() { called = true; return saved; };
            const e = new Error("test");
            console.log(e);
            Error.prepareStackTrace = saved;
            return called;
        "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!(false));
    }

    #[test]
    pub(crate) fn element_aria_reflection_setters_write_through() {
        let mut rt = setup_runtime(r#"<div id="d"></div>"#);
        let result = rt
            .evaluate(
                r#"
                const el = document.getElementById('d');
                el.role = 'menu';
                el.ariaExpanded = 'true';
                return [el.getAttribute('role'), el.getAttribute('aria-expanded')];
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!(["menu", "true"]));
    }

    /// Framework schedulers commonly subclass EventTarget for their own
    /// lifecycle events. These targets have no backing DOM node, but must
    /// still deliver callbacks (including object, once, and signal listeners).
    #[test]
    pub(crate) fn standalone_event_target_delivers_framework_lifecycle_events() {
        let mut rt = setup_runtime("<div></div>");
        let result = rt
            .evaluate(
                r#"
                const TypedEventTarget = class extends EventTarget {};
                const target = new TypedEventTarget();
                const calls = [];
                const removed = () => calls.push("removed");
                const controller = new AbortController();
                const node = { id: "canvas-ref" };

                target.addEventListener("insert", (event) => calls.push(event.node.id));
                target.addEventListener("insert", { handleEvent() { calls.push("object"); } });
                target.addEventListener("insert", () => calls.push("once"), { once: true });
                target.addEventListener("insert", removed);
                target.removeEventListener("insert", removed);
                target.addEventListener("insert", () => calls.push("aborted"), {
                    signal: controller.signal,
                });
                controller.abort();

                const first = new Event("insert", { cancelable: true });
                first.node = node;
                const firstResult = target.dispatchEvent(first);
                const second = new Event("insert");
                second.node = node;
                const secondResult = target.dispatchEvent(second);
                return [
                    target instanceof EventTarget,
                    calls,
                    firstResult,
                    secondResult,
                    first.target === target,
                    first.currentTarget === null,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                true,
                ["canvas-ref", "object", "once", "canvas-ref", "object"],
                true,
                true,
                true,
                true
            ])
        );
    }

    #[test]
    pub(crate) fn media_text_tracks_expose_loaded_webvtt_cues() {
        let mut rt = setup_runtime(
            r#"<video><track id="captions" kind="captions" srclang="en" default
                src="data:text/vtt,WEBVTT%0A%0A00%3A00%3A01.000%20--%3E%2000%3A00%3A03.000%0AHello"></video>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const element = document.getElementById("captions");
                const video = document.querySelector("video");
                const cue = element.track.cues[0];
                const added = video.addTextTrack("metadata", "Data", "en");
                cue.line = -2;
                cue.size = 80;
                return [
                    element instanceof HTMLTrackElement,
                    element.readyState === HTMLTrackElement.LOADED,
                    element.track instanceof TextTrack,
                    element.track.cues.length,
                    cue.startTime,
                    cue.endTime,
                    cue.text,
                    cue.line,
                    cue.size,
                    video.textTracks.length,
                    video.textTracks.getTrackById("captions") === element.track,
                    added.kind,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([true, true, true, 1, 1, 3, "Hello", -2, 80, 1, true, "metadata"])
        );
    }

    #[test]
    pub(crate) fn unsupported_media_capabilities_and_readiness_are_honest() {
        let mut rt = setup_runtime(
            r#"<video id="media" src="https://example.test/movie.mp4"
                poster="https://example.test/poster.png"></video>"#,
        );
        let result = rt
            .evaluate(
                r#"
                const media = document.getElementById("media");
                return [
                    media.canPlayType("video/mp4"),
                    media.canPlayType('video/webm; codecs="vp9"'),
                    media.readyState,
                    media.currentTime,
                    media.videoWidth,
                    media.videoHeight,
                    media.paused,
                    media.currentSrc,
                    media.poster,
                ];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([
                "",
                "",
                0,
                0,
                0,
                0,
                true,
                "",
                "https://example.test/poster.png"
            ])
        );
    }

    #[test]
    pub(crate) fn html_string_scripts_remain_inert_when_connected() {
        let mut rt = setup_runtime("<html><head></head><body><div id=target></div></body></html>");
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                globalThis.__fragmentScriptRuns = 0;

                const direct = document.createElement("div");
                direct.innerHTML = "<script>globalThis.__fragmentScriptRuns++<\/script>";
                document.body.appendChild(direct.firstChild);

                const nested = document.createElement("div");
                nested.innerHTML = "<section><script>globalThis.__fragmentScriptRuns++<\/script></section>";
                document.body.appendChild(nested.firstChild);

                const template = document.createElement("template");
                template.innerHTML = "<script>globalThis.__fragmentScriptRuns++<\/script>";
                document.body.appendChild(template.content.firstChild);

                const nestedTemplateHolder = document.createElement("div");
                nestedTemplateHolder.innerHTML =
                    "<template><script>globalThis.__fragmentScriptRuns++<\/script></template>";
                document.body.appendChild(
                    nestedTemplateHolder.firstChild.content.firstChild
                );

                document.getElementById("target").insertAdjacentHTML(
                    "beforeend",
                    "<script>globalThis.__fragmentScriptRuns++<\/script>"
                );

                const parsed = new DOMParser().parseFromString(
                    "<body><script>globalThis.__fragmentScriptRuns++<\/script></body>",
                    "text/html"
                );
                document.body.appendChild(parsed.querySelector("script"));

                let externalFetches = 0;
                const originalFetchOp = Deno.core.ops.op_fetch_url;
                try {
                    Deno.core.ops.op_fetch_url = () => {
                        externalFetches++;
                        return JSON.stringify({
                            status: 200,
                            headers: {"content-type": "text/javascript"},
                            body: "globalThis.__fragmentScriptRuns++",
                            url: "http://example.com/inert.js"
                        });
                    };
                    const external = document.createElement("div");
                    external.innerHTML = "<script src=/inert.js><\/script>";
                    document.head.appendChild(external.firstChild);
                } finally {
                    Deno.core.ops.op_fetch_url = originalFetchOp;
                }
                return [globalThis.__fragmentScriptRuns, externalFetches];
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!([0, 0]));
    }

    #[test]
    pub(crate) fn connected_insertion_prepares_dynamic_script_subtrees_once() {
        let mut rt = setup_runtime("<html><head></head><body><i id=anchor></i></body></html>");
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                globalThis.__dynamicScriptRuns = [];

                const detached = document.createElement("div");
                const delayed = document.createElement("script");
                delayed.textContent = "globalThis.__dynamicScriptRuns.push('delayed')";
                detached.appendChild(delayed);
                const beforeConnection = globalThis.__dynamicScriptRuns.length;
                document.body.appendChild(detached);
                document.head.appendChild(delayed);

                const before = document.createElement("script");
                before.textContent = "globalThis.__dynamicScriptRuns.push('before')";
                document.body.insertBefore(before, document.getElementById("anchor"));

                const replacement = document.createElement("script");
                replacement.textContent = "globalThis.__dynamicScriptRuns.push('replace')";
                document.body.replaceChild(replacement, document.getElementById("anchor"));

                const subtree = document.createElement("section");
                const nested = document.createElement("script");
                nested.textContent = "globalThis.__dynamicScriptRuns.push('nested')";
                subtree.appendChild(nested);
                document.body.appendChild(subtree);

                return [beforeConnection, globalThis.__dynamicScriptRuns];
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!([0, ["delayed", "before", "replace", "nested"]])
        );
    }

    #[test]
    pub(crate) fn script_clone_preserves_started_state() {
        let mut rt = setup_runtime(
            "<html><head></head><body><script id=parser>globalThis.__cloneScriptRuns++</script></body></html>",
        );
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                globalThis.__cloneScriptRuns = 0;
                const parser = document.getElementById("parser");
                globalThis.__markParserScripts([parser._nid]);
                document.head.appendChild(parser);
                document.body.appendChild(parser.cloneNode(true));

                const dynamic = document.createElement("script");
                dynamic.textContent = "globalThis.__cloneScriptRuns++";
                document.body.appendChild(dynamic);
                document.body.appendChild(dynamic.cloneNode(true));

                const holder = document.createElement("div");
                holder.innerHTML = "<script>globalThis.__cloneScriptRuns++<\/script>";
                document.body.appendChild(holder.firstChild.cloneNode(true));

                const fragment = document.createDocumentFragment();
                fragment.appendChild(dynamic.cloneNode(true));
                document.body.appendChild(fragment.cloneNode(true));
                return globalThis.__cloneScriptRuns;
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!(1.0));
    }

    #[test]
    pub(crate) fn contextual_fragment_and_document_write_keep_executable_script_policy() {
        let mut rt = setup_runtime("<html><head></head><body><div id=context></div></body></html>");
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                globalThis.__executableFragmentRuns = [];
                const range = document.createRange();
                range.selectNode(document.getElementById("context"));
                const fragment = range.createContextualFragment(
                    "<template><script>globalThis.__executableFragmentRuns.push('template')<\/script></template>" +
                    "<script>globalThis.__executableFragmentRuns.push('range')<\/script>"
                );
                document.body.appendChild(fragment);
                document.write(
                    "<script>globalThis.__executableFragmentRuns.push('write')<\/script>"
                );
                return globalThis.__executableFragmentRuns;
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!(["range", "write"]));
    }

    // One stream per document. The tokenizer carries its state across the calls.
    // https://html.spec.whatwg.org/multipage/dynamic-markup-insertion.html#dom-document-write
    #[test]
    pub(crate) fn document_write_joins_an_element_split_across_calls() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                document.write('<di');
                document.write('v id="split">');
                document.write('content</div>');
                const el = document.getElementById('split');
                return el ? el.textContent : null;
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!("content"));
    }

    #[test]
    pub(crate) fn document_write_joins_a_tag_name_split_across_calls() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                document.write('<spa');
                document.write('n id="half">x</span>');
                const el = document.getElementById('half');
                return el ? el.tagName : null;
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!("SPAN"));
    }

    // The shape the UI5 cachebuster writes: "<script", one per attribute, then ">".
    #[test]
    pub(crate) fn document_write_runs_a_script_split_across_calls() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                globalThis.__splitScriptRan = false;
                document.write('<scr' + 'ipt');
                document.write(' id="split-script"');
                document.write('>');
                document.write('globalThis.__splitScriptRan = true;');
                document.write('<\/scr' + 'ipt>');
                return [!!document.getElementById('split-script'), globalThis.__splitScriptRan];
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!([true, true]));
    }

    // A script in the <head> inserts behind itself, so that what it writes runs before what
    // the parser saw after it.
    #[test]
    pub(crate) fn document_write_inserts_at_the_writing_scripts_position() {
        let mut rt = setup_runtime(
            r#"<html><head><script id="writer"></script></head><body><p id="existing">x</p></body></html>"#,
        );
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                // What the production path sets while a script runs; bootstrap.js
                // assigns __currentScriptNid around every script it prepares.
                globalThis.__currentScriptNid = document.getElementById('writer')._nid;
                document.write('<span id="written"></span>');
                return JSON.stringify({
                  head: Array.from(document.head.children).map(e => e.id || e.tagName),
                  body: Array.from(document.body.children).map(e => e.id || e.tagName),
                });
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!(r#"{"head":["writer","written"],"body":["existing"]}"#)
        );
    }

    // Holding back until the close would lose everything written after it. It belongs inside.
    #[test]
    pub(crate) fn document_write_shows_an_element_that_is_never_closed() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                document.write('<div id="unclosed">hello');
                const el = document.getElementById('unclosed');
                return el ? el.textContent : null;
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!("hello"));
    }

    #[test]
    pub(crate) fn document_write_grows_an_open_element_across_calls() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                document.write('<div id="wrap">');
                document.write('<span id="inner">y</span>');
                const inner = document.getElementById('inner');
                return JSON.stringify({
                  wrap: !!document.getElementById('wrap'),
                  inner: !!inner,
                  nested: !!(inner && inner.parentElement && inner.parentElement.id === 'wrap'),
                });
                "#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!(r#"{"wrap":true,"inner":true,"nested":true}"#)
        );
    }

    // Writing goes through the same insertion steps as any other insertion.
    #[test]
    pub(crate) fn document_write_reports_to_mutation_observers() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                globalThis.__seen = [];
                const observer = new MutationObserver((records) => {
                  for (const record of records) {
                    for (const node of record.addedNodes) globalThis.__seen.push(node.nodeName);
                  }
                });
                observer.observe(document.body, { childList: true });
                document.write('<span id="watched">z</span>');
                observer.takeRecords().forEach((record) => {
                  for (const node of record.addedNodes) globalThis.__seen.push(node.nodeName);
                });
                return globalThis.__seen.join(',');
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!("SPAN"));
    }

    // before(), after() and replaceWith() all go through parent.insertBefore. replaceChild
    // also goes there in the fragment branch. AGENTS.md requires whoever touches insertBefore
    // to check them: the order of reference node versus parent nid is easy to break. The test
    // also pins that every insertion is reported exactly once, not twice.
    #[test]
    pub(crate) fn child_node_methods_place_nodes_and_report_once() {
        let mut rt = setup_runtime(r#"<html><body><p id="a"></p><p id="b"></p></body></html>"#);
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                const ids = () => Array.from(document.body.children).map((e) => e.id).join(',');
                const make = (id) => { const e = document.createElement('span'); e.id = id; return e; };
                const observer = new MutationObserver(() => {});
                observer.observe(document.body, { childList: true });
                const steps = {};

                document.getElementById('b').before(make('x'));
                steps.before = ids();
                document.getElementById('b').after(make('y'));
                steps.after = ids();
                document.getElementById('y').replaceWith(make('z'));
                steps.replaceWith = ids();
                document.body.replaceChild(make('w'), document.getElementById('z'));
                steps.replaceChild = ids();

                const added = observer.takeRecords()
                  .flatMap((record) => Array.from(record.addedNodes).map((n) => n.id));
                observer.disconnect();
                steps.added = added.join(',');
                return JSON.stringify(steps);
                "#,
            )
            .unwrap();
        let steps: serde_json::Value =
            serde_json::from_str(result.as_str().unwrap()).expect("steps json");
        assert_eq!(steps["before"], "a,x,b");
        assert_eq!(steps["after"], "a,x,b,y");
        assert_eq!(steps["replaceWith"], "a,x,b,z");
        assert_eq!(steps["replaceChild"], "a,x,b,w");
        // Every inserted node exactly once, in the order of insertion.
        assert_eq!(steps["added"], "x,y,z,w");
    }

    // insertBefore reported no mutation at all, appendChild did.
    #[test]
    pub(crate) fn insert_before_reports_to_mutation_observers() {
        let mut rt = setup_runtime("<html><body><p id=\"ref\"></p></body></html>");
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                const observer = new MutationObserver(() => {});
                observer.observe(document.body, { childList: true });
                document.body.insertBefore(
                  document.createElement('span'),
                  document.getElementById('ref'),
                );
                const seen = observer.takeRecords()
                  .flatMap((record) => Array.from(record.addedNodes).map((n) => n.nodeName));
                observer.disconnect();
                return seen.join(',');
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!("SPAN"));
    }

    #[test]
    pub(crate) fn document_write_registers_window_named_access() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                document.write('<img name="namedImage" src="x.png">');
                return typeof window.namedImage;
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!("object"));
    }

    #[test]
    pub(crate) fn document_write_keeps_call_order_at_the_insertion_point() {
        let mut rt = setup_runtime(
            r#"<html><head><script id="writer"></script></head><body></body></html>"#,
        );
        let result = rt
            .evaluate(
                r#"
                var scriptTestSetup = true;
                globalThis.__currentScriptNid = document.getElementById('writer')._nid;
                document.write('<span id="one"></span>');
                document.write('<span id="two"></span>');
                return Array.from(document.head.children).map(e => e.id).join(',');
                "#,
            )
            .unwrap();
        assert_eq!(result, serde_json::json!("writer,one,two"));
    }
