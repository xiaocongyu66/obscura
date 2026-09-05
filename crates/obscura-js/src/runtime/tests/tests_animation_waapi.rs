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
    pub(crate) fn waapi_pause_seek_and_cancel_preserve_authored_inline_style() {
        let mut rt = setup_runtime(
            r#"<html><body><div id="box" style="opacity:.2;width:20px;height:20px"></div></body></html>"#,
        );
        rt.execute_script(
            "waapi",
            r#"
                globalThis.box = document.getElementById('box');
                globalThis.__animation = box.animate(
                    [{opacity:.2, transform:'translateX(0px)'}, {opacity:1, transform:'translateX(100px)'}],
                    {duration:100, fill:'both', easing:'linear'}
                );
                __animation.pause();
                __animation.currentTime = 50;
            "#,
        ).unwrap();
        assert_eq!(rt.evaluate("box.style.opacity").unwrap(), serde_json::json!(".2"));
        assert_eq!(rt.evaluate("box.getAnimations()[0] === __animation").unwrap(), serde_json::json!(true));
        assert_eq!(rt.evaluate("document.getAnimations()[0] === __animation").unwrap(), serde_json::json!(true));
        assert_eq!(rt.evaluate("__animation.playState").unwrap(), serde_json::json!("paused"));
        assert_eq!(
            rt.evaluate("!('easingBezier' in __animation.effect.getTiming()) && !('linearEasing' in __animation.effect.getComputedTiming())").unwrap(),
            serde_json::json!(true),
        );
        let opacity = rt.evaluate("getComputedStyle(box).opacity").unwrap();
        let opacity = opacity.as_str().unwrap().parse::<f32>().unwrap();
        assert!((opacity - 0.6).abs() < 0.001, "midpoint opacity was {opacity}");

        rt.execute_script("cancel", "__animation.cancel()").unwrap();
        assert_eq!(rt.evaluate("box.style.opacity").unwrap(), serde_json::json!(".2"));
        assert_eq!(rt.evaluate("getComputedStyle(box).opacity").unwrap(), serde_json::json!("0.2"));
        assert_eq!(rt.evaluate("box.getAnimations().length").unwrap(), serde_json::json!(0.0));
        assert_eq!(rt.evaluate("document.getAnimations().length").unwrap(), serde_json::json!(0.0));
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn waapi_zero_duration_finishes_asynchronously_and_fires_lifecycle() {
        let mut rt = setup_runtime(r#"<div id="box" style="opacity:.1"></div>"#);
        rt.execute_script(
            "waapi-lifecycle",
            r#"
                globalThis.box = document.getElementById('box');
                globalThis.__ready = false;
                globalThis.__finished = false;
                globalThis.__finishEvent = false;
                globalThis.__animation = box.animate([{opacity:.1}, {opacity:1}], {duration:0, fill:'both'});
                __animation.onfinish = () => { __finishEvent = true; };
                __animation.ready.then(() => { __ready = true; });
                __animation.finished.then(() => { __finished = true; });
            "#,
        ).unwrap();
        rt.run_event_loop_bounded(20).await.unwrap();
        assert_eq!(rt.evaluate("__ready").unwrap(), serde_json::json!(true));
        assert_eq!(rt.evaluate("__finished").unwrap(), serde_json::json!(true));
        assert_eq!(rt.evaluate("__finishEvent").unwrap(), serde_json::json!(true));
        assert_eq!(rt.evaluate("__animation.playState").unwrap(), serde_json::json!("finished"));
        assert_eq!(rt.evaluate("getComputedStyle(box).opacity").unwrap(), serde_json::json!("1"));
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn waapi_positive_infinite_iterations_remain_active() {
        let mut rt = setup_runtime(r#"<div id="box" style="opacity:.1"></div>"#);
        rt.execute_script(
            "waapi-infinite",
            r#"
                globalThis.__infiniteFinished = false;
                globalThis.__infiniteAnimation = document.getElementById('box').animate(
                    [{opacity:.1}, {opacity:1}],
                    {duration:1, iterations:Infinity, fill:'both', easing:'linear'}
                );
                __infiniteAnimation.finished.then(() => { __infiniteFinished = true; });
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(20).await.unwrap();
        assert_eq!(
            rt.evaluate(
                "[__infiniteAnimation.playState, __infiniteAnimation.effect.getTiming().iterations === Infinity, __infiniteFinished]",
            )
            .unwrap(),
            serde_json::json!(["running", true, false]),
        );
        rt.evaluate("getComputedStyle(document.getElementById('box')).opacity")
            .unwrap();
        assert!(rt.prepared_has_active_css_animations());
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn forward_animation_samples_retain_static_prepared_render() {
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div style="width:80px;height:60px;background:#1769aa"></div>
            </body></html>"#,
        ));
        rt.set_url("http://example.test/page");
        rt.set_viewport(80.0, 60.0);
        rt.run_page_init();

        assert!(rt.set_animation_sample_time(obscura_render::AnimationSampleTime {
            milliseconds: 100.0,
        }));
        let first = rt
            .screenshot_prepared((80.0, 60.0), Some("http://example.test/page"))
            .expect("first static frame");
        let prepared_address = {
            let state = rt.state.borrow();
            state.prepared_render.as_ref().unwrap() as *const _ as usize
        };

        assert!(rt.set_animation_sample_time(obscura_render::AnimationSampleTime {
            milliseconds: 250.0,
        }));
        let second = rt
            .screenshot_prepared((80.0, 60.0), Some("http://example.test/page"))
            .expect("second static frame");
        let state = rt.state.borrow();
        assert_eq!(
            state.prepared_render.as_ref().unwrap() as *const _ as usize,
            prepared_address,
            "a live timestamp alone must not relayout a static document"
        );
        assert_eq!(first, second);
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn forward_active_animation_sample_updates_geometry_and_paint_from_retained_frame() {
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(parse_html(
            r#"<html style="margin:0"><head><style>
                @keyframes grow {
                    from { width:20px; background-color:#ff0000 }
                    to { width:100px; background-color:#0000ff }
                }
                #box { height:40px; animation:grow 1000ms linear both }
            </style></head><body style="margin:0"><div id="box"></div></body></html>"#,
        ));
        rt.set_url("http://example.test/page");
        rt.set_viewport(120.0, 40.0);
        rt.run_page_init();

        assert!(rt.set_animation_sample(obscura_render::AnimationSample::document(0.0)));
        let initial = rt
            .screenshot_prepared((120.0, 40.0), Some("http://example.test/page"))
            .expect("initial animation frame");
        assert!((animation_test_width(&rt, "box") - 20.0).abs() < 0.1);

        assert!(rt.set_animation_sample(obscura_render::AnimationSample::document(500.0)));
        assert!(
            rt.state.borrow().prepared_render.is_some(),
            "a forward active sample should retain the previous style graph until flush"
        );
        let midpoint = rt
            .screenshot_prepared((120.0, 40.0), Some("http://example.test/page"))
            .expect("retained midpoint animation frame");

        assert!((animation_test_width(&rt, "box") - 60.0).abs() < 0.1);
        assert_ne!(initial, midpoint, "animated paint output must advance");
        assert_eq!(
            rt.state
                .borrow()
                .prepared_render
                .as_ref()
                .unwrap()
                .animation_sample_time()
                .milliseconds,
            500.0
        );
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn forward_waapi_sample_updates_retained_style_and_paint() {
        let mut rt = setup_runtime(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="box" style="width:40px;height:40px;background:#1769aa"></div>
            </body></html>"#,
        );
        rt.set_viewport(120.0, 40.0);
        rt.state.borrow_mut().animation_timeline_origin = std::time::Instant::now();
        assert!(rt.set_animation_sample(obscura_render::AnimationSample::document(0.0)));
        rt.screenshot_prepared((120.0, 40.0), Some("http://example.com/test"))
            .expect("static frame before WAAPI registration");
        rt.execute_script(
            "waapi-retained-frame",
            r#"document.getElementById('box').animate(
                [{opacity:0,transform:'translateX(0px)'},
                 {opacity:1,transform:'translateX(80px)'}],
                {duration:1000,fill:'both',easing:'linear'}
            )"#,
        )
        .unwrap();
        {
            let state = rt.state.borrow();
            let box_node = state
                .dom
                .as_ref()
                .unwrap()
                .get_element_by_id("box")
                .unwrap();
            assert!(
                state.prepared_render.is_some(),
                "registering one WAAPI effect must retain the previous style graph"
            );
            assert_eq!(
                state.pending_style_mutations,
                vec![obscura_render::RetainedStyleMutation::WaapiAnimation {
                    node: box_node
                }]
            );
        }
        let initial = rt
            .screenshot_prepared((120.0, 40.0), Some("http://example.com/test"))
            .expect("initial WAAPI frame");

        assert!(rt.set_animation_sample(obscura_render::AnimationSample::document(500.0)));
        assert!(
            rt.state.borrow().prepared_render.is_some(),
            "a forward WAAPI sample should preserve the prepared style graph until flush"
        );
        let midpoint = rt
            .screenshot_prepared((120.0, 40.0), Some("http://example.com/test"))
            .expect("retained WAAPI midpoint");
        let midpoint_opacity = {
            let state = rt.state.borrow();
            let dom = state.dom.as_ref().unwrap();
            let box_node = dom.get_element_by_id("box").unwrap();
            state.prepared_render.as_ref().unwrap().layout().styles[&box_node]
                .opacity
                .unwrap()
        };

        assert!(
            (0.45..0.55).contains(&midpoint_opacity),
            "WAAPI midpoint opacity={midpoint_opacity}"
        );
        assert_ne!(initial, midpoint, "WAAPI paint output must advance");
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn waapi_cancel_retains_static_style_graph_and_restores_authored_style() {
        let mut rt = setup_runtime(
            r#"<html><body><div id="box" style="opacity:.25;width:20px;height:20px"></div></body></html>"#,
        );
        rt.set_viewport(40.0, 40.0);
        rt.screenshot_prepared((40.0, 40.0), Some("http://example.com/test"))
            .expect("static frame");
        rt.execute_script(
            "waapi-retained-cancel",
            r#"globalThis.__cancelAnimation = document.getElementById('box').animate(
                [{opacity:1}, {opacity:0}], {duration:1000, fill:'both'}
            )"#,
        )
        .unwrap();
        let animated_opacity = rt
            .evaluate("Number(getComputedStyle(document.getElementById('box')).opacity)")
            .unwrap()
            .as_f64()
            .unwrap();
        assert!(animated_opacity > 0.9, "animated opacity={animated_opacity}");

        rt.evaluate("__cancelAnimation.cancel()").unwrap();
        assert!(
            rt.state.borrow().prepared_render.is_some(),
            "canceling one WAAPI effect must retain the previous style graph until recascade"
        );
        assert_eq!(
            rt.evaluate("getComputedStyle(document.getElementById('box')).opacity")
                .unwrap(),
            serde_json::json!("0.25")
        );
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn completed_animation_retains_forward_frame_but_backward_seek_rebuilds() {
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(parse_html(
            r#"<html style="margin:0"><head><style>
                @keyframes fade { from { opacity:1 } to { opacity:0 } }
                #box { width:80px; height:60px; background:#ff0000;
                       animation:fade 100ms linear forwards }
            </style></head><body style="margin:0"><div id="box"></div></body></html>"#,
        ));
        rt.set_url("http://example.test/page");
        rt.set_viewport(80.0, 60.0);
        rt.run_page_init();

        assert!(rt.set_animation_sample_time(obscura_render::AnimationSampleTime {
            milliseconds: 150.0,
        }));
        let completed = rt
            .screenshot_prepared((80.0, 60.0), Some("http://example.test/page"))
            .expect("completed animation frame");
        assert!(!rt.prepared_has_active_css_animations());
        let prepared_address = {
            let state = rt.state.borrow();
            state.prepared_render.as_ref().unwrap() as *const _ as usize
        };

        assert!(rt.set_animation_sample_time(obscura_render::AnimationSampleTime {
            milliseconds: 300.0,
        }));
        let later = rt
            .screenshot_prepared((80.0, 60.0), Some("http://example.test/page"))
            .expect("later completed frame");
        assert_eq!(completed, later);
        assert_eq!(
            rt.state
                .borrow()
                .prepared_render
                .as_ref()
                .unwrap() as *const _ as usize,
            prepared_address,
            "a finite fill-forwards animation must not relayout after completion"
        );

        assert!(rt.set_animation_sample_time(obscura_render::AnimationSampleTime {
            milliseconds: 0.0,
        }));
        assert!(
            rt.state.borrow().prepared_render.is_none(),
            "backward timeline seeks must invalidate the completed frame"
        );
        let initial = rt
            .screenshot_prepared((80.0, 60.0), Some("http://example.test/page"))
            .expect("initial animation frame");
        assert_ne!(completed, initial);
        assert!(rt.prepared_has_active_css_animations());
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn unsupported_custom_property_animation_does_not_keep_render_damage_active() {
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(parse_html(
            r#"<html style="margin:0"><head><style>
                @property --brand-cycle { syntax:"<color>"; inherits:true; initial-value:#2dacf9 }
                @keyframes brand-cycle {
                    from { --brand-cycle:#2dacf9 }
                    to { --brand-cycle:#7ce95a }
                }
                :root { animation:brand-cycle 10s linear infinite }
            </style></head><body style="margin:0">
                <div style="width:80px;height:60px;background:#1769aa"></div>
            </body></html>"#,
        ));
        rt.set_url("http://example.test/page");
        rt.set_viewport(80.0, 60.0);
        rt.run_page_init();

        let first = rt
            .screenshot_prepared((80.0, 60.0), Some("http://example.test/page"))
            .expect("initial frame");
        assert!(
            !rt.prepared_has_active_css_animations(),
            "an unsupported custom-property-only animation has no render damage"
        );
        let prepared_address = {
            let state = rt.state.borrow();
            state.prepared_render.as_ref().unwrap() as *const _ as usize
        };
        assert!(rt.set_animation_sample_time(obscura_render::AnimationSampleTime {
            milliseconds: 5_000.0,
        }));
        let later = rt
            .screenshot_prepared((80.0, 60.0), Some("http://example.test/page"))
            .expect("later frame");
        assert_eq!(first, later);
        assert_eq!(
            rt.state.borrow().prepared_render.as_ref().unwrap() as *const _ as usize,
            prepared_address
        );
    }

    #[cfg(feature = "render")]
    pub(crate) fn animation_test_width(rt: &ObscuraJsRuntime, id: &str) -> f32 {
        let state = rt.state.borrow();
        let dom = state.dom.as_ref().unwrap();
        let node = dom.query_selector(&format!("#{id}")).unwrap().unwrap();
        match state.prepared_render.as_ref().unwrap().layout().styles[&node].width {
            obscura_render::Dimension::Px(width) => width,
            ref other => panic!("expected animated pixel width, got {other:?}"),
        }
    }

    #[cfg(feature = "render")]
    pub(crate) fn animation_epoch_runtime() -> ObscuraJsRuntime {
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(parse_html(
            r#"<html style="margin:0"><head><style>
                @keyframes grow { from { width:0px } to { width:100px } }
                .anim { height:10px; animation:grow 1000ms linear forwards }
            </style></head><body style="margin:0"><i id="anchor"></i></body></html>"#,
        ));
        rt.set_url("http://example.test/page");
        rt.set_viewport(200.0, 80.0);
        rt.run_page_init();
        rt
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn remove_and_reappend_restarts_animation_without_intermediate_flush() {
        let mut rt = animation_epoch_runtime();
        rt.evaluate("var box=document.createElement('div');box.id='box';box.className='anim';document.body.appendChild(box)")
            .unwrap();
        assert!(rt.set_animation_sample(obscura_render::AnimationSample::document(1_000.0)));
        rt.screenshot_prepared((200.0, 80.0), Some("http://example.test/page"))
            .unwrap();
        assert!(animation_test_width(&rt, "box") > 95.0);

        rt.state.borrow_mut().animation_timeline_origin =
            std::time::Instant::now() - std::time::Duration::from_millis(1_000);
        rt.evaluate("var box=document.getElementById('box');box.remove();document.body.appendChild(box)")
            .unwrap();
        assert!(rt.set_animation_sample(obscura_render::AnimationSample::document(1_100.0)));
        rt.screenshot_prepared((200.0, 80.0), Some("http://example.test/page"))
            .unwrap();
        let restarted = animation_test_width(&rt, "box");
        assert!((5.0..20.0).contains(&restarted), "restarted width={restarted}");
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn scoped_animation_epochs_survive_later_unrelated_mutations_and_t0_capture() {
        let mut rt = animation_epoch_runtime();
        rt.state.borrow_mut().animation_timeline_origin =
            std::time::Instant::now() - std::time::Duration::from_millis(100);
        rt.evaluate("var a=document.createElement('div');a.id='first';a.className='anim';document.body.appendChild(a)")
            .unwrap();
        rt.state.borrow_mut().animation_timeline_origin =
            std::time::Instant::now() - std::time::Duration::from_millis(500);
        rt.evaluate("var b=document.createElement('div');b.id='second';b.className='anim';document.body.appendChild(b)")
            .unwrap();
        rt.state.borrow_mut().animation_timeline_origin =
            std::time::Instant::now() - std::time::Duration::from_millis(600);
        rt.evaluate("document.getElementById('anchor').setAttribute('data-unrelated','yes')")
            .unwrap();

        assert!(rt.set_animation_sample(obscura_render::AnimationSample::local_override(0.0)));
        rt.screenshot_prepared((200.0, 80.0), Some("http://example.test/page"))
            .unwrap();
        assert_eq!(animation_test_width(&rt, "first"), 0.0);
        assert_eq!(animation_test_width(&rt, "second"), 0.0);

        assert!(rt.set_animation_sample(obscura_render::AnimationSample::document(700.0)));
        rt.screenshot_prepared((200.0, 80.0), Some("http://example.test/page"))
            .unwrap();
        let first = animation_test_width(&rt, "first");
        let second = animation_test_width(&rt, "second");
        assert!((55.0..65.0).contains(&first), "first width={first}");
        assert!((15.0..25.0).contains(&second), "second width={second}");
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn timing_edits_preserve_identity_and_pause_holds_then_resumes() {
        let mut rt = animation_epoch_runtime();
        rt.evaluate("var box=document.createElement('div');box.id='box';box.className='anim';document.body.appendChild(box)")
            .unwrap();
        assert!(rt.set_animation_sample(obscura_render::AnimationSample::document(300.0)));
        rt.screenshot_prepared((200.0, 80.0), Some("http://example.test/page"))
            .unwrap();
        assert!((25.0..35.0).contains(&animation_test_width(&rt, "box")));

        rt.state.borrow_mut().animation_timeline_origin =
            std::time::Instant::now() - std::time::Duration::from_millis(300);
        rt.evaluate("document.getElementById('box').setAttribute('style','animation-duration:2000ms;animation-play-state:paused')")
            .unwrap();
        assert!(rt.set_animation_sample(obscura_render::AnimationSample::document(700.0)));
        rt.screenshot_prepared((200.0, 80.0), Some("http://example.test/page"))
            .unwrap();
        let held = animation_test_width(&rt, "box");
        assert!((12.0..18.0).contains(&held), "held width={held}");

        rt.state.borrow_mut().animation_timeline_origin =
            std::time::Instant::now() - std::time::Duration::from_millis(800);
        rt.evaluate("document.getElementById('box').setAttribute('style','animation-duration:2000ms;animation-play-state:running')")
            .unwrap();
        assert!(rt.set_animation_sample(obscura_render::AnimationSample::document(1_000.0)));
        rt.screenshot_prepared((200.0, 80.0), Some("http://example.test/page"))
            .unwrap();
        let resumed = animation_test_width(&rt, "box");
        assert!((22.0..28.0).contains(&resumed), "resumed width={resumed}");
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn cssom_geometry_samples_live_document_time() {
        let mut rt = animation_epoch_runtime();
        rt.evaluate("var box=document.createElement('div');box.id='box';box.className='anim';document.body.appendChild(box)")
            .unwrap();
        let initial = rt
            .evaluate("document.getElementById('box').getBoundingClientRect().width")
            .unwrap()
            .as_f64()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(120));
        let later = rt
            .evaluate("document.getElementById('box').getBoundingClientRect().width")
            .unwrap()
            .as_f64()
            .unwrap();
        assert!(later >= initial + 8.0, "initial={initial}, later={later}");
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn fixed_animation_capture_is_invariant_after_geometry_flush() {
        let make_runtime = || {
            let mut rt = ObscuraJsRuntime::new();
            rt.set_dom(parse_html(
                r#"<html style="margin:0"><head><style>
                    @keyframes dismiss {
                        from { opacity:1; transform:translateY(0) }
                        to { opacity:0; transform:translateY(-80px) }
                    }
                    body { margin:0; width:160px; height:100px; background:#f5f7fa }
                    #content { width:120px; height:50px; margin:20px; background:#1769aa }
                    #shell { position:fixed; inset:0; background:#111827 }
                    #shell.dismissed { animation:dismiss 600ms linear forwards }
                </style></head><body><div id="content"></div>
                    <div id="shell"></div></body></html>"#,
            ));
            rt.set_url("http://example.test/github-like-shell");
            rt.set_viewport(160.0, 100.0);
            rt.run_page_init();
            rt.evaluate("document.getElementById('shell').className='dismissed'")
                .unwrap();
            rt
        };
        let fixed = obscura_render::AnimationSample::local_override(750.0);
        let direct_rt = make_runtime();
        assert!(direct_rt.set_animation_sample(fixed));
        let direct = direct_rt
            .screenshot_prepared((160.0, 100.0), Some("http://example.test/github-like-shell"))
            .expect("direct fixed-time capture");

        let mut geometry_rt = make_runtime();
        let rect = geometry_rt
            .evaluate("document.getElementById('content').getBoundingClientRect().toJSON()")
            .expect("geometry flush before capture");
        assert_eq!(rect["width"].as_f64(), Some(120.0));
        assert!(geometry_rt.set_animation_sample(fixed));
        let after_geometry = geometry_rt
            .screenshot_prepared((160.0, 100.0), Some("http://example.test/github-like-shell"))
            .expect("fixed-time capture after geometry");

        assert_eq!(
            direct, after_geometry,
            "a CSSOM geometry flush must not change fixed-time capture output"
        );
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn cssom_animation_sample_is_frozen_within_one_javascript_task() {
        let mut rt = animation_epoch_runtime();
        rt.evaluate("var box=document.createElement('div');box.id='box';box.className='anim';document.body.appendChild(box)")
            .unwrap();
        let values = rt
            .evaluate(
                r#"(function(){
                    const box = document.getElementById('box');
                    const first = box.getBoundingClientRect().width;
                    const deadline = Date.now() + 120;
                    while (Date.now() < deadline) {}
                    return [first, box.getBoundingClientRect().width];
                })()"#,
            )
            .unwrap();
        let widths = values.as_array().unwrap();
        assert_eq!(
            widths[0].as_f64(),
            widths[1].as_f64(),
            "forced layout reads in one long task must share one animation frame"
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn timer_callback_starts_a_fresh_lazy_animation_sample() {
        let mut rt = animation_epoch_runtime();
        rt.execute_script(
            "timer-animation-sample",
            r#"
                var box=document.createElement('div');
                box.id='box';box.className='anim';document.body.appendChild(box);
                globalThis.__beforeTimerWidth=box.getBoundingClientRect().width;
                setTimeout(() => {
                    globalThis.__afterTimerWidth=box.getBoundingClientRect().width;
                }, 100);
            "#,
        )
        .unwrap();
        rt.run_event_loop_bounded(250).await.unwrap();
        let values = rt
            .evaluate("[globalThis.__beforeTimerWidth, globalThis.__afterTimerWidth]")
            .unwrap();
        let widths = values.as_array().unwrap();
        let before = widths[0].as_f64().unwrap();
        let after = widths[1].as_f64().unwrap();
        assert!(after >= before + 7.0, "before={before}, after={after}");
    }
