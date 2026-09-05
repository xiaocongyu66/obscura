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
    pub(crate) fn function_to_string_has_native_function_shape() {
        let mut rt = setup_runtime("<html><body></body></html>");

        assert_eq!(
            rt.evaluate(
                r#"(() => {
                    const fn = Function.prototype.toString;
                    let constructible = true;
                    try {
                        Reflect.construct(function () {}, [], fn);
                    } catch (error) {
                        constructible = false;
                    }
                    return {
                        source: fn.toString(),
                        name: fn.name,
                        length: fn.length,
                        hasOwnPrototype: Object.prototype.hasOwnProperty.call(fn, "prototype"),
                        constructible,
                    };
                })()"#,
            )
            .unwrap(),
            serde_json::json!({
                "source": "function toString() { [native code] }",
                "name": "toString",
                "length": 0,
                "hasOwnPrototype": false,
                "constructible": false,
            })
        );
    }

    #[test]
    pub(crate) fn iframe_content_window_exposes_realm_globals() {
        let mut rt = setup_runtime("<html><body></body></html>");

        assert_eq!(
            rt.evaluate(
                r#"(() => {
                    const iframe = document.createElement("iframe");
                    document.body.appendChild(iframe);
                    const child = iframe.contentWindow;
                    const names = [
                        "Object", "Function", "Error", "Promise", "Proxy",
                        "XMLHttpRequest", "Worker", "Blob", "FormData",
                        "WebSocket", "MutationObserver",
                    ];
                    return {
                        types: names.map(name => typeof child[name]),
                        separate: [
                            child.Object !== Object,
                            child.Promise !== Promise,
                            child.XMLHttpRequest !== XMLHttpRequest,
                            child.Math !== Math,
                        ],
                        constructible: [
                            new child.Object() instanceof child.Object,
                            new child.Promise(resolve => resolve()) instanceof child.Promise,
                            new child.XMLHttpRequest() instanceof child.XMLHttpRequest,
                            new child.Blob([]) instanceof child.Blob,
                            new child.FormData() instanceof child.FormData,
                            new child.MutationObserver(() => {}) instanceof child.MutationObserver,
                        ],
                        utilities: [
                            child.Object.keys({ first: 1 })[0] === "first",
                            child.Array.isArray([]),
                            child.Promise.resolve(1) instanceof child.Promise,
                            child.Function("return 7")() === 7,
                            Object.getOwnPropertyNames(child).includes("XMLHttpRequest"),
                            child.globalThis === child,
                        ],
                    };
                })()"#,
            )
            .unwrap(),
            serde_json::json!({
                "types": vec!["function"; 11],
                "separate": vec![true; 4],
                "constructible": vec![true; 6],
                "utilities": vec![true; 6],
            })
        );
    }

    #[test]
    pub(crate) fn document_domain_getter_and_valid_relaxation_match_effective_host() {
        let dom = parse_html("<html><body></body></html>");
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_url("https://deep.assets.example.co.uk:8443/page");
        rt.run_page_init();

        assert_eq!(
            rt.evaluate(
                r#"(() => {
                    const initial = document.domain;
                    document.domain = "ASSETS.EXAMPLE.CO.UK";
                    const first = document.domain;
                    document.domain = "example.co.uk";
                    return [initial, first, document.domain, location.hostname,
                            (new Document()).domain,
                            new DOMParser().parseFromString("", "text/html").domain];
                })()"#,
            )
            .unwrap(),
            serde_json::json!([
                "deep.assets.example.co.uk",
                "assets.example.co.uk",
                "example.co.uk",
                "deep.assets.example.co.uk",
                "example.co.uk",
                "example.co.uk"
            ])
        );
    }

    #[test]
    pub(crate) fn document_domain_rejects_unrelated_child_and_public_suffix_hosts() {
        let dom = parse_html("<html><body></body></html>");
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_url("https://app.user.github.io/page");
        rt.run_page_init();

        assert_eq!(
            rt.evaluate(
                r#"(() => {
                    const attempts = ["", ".github.io", "github.io", "evilgithub.io",
                                      "other.github.io", "child.app.user.github.io"];
                    const rejected = attempts.map(value => {
                        try { document.domain = value; return "accepted"; }
                        catch (error) { return error.name; }
                    });
                    document.domain = "user.github.io";
                    return rejected.concat(document.domain);
                })()"#,
            )
            .unwrap(),
            serde_json::json!([
                "SecurityError",
                "SecurityError",
                "SecurityError",
                "SecurityError",
                "SecurityError",
                "SecurityError",
                "user.github.io"
            ])
        );
    }

    #[test]
    pub(crate) fn document_domain_detached_and_hostless_setters_throw_security_error() {
        let mut rt = setup_runtime("<html><body></body></html>");
        assert_eq!(
            rt.evaluate(
                r#"(() => {
                    const detached = [new Document(),
                        document.implementation.createHTMLDocument("x"),
                        document.implementation.createDocument(null, "root")];
                    const errors = detached.map(doc => {
                        try { doc.domain = "example.com"; return "accepted"; }
                        catch (error) { return error.name; }
                    });
                    return [typeof document.domain, document.domain].concat(errors);
                })()"#,
            )
            .unwrap(),
            serde_json::json!([
                "string",
                "example.com",
                "SecurityError",
                "SecurityError",
                "SecurityError"
            ])
        );

        let dom = parse_html("<html><body></body></html>");
        let mut hostless = ObscuraJsRuntime::new();
        hostless.set_dom(dom);
        hostless.set_url("about:blank");
        hostless.run_page_init();
        assert_eq!(
            hostless
                .evaluate(
                    r#"(() => {
                        let error = "";
                        try { document.domain = "example.com"; }
                        catch (caught) { error = caught.name; }
                        return [document.domain, error];
                    })()"#,
                )
                .unwrap(),
            serde_json::json!(["", "SecurityError"])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn string_timeout_handler_executes_in_global_scope() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.evaluate("var __timerValue='pending'; setTimeout('__timerValue=\"done\"', 0)")
            .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("globalThis.__timerValue").unwrap(),
            serde_json::json!("done")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn string_timeout_declarations_reach_global_scope() {
        // A string timer handler runs as a classic script in global scope, so a
        // top-level var/function declaration in it becomes a global. new Function()
        // kept those declarations local to the compiled function, so they never
        // reached globalThis.
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.evaluate(
            "setTimeout('var __leaked = 42; function __leakedFn(){ return 7; }', 0)",
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        let v = rt
            .evaluate(
                "String(globalThis.__leaked) + '|' + (typeof globalThis.__leakedFn === 'function' ? globalThis.__leakedFn() : 'missing')",
            )
            .unwrap();
        assert_eq!(v, serde_json::json!("42|7"));
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn string_interval_handler_repeats_and_can_clear_itself() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.evaluate("globalThis.__ticks=0").unwrap();
        rt.evaluate(
            "globalThis.__timerId=setInterval('__ticks++;if(__ticks===2)clearInterval(__timerId)',1)",
        )
        .unwrap();
        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("globalThis.__ticks").unwrap(),
            serde_json::json!(2.0)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn zero_delay_timer_runs_as_a_task_after_microtasks() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "zero-delay-task-order",
            r#"
                globalThis.__taskOrder = ["sync"];
                setTimeout(() => __taskOrder.push("timer"), 0);
                Promise.resolve().then(() => __taskOrder.push("microtask"));
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("__taskOrder").unwrap(),
            serde_json::json!(["sync", "microtask", "timer"])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn scheduler_post_task_observes_priority_fifo_and_task_boundaries() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "scheduler-priority-order",
            r#"
                globalThis.__schedulerOrder = ["sync"];
                const schedule = (name, priority) => scheduler.postTask(() => {
                    __schedulerOrder.push(name);
                    Promise.resolve().then(() => __schedulerOrder.push(name + "-microtask"));
                    return name + "-result";
                }, { priority });
                globalThis.__schedulerResults = Promise.all([
                    schedule("background-1", "background"),
                    schedule("background-2", "background"),
                    schedule("visible", "user-visible"),
                    schedule("blocking-1", "user-blocking"),
                    schedule("blocking-2", "user-blocking"),
                ]).then(values => { globalThis.__schedulerValues = values; });
                Promise.resolve().then(() => __schedulerOrder.push("initial-microtask"));
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("__schedulerOrder").unwrap(),
            serde_json::json!([
                "sync",
                "initial-microtask",
                "blocking-1",
                "blocking-1-microtask",
                "blocking-2",
                "blocking-2-microtask",
                "visible",
                "visible-microtask",
                "background-1",
                "background-1-microtask",
                "background-2",
                "background-2-microtask",
            ])
        );
        assert_eq!(
            rt.evaluate("__schedulerValues").unwrap(),
            serde_json::json!([
                "background-1-result",
                "background-2-result",
                "visible-result",
                "blocking-1-result",
                "blocking-2-result",
            ])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn scheduler_abort_delay_and_yield_follow_task_state() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "scheduler-abort-delay-yield",
            r#"
                globalThis.__schedulerState = {
                    order: [],
                    canceledCallbackRan: false,
                    exactAbortReason: false,
                    selfAbortCallbackRan: false,
                    exactSelfAbortReason: false,
                };
                const abortReason = { reason: "stop" };
                const canceled = new AbortController();
                scheduler.postTask(() => {
                    __schedulerState.canceledCallbackRan = true;
                }, { signal: canceled.signal, delay: 20 }).catch(error => {
                    __schedulerState.exactAbortReason = error === abortReason;
                });
                canceled.abort(abortReason);

                const selfAbortReason = { reason: "inside callback" };
                const selfCanceled = new AbortController();
                scheduler.postTask(() => {
                    __schedulerState.selfAbortCallbackRan = true;
                    selfCanceled.abort(selfAbortReason);
                    return "ignored result";
                }, { signal: selfCanceled.signal }).catch(error => {
                    __schedulerState.exactSelfAbortReason = error === selfAbortReason;
                });

                scheduler.postTask(async () => {
                    __schedulerState.order.push("blocking-start");
                    await scheduler.yield();
                    __schedulerState.order.push("blocking-continuation");
                }, { priority: "user-blocking" });
                scheduler.postTask(() => {
                    __schedulerState.order.push("background");
                }, { priority: "background" });
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate(
                r#"[
                    __schedulerState.order,
                    __schedulerState.canceledCallbackRan,
                    __schedulerState.exactAbortReason,
                    __schedulerState.selfAbortCallbackRan,
                    __schedulerState.exactSelfAbortReason,
                    scheduler instanceof Scheduler,
                    Object.prototype.toString.call(scheduler),
                    Scheduler.prototype.postTask.length,
                    Scheduler.prototype.yield.length,
                ]"#,
            )
            .unwrap(),
            serde_json::json!([
                ["blocking-start", "blocking-continuation", "background"],
                false,
                true,
                true,
                true,
                true,
                "[object Scheduler]",
                1,
                0,
            ])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn self_requeueing_message_channel_yields_to_timers() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "message-channel-task-yield",
            r#"
                globalThis.__messageCount = 0;
                globalThis.__timerObserved = false;
                const channel = new MessageChannel();
                channel.port2.onmessage = () => {
                    __messageCount++;
                    if (!__timerObserved) channel.port1.postMessage(null);
                };
                channel.port1.postMessage(null);
                setTimeout(() => { __timerObserved = true; }, 1);
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        let result = rt.evaluate("[__messageCount, __timerObserved]").unwrap();
        let values = result.as_array().unwrap();
        assert!(
            values[0]
                .as_u64()
                .is_some_and(|count| count > 0 && count < 10_000),
            "message task did not yield: {result}"
        );
        assert_eq!(values[1], serde_json::json!(true));
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn message_port_queues_until_start_and_clones_at_post_time() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "message-port-start-and-clone",
            r#"
                const channel = new MessageChannel();
                const payload = { nested: { value: 7 } };
                globalThis.__messagePortResult = {
                    portInstance: channel.port1 instanceof MessagePort,
                    channelInstance: channel instanceof MessageChannel,
                    deliveredBeforeStart: false,
                    delivered: false,
                };
                channel.port2.addEventListener("message", function(event) {
                    __messagePortResult.delivered = true;
                    __messagePortResult.value = event.data.nested.value;
                    __messagePortResult.targetIsPort = event.target === channel.port2;
                    __messagePortResult.thisIsPort = this === channel.port2;
                    __messagePortResult.origin = event.origin;
                    __messagePortResult.portCount = event.ports.length;
                });
                channel.port1.postMessage(payload);
                payload.nested.value = 99;
                setTimeout(() => {
                    __messagePortResult.deliveredBeforeStart = __messagePortResult.delivered;
                    channel.port2.start();
                }, 0);
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("__messagePortResult").unwrap(),
            serde_json::json!({
                "portInstance": true,
                "channelInstance": true,
                "deliveredBeforeStart": false,
                "delivered": true,
                "value": 7,
                "targetIsPort": true,
                "thisIsPort": true,
                "origin": "",
                "portCount": 0,
            })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn message_port_onmessage_starts_and_yields_between_messages() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "message-port-task-boundaries",
            r#"
                globalThis.__messagePortOrder = [];
                const channel = new MessageChannel();
                channel.port1.postMessage(1);
                channel.port1.postMessage(2);
                channel.port2.onmessage = (event) => {
                    __messagePortOrder.push("message-" + event.data);
                    if (event.currentTarget !== channel.port2) __messagePortOrder.push("bad-current-target");
                    Promise.resolve().then(() => __messagePortOrder.push("microtask-" + event.data));
                };
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("__messagePortOrder").unwrap(),
            serde_json::json!(["message-1", "microtask-1", "message-2", "microtask-2",])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn message_port_close_discards_delivery_already_queued_for_a_task() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "message-port-close-cancels-queued-delivery",
            r#"
                globalThis.__closedPortDeliveries = 0;
                const channel = new MessageChannel();
                channel.port2.onmessage = () => { __closedPortDeliveries++; };
                channel.port1.postMessage("queued");
                channel.port2.close();
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("__closedPortDeliveries").unwrap(),
            serde_json::json!(0.0)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn message_port_handler_and_listener_follow_registration_order() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "message-port-mixed-registration-order",
            r#"
                globalThis.__messagePortRegistrationOrder = [];

                const handlerFirst = new MessageChannel();
                handlerFirst.port2.onmessage = () => __messagePortRegistrationOrder.push("handler-first:handler");
                handlerFirst.port2.addEventListener("message", () => __messagePortRegistrationOrder.push("handler-first:listener"));
                handlerFirst.port1.postMessage(null);

                const listenerFirst = new MessageChannel();
                listenerFirst.port2.addEventListener("message", () => __messagePortRegistrationOrder.push("listener-first:listener"));
                listenerFirst.port2.onmessage = () => __messagePortRegistrationOrder.push("listener-first:handler");
                listenerFirst.port1.postMessage(null);
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("__messagePortRegistrationOrder").unwrap(),
            serde_json::json!([
                "handler-first:handler",
                "handler-first:listener",
                "listener-first:listener",
                "listener-first:handler",
            ])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn message_port_internal_state_is_hidden_and_ignores_own_property_tampering() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "message-port-hidden-state",
            r#"
                const channel = new MessageChannel();
                globalThis.__messagePortOwnKeys = Object.keys(channel.port2);
                globalThis.__messagePortOwnNames = Object.getOwnPropertyNames(channel.port2);
                globalThis.__messagePortTamperResult = [];
                channel.port2.onmessage = (event) => __messagePortTamperResult.push(event.data);

                // These names used to be the actual implementation state. An
                // expando with any of them must not alter delivery now.
                channel.port1._closed = true;
                channel.port1._entangled = null;
                channel.port2._closed = true;
                channel.port2._messageQueue = [];
                channel.port2._messageQueueEnabled = false;
                channel.port2._messageDeliveryPending = true;
                channel.port2._onmessage = null;
                channel.port2._scheduleMessageDelivery = () => {};
                channel.port2.dispatchEvent = () => { throw new Error("tampered dispatchEvent called"); };
                channel.port1.postMessage("delivered");
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("[__messagePortOwnKeys, __messagePortOwnNames, __messagePortTamperResult]")
                .unwrap(),
            serde_json::json!([[], [], ["delivered"]])
        );
    }

    #[test]
    pub(crate) fn message_port_has_browser_shaped_construction_and_clone_errors() {
        let mut rt = setup_runtime("<html><body></body></html>");
        let result = rt
            .evaluate(
                r#"(() => {
                    let constructorError = "";
                    let cloneError = "";
                    try { new MessagePort(); } catch (error) { constructorError = error.name; }
                    try { new MessageChannel().port1.postMessage(() => {}); }
                    catch (error) { cloneError = error.name; }
                    return [constructorError, cloneError, Object.prototype.toString.call(new MessageChannel().port1)];
                })()"#,
            )
            .unwrap();
        assert_eq!(
            result,
            serde_json::json!(["TypeError", "DataCloneError", "[object MessagePort]"])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn broadcast_channel_delivers_independent_post_time_clones_to_matching_peers() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "broadcast-channel-clone-delivery",
            r#"
                globalThis.__broadcastResults = { sender: 0, otherName: 0, peers: [] };
                const sender = new BroadcastChannel("session-sync");
                const first = new BroadcastChannel("session-sync");
                const second = new BroadcastChannel("session-sync");
                const other = new BroadcastChannel("other-name");
                sender.onmessage = () => { __broadcastResults.sender++; };
                other.onmessage = () => { __broadcastResults.otherName++; };
                first.onmessage = (event) => {
                    __broadcastResults.peers.push({
                        peer: "first",
                        value: event.data.nested.value,
                        bytes: Array.from(event.data.bytes),
                        source: event.source,
                        ports: event.ports.length,
                    });
                    event.data.nested.value = 500;
                    event.data.bytes[0] = 99;
                };
                second.onmessage = (event) => {
                    __broadcastResults.peers.push({
                        peer: "second",
                        value: event.data.nested.value,
                        bytes: Array.from(event.data.bytes),
                        source: event.source,
                        ports: event.ports.length,
                    });
                };
                const payload = { nested: { value: 7 }, bytes: new Uint8Array([1, 2, 3]) };
                sender.postMessage(payload);
                payload.nested.value = 42;
                payload.bytes[0] = 88;
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("__broadcastResults").unwrap(),
            serde_json::json!({
                "sender": 0,
                "otherName": 0,
                "peers": [
                    { "peer": "first", "value": 7, "bytes": [1, 2, 3], "source": null, "ports": 0 },
                    { "peer": "second", "value": 7, "bytes": [1, 2, 3], "source": null, "ports": 0 },
                ],
            })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn broadcast_channel_handlers_follow_registration_order_and_task_timing() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "broadcast-channel-registration-order",
            r#"
                globalThis.__broadcastOrder = ["sync"];
                const sender = new BroadcastChannel("ordering");
                const handlerFirst = new BroadcastChannel("ordering");
                const listenerFirst = new BroadcastChannel("ordering");
                handlerFirst.onmessage = () => __broadcastOrder.push("handler-first:handler");
                handlerFirst.addEventListener("message", () => __broadcastOrder.push("handler-first:listener"));
                listenerFirst.addEventListener("message", () => __broadcastOrder.push("listener-first:listener"));
                listenerFirst.onmessage = () => __broadcastOrder.push("listener-first:handler");
                sender.postMessage(null);
                Promise.resolve().then(() => __broadcastOrder.push("microtask"));
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("__broadcastOrder").unwrap(),
            serde_json::json!([
                "sync",
                "microtask",
                "handler-first:handler",
                "handler-first:listener",
                "listener-first:listener",
                "listener-first:handler",
            ])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn broadcast_channel_close_cancels_delivery_and_closed_post_throws() {
        let mut rt = setup_runtime("<html><body></body></html>");
        rt.execute_script(
            "broadcast-channel-close",
            r#"
                globalThis.__broadcastCloseResult = { deliveries: 0 };
                const sender = new BroadcastChannel("close-test");
                const recipient = new BroadcastChannel("close-test");
                recipient.onmessage = () => { __broadcastCloseResult.deliveries++; };
                sender.postMessage("queued");
                recipient.close();
                sender.close();
                try { sender.postMessage("closed"); }
                catch (error) { __broadcastCloseResult.closedError = error.name; }
                try { new BroadcastChannel(); }
                catch (error) { __broadcastCloseResult.constructorError = error.name; }
                try { new BroadcastChannel("no-peers").postMessage(() => {}); }
                catch (error) { __broadcastCloseResult.cloneError = error.name; }
                __broadcastCloseResult.ownKeys = Object.keys(new BroadcastChannel("shape"));
                __broadcastCloseResult.tag = Object.prototype.toString.call(new BroadcastChannel("shape"));
                __broadcastCloseResult.eventTarget = new BroadcastChannel("shape") instanceof EventTarget;
            "#,
        )
        .unwrap();

        rt.run_event_loop_bounded(100).await.unwrap();
        assert_eq!(
            rt.evaluate("__broadcastCloseResult").unwrap(),
            serde_json::json!({
                "deliveries": 0,
                "closedError": "InvalidStateError",
                "constructorError": "TypeError",
                "cloneError": "DataCloneError",
                "ownKeys": [],
                "tag": "[object BroadcastChannel]",
                "eventTarget": true,
            })
        );
    }
