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
    pub(crate) fn prepared_render_shares_resource_geometry_with_cssom_and_screenshots() {
        let dom = parse_html(
            r#"<html style="margin:0"><head>
                <base href="/assets/">
            </head><body style="margin:0">
                <div id="frame" style="width:160px">
                    <img id="hero" src="hero.svg" style="display:block;width:100%;height:auto">
                </div>
                <div style="height:400px;background:#0000ff"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_url("http://example.test/docs/page");
        rt.set_viewport(200.0, 100.0);

        let loads = std::sync::Arc::new(std::sync::Mutex::new(0usize));
        let loader_loads = std::sync::Arc::clone(&loads);
        rt.state.borrow_mut().render_resources =
            obscura_render::RenderResourceCache::with_loader(move |url: &str| {
                assert_eq!(url, "http://example.test/assets/hero.svg");
                *loader_loads.lock().expect("loader count") += 1;
                Some(
                    br##"<svg xmlns="http://www.w3.org/2000/svg" width="400" height="100">
                        <rect width="400" height="100" fill="#ffff00"/>
                    </svg>"##
                        .to_vec(),
                )
            });
        rt.run_page_init();

        let before = rt
            .evaluate(
                r#"
                const hero = document.getElementById("hero");
                const rect = hero.getBoundingClientRect();
                return [rect.width, rect.height, document.documentElement.scrollHeight];
                "#,
            )
            .expect("initial geometry");
        let before = before.as_array().expect("initial tuple");
        assert_eq!(before[0].as_f64(), Some(160.0));
        assert_eq!(before[1].as_f64(), Some(40.0));
        let cssom_height = before[2].as_f64().expect("scroll height") as f32;
        let (prepared_address, prepared_height) = {
            let state = rt.state.borrow();
            let prepared = state.prepared_render.as_ref().expect("prepared by CSSOM");
            (
                prepared as *const obscura_render::PreparedRender as usize,
                prepared.content_size().1,
            )
        };
        assert_eq!(cssom_height, prepared_height);
        assert_eq!(*loads.lock().expect("prepare load count"), 1);

        let base_url = Some("http://example.test/assets/");
        let top = rt
            .screenshot_prepared((200.0, 100.0), base_url)
            .expect("top screenshot");
        rt.evaluate(
            "(function(){ window.scrollTo(0, document.documentElement.scrollHeight); return window.scrollY; })()",
        )
        .expect("scroll to bottom");
        let bottom = rt
            .screenshot_prepared((200.0, 100.0), base_url)
            .expect("bottom screenshot");
        let bottom_repeat = rt
            .screenshot_prepared((200.0, 100.0), base_url)
            .expect("repeated bottom screenshot");
        assert_ne!(top, bottom);
        assert_eq!(bottom, bottom_repeat);
        {
            let state = rt.state.borrow();
            let prepared = state
                .prepared_render
                .as_ref()
                .expect("retained prepared render");
            assert_eq!(
                prepared as *const obscura_render::PreparedRender as usize, prepared_address,
                "screenshots must consume the CSSOM-prepared layout"
            );
            assert_eq!(prepared.content_size().1, cssom_height);
        }
        assert_eq!(*loads.lock().expect("paint load count"), 1);

        let after = rt
            .evaluate(
                r#"
                const hero = document.getElementById("hero");
                document.getElementById("frame").setAttribute("style", "width:80px");
                const rect = hero.getBoundingClientRect();
                return [rect.width, rect.height, document.documentElement.scrollHeight];
                "#,
            )
            .expect("mutated geometry");
        let after = after.as_array().expect("mutated tuple");
        assert_eq!(after[0].as_f64(), Some(80.0));
        assert_eq!(after[1].as_f64(), Some(20.0));
        let mutated_height = after[2].as_f64().expect("mutated scroll height") as f32;
        assert_eq!(
            rt.state
                .borrow()
                .prepared_render
                .as_ref()
                .expect("rebuilt prepared render")
                .content_size()
                .1,
            mutated_height
        );
        rt.screenshot_prepared((200.0, 100.0), base_url)
            .expect("mutated screenshot");
        assert_eq!(
            *loads.lock().expect("mutation load count"),
            1,
            "relayout must retain successful resource bytes"
        );
    }

    #[cfg(feature = "render")]
    #[tokio::test(flavor = "current_thread")]
    pub(crate) async fn missing_render_resource_preserves_prepared_layout_and_scroll() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0;height:240px">
                <div style="height:240px;background:blue"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_url("http://example.test/page");
        rt.set_viewport(80.0, 60.0);
        rt.run_page_init();
        rt.evaluate("window.scrollTo(0, 40)")
            .expect("scroll fixture");
        let before_png = rt
            .screenshot_prepared((80.0, 60.0), Some("http://example.test/page"))
            .expect("prepare retained render");
        let (prepared_address, resolved_address, scroll_generation, root_offset) = {
            let state = rt.state.borrow();
            let prepared = state.prepared_render.as_ref().expect("prepared layout");
            let resolved = &state.resolved_scroll.as_ref().expect("resolved scroll").1;
            (
                prepared as *const obscura_render::PreparedRender as usize,
                resolved as *const obscura_render::ResolvedScrollState as usize,
                state.scroll_generation,
                resolved.root_offset(),
            )
        };

        let missing_url = "http://example.test/missing.svg".to_string();
        rt.seed_render_resource(missing_url.clone(), None);
        assert!(rt.render_resource_is_known(&missing_url));
        {
            let state = rt.state.borrow();
            let prepared = state.prepared_render.as_ref().expect("retained layout");
            let resolved = &state.resolved_scroll.as_ref().expect("retained scroll").1;
            assert_eq!(
                prepared as *const obscura_render::PreparedRender as usize, prepared_address,
                "negative cache entries cannot change intrinsic geometry"
            );
            assert_eq!(
                resolved as *const obscura_render::ResolvedScrollState as usize, resolved_address,
                "negative cache entries must retain resolved scrolling"
            );
            assert_eq!(state.scroll_generation, scroll_generation);
            assert_eq!(resolved.root_offset(), root_offset);
        }
        assert_eq!(
            rt.screenshot_prepared((80.0, 60.0), Some("http://example.test/page"))
                .expect("capture retained render"),
            before_png,
        );

        rt.seed_render_resource(
            "http://example.test/loaded.svg".to_string(),
            Some(br#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"/>"#.to_vec()),
        );
        let state = rt.state.borrow();
        let prepared = state.prepared_render.as_ref().expect("retained style graph");
        assert_eq!(
            prepared as *const obscura_render::PreparedRender as usize,
            prepared_address,
            "resource arrival waits for the next geometry flush"
        );
        assert_eq!(
            state.pending_style_mutations,
            vec![obscura_render::RetainedStyleMutation::Resource],
            "successful bytes queue one resource-dependent rebuild"
        );
        assert!(
            state.resolved_scroll.is_none(),
            "successful bytes invalidate scroll geometry"
        );
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn image_resource_arrival_retains_styles_and_rebuilds_intrinsic_geometry() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <img id="hero" src="http://example.test/late.png" style="display:block">
                <div id="after" style="height:10px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_url("http://example.test/page");
        rt.set_viewport(80.0, 60.0);
        rt.state.borrow_mut().render_resources =
            obscura_render::RenderResourceCache::with_loader(|_: &str| None);
        rt.run_page_init();

        let before = rt
            .evaluate("[hero.getBoundingClientRect().height, after.getBoundingClientRect().top]")
            .expect("geometry before image arrival");
        assert_eq!(before, serde_json::json!([0, 0]));
        let prepared_address = {
            let state = rt.state.borrow();
            state
                .prepared_render
                .as_ref()
                .expect("initial prepared render") as *const obscura_render::PreparedRender
                as usize
        };

        // Preserve already queued framework damage and coalesce repeated
        // notification of the same shared resource into one refresh marker.
        rt.evaluate("after.setAttribute('data-ready', 'true')")
            .expect("queued DOM mutation");
        let png = two_by_three_png();
        rt.seed_render_image_resource(
            "http://example.test/late.png".to_string(),
            crate::ops::ImageRequestProfile::NoCorsInclude,
            Some(png.clone()),
        );
        rt.seed_render_image_resource(
            "http://example.test/late.png".to_string(),
            crate::ops::ImageRequestProfile::NoCorsInclude,
            Some(png),
        );
        {
            let state = rt.state.borrow();
            assert_eq!(
                state
                    .prepared_render
                    .as_ref()
                    .expect("style graph remains available")
                    as *const obscura_render::PreparedRender as usize,
                prepared_address,
            );
            assert_eq!(
                state
                    .pending_style_mutations
                    .iter()
                    .filter(|mutation| matches!(mutation, obscura_render::RetainedStyleMutation::Resource))
                    .count(),
                1,
            );
            assert!(state.pending_style_mutations.iter().any(|mutation| matches!(
                mutation,
                obscura_render::RetainedStyleMutation::Attribute(_)
            )));
        }

        let after = rt
            .evaluate("[hero.getBoundingClientRect().height, after.getBoundingClientRect().top]")
            .expect("geometry after image arrival");
        assert_eq!(after, serde_json::json!([3, 3]));
        assert!(rt.state.borrow().pending_style_mutations.is_empty());
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn fixed_image_resource_arrival_repaints_without_rebuilding_geometry() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <img id="hero" src="http://example.test/fixed.png"
                     style="display:block;width:20px;height:10px">
                <div id="after" style="height:10px;background:blue"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_url("http://example.test/page");
        rt.set_viewport(80.0, 60.0);
        rt.state.borrow_mut().render_resources =
            obscura_render::RenderResourceCache::with_loader(|_: &str| None);
        rt.run_page_init();

        assert_eq!(
            rt.evaluate("[hero.offsetWidth, hero.offsetHeight, after.offsetTop]")
                .expect("fixed geometry"),
            serde_json::json!([20, 10, 10])
        );
        let before_png = rt
            .screenshot_prepared((80.0, 60.0), Some("http://example.test/page"))
            .expect("capture before image arrival");
        let (prepared_address, resolved_address, activity_before) = {
            let state = rt.state.borrow();
            (
                state.prepared_render.as_ref().unwrap() as *const _ as usize,
                &state.resolved_scroll.as_ref().unwrap().1 as *const _ as usize,
                state.activity_generation,
            )
        };

        rt.seed_render_image_resource(
            "http://example.test/fixed.png".to_string(),
            crate::ops::ImageRequestProfile::NoCorsInclude,
            Some(two_by_three_png()),
        );
        {
            let state = rt.state.borrow();
            assert_eq!(
                state.prepared_render.as_ref().unwrap() as *const _ as usize,
                prepared_address,
                "fixed replaced content must keep the prepared geometry"
            );
            assert_eq!(
                &state.resolved_scroll.as_ref().unwrap().1 as *const _ as usize,
                resolved_address,
                "paint-only resource damage must keep resolved scrolling"
            );
            assert!(state.pending_style_mutations.is_empty());
            assert!(state.activity_generation > activity_before);
        }
        assert_eq!(
            rt.evaluate("[hero.offsetWidth, hero.offsetHeight, after.offsetTop]")
                .expect("retained fixed geometry"),
            serde_json::json!([20, 10, 10])
        );
        let after_png = rt
            .screenshot_prepared((80.0, 60.0), Some("http://example.test/page"))
            .expect("capture after image arrival");
        assert_ne!(after_png, before_png, "new image pixels must reach paint");
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn fixed_flex_image_resource_arrival_still_rebuilds_geometry() {
        let dom = parse_html(
            r#"<html><body><div style="display:flex">
                <img id="hero" src="http://example.test/flex.png"
                     style="width:20px;height:10px">
            </div></body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_url("http://example.test/page");
        rt.state.borrow_mut().render_resources =
            obscura_render::RenderResourceCache::with_loader(|_: &str| None);
        rt.run_page_init();
        rt.evaluate("hero.getBoundingClientRect().width")
            .expect("prepare flex geometry");
        assert!(rt.state.borrow().resolved_scroll.is_some());

        rt.seed_render_image_resource(
            "http://example.test/flex.png".to_string(),
            crate::ops::ImageRequestProfile::NoCorsInclude,
            Some(two_by_three_png()),
        );
        let state = rt.state.borrow();
        assert_eq!(
            state.pending_style_mutations,
            vec![obscura_render::RetainedStyleMutation::Resource]
        );
        assert!(state.resolved_scroll.is_none());
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn fixed_css_content_image_arrival_still_rebuilds_intrinsic_geometry() {
        let dom = parse_html(
            r#"<html><body><img id="hero" src="fallback.png"
                style="display:block;width:20px;height:10px;content:url('http://example.test/content.png')">
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_url("http://example.test/page");
        rt.state.borrow_mut().render_resources =
            obscura_render::RenderResourceCache::with_loader(|_: &str| None);
        rt.run_page_init();
        rt.evaluate("hero.getBoundingClientRect().width")
            .expect("prepare CSS replaced content");
        assert!(rt.state.borrow().resolved_scroll.is_some());

        rt.seed_render_image_resource(
            "http://example.test/content.png".to_string(),
            crate::ops::ImageRequestProfile::NoCorsInclude,
            Some(two_by_three_png()),
        );
        let state = rt.state.borrow();
        assert_eq!(
            state.pending_style_mutations,
            vec![obscura_render::RetainedStyleMutation::Resource]
        );
        assert!(state.resolved_scroll.is_none());
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn document_region_capture_preserves_live_runtime_state_and_resource_cache() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0;height:260px">
                <div style="height:120px;background:red"></div>
                <img src="http://example.test/marker.svg"
                     style="display:block;width:20px;height:20px">
                <div style="height:120px;background:blue"></div>
                <div style="position:fixed;left:0;top:0;width:10px;height:10px;background:lime"></div>
            </body></html>"#,
        );
        let loads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let loader_loads = loads.clone();
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_url("http://example.test/page");
        rt.set_viewport(80.0, 60.0);
        rt.state.borrow_mut().render_resources =
            obscura_render::RenderResourceCache::with_loader(move |url: &str| {
                assert_eq!(url, "http://example.test/marker.svg");
                loader_loads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Some(
                    br##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20">
                        <rect width="20" height="20" fill="#ffff00"/>
                    </svg>"##
                        .to_vec(),
                )
            });
        rt.run_page_init();
        assert_eq!(
            rt.evaluate("(function(){ window.scrollTo(0, 50); return window.scrollY; })()")
                .expect("live scroll")
                .as_f64(),
            Some(50.0)
        );
        let live_before = rt
            .screenshot_prepared((80.0, 60.0), Some("http://example.test/page"))
            .expect("live screenshot");
        let (
            prepared_address,
            viewport,
            scroll_offset,
            scroll_generation,
            resolved_root,
            full_height,
        ) = {
            let state = rt.state.borrow();
            let prepared = state.prepared_render.as_ref().expect("prepared render");
            (
                prepared as *const obscura_render::PreparedRender as usize,
                state.viewport,
                state.scroll_offset,
                state.scroll_generation,
                state
                    .resolved_scroll
                    .as_ref()
                    .expect("resolved scroll")
                    .1
                    .root_offset(),
                prepared.content_size().1,
            )
        };
        assert_eq!(loads.load(std::sync::atomic::Ordering::SeqCst), 1);

        let region_png = rt
            .screenshot_prepared_region(obscura_render::CaptureRegion::new(
                0.0, 115.0, 80.0, 40.0, 1.5,
            ))
            .expect("offscreen scaled region");
        let full_png = rt
            .screenshot_prepared_region(obscura_render::CaptureRegion::new(
                0.0,
                0.0,
                80.0,
                full_height,
                1.0,
            ))
            .expect("full-content region");
        let png_size = |bytes: &[u8]| {
            assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
            (
                u32::from_be_bytes(bytes[16..20].try_into().expect("PNG width")),
                u32::from_be_bytes(bytes[20..24].try_into().expect("PNG height")),
            )
        };
        assert_eq!(png_size(&region_png), (120, 60));
        assert_eq!(png_size(&full_png), (80, full_height.ceil() as u32));

        let live_after = rt
            .screenshot_prepared((80.0, 60.0), Some("http://example.test/page"))
            .expect("unchanged live screenshot");
        assert_eq!(live_after, live_before);
        let state = rt.state.borrow();
        let prepared = state.prepared_render.as_ref().expect("retained render");
        assert_eq!(
            prepared as *const obscura_render::PreparedRender as usize,
            prepared_address
        );
        assert_eq!(state.viewport, viewport);
        assert_eq!(state.scroll_offset, scroll_offset);
        assert_eq!(state.scroll_generation, scroll_generation);
        assert_eq!(
            state
                .resolved_scroll
                .as_ref()
                .expect("retained resolved scroll")
                .1
                .root_offset(),
            resolved_root
        );
        assert_eq!(loads.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn script_registered_url_font_reaches_render_resource_collection() {
        let loads = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let loader_loads = std::sync::Arc::clone(&loads);
        let font = include_bytes!("../../obscura-render/assets/liberation-serif.ttf").to_vec();
        let mut rt = parser_image_runtime(
            r#"<html><body style="margin:0">
                <span id="sample" style="display:inline-block;width:max-content;
                    font-family:DynamicFixture;font-size:40px;white-space:nowrap">WWWWiiii</span>
            </body></html>"#,
            move |url: &str| {
                loader_loads
                    .lock()
                    .expect("font loads")
                    .push(url.to_string());
                (url == "http://example.com/fonts/dynamic.ttf").then(|| font.clone())
            },
        );
        rt.set_viewport(400.0, 100.0);
        let before = rt
            .evaluate("document.getElementById('sample').getBoundingClientRect().width")
            .unwrap()
            .as_f64()
            .expect("fallback width");
        assert!(loads.lock().expect("initial loads").is_empty());

        let registered = rt
            .evaluate(
                r#"(() => {
                    const face = new FontFace("DynamicFixture",
                        "url('../fonts/dynamic.ttf') format('truetype')",
                        { weight: "normal", style: "normal", unicodeRange: "U+20-7E" });
                    return [document.fonts.add(face) === document.fonts,
                        document.fonts.size, document.fonts.has(face)];
                })()"#,
            )
            .unwrap();
        assert_eq!(registered, serde_json::json!([true, 1, true]));
        {
            let state = rt.state.borrow();
            assert_eq!(state.dynamic_fonts.len(), 1);
            assert!(state.prepared_render.is_some());
            assert_eq!(
                state.pending_style_mutations,
                vec![obscura_render::RetainedStyleMutation::Resource],
                "font registry changes need reshaping and layout, not a fresh cascade"
            );
        }

        let after = rt
            .evaluate("document.getElementById('sample').getBoundingClientRect().width")
            .unwrap()
            .as_f64()
            .expect("dynamic font width");
        assert_ne!(
            before, after,
            "registered face must affect final text geometry"
        );
        assert_eq!(
            *loads.lock().expect("dynamic font loads"),
            vec!["http://example.com/fonts/dynamic.ttf".to_string()]
        );
        rt.screenshot_prepared((400.0, 100.0), Some("http://example.com/page/index.html"))
            .expect("dynamic font screenshot");
        assert_eq!(loads.lock().expect("repeated font loads").len(), 1);
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn rendered_layout_cache_is_invalidated_by_style_mutations() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="box" style="height:300px;width:40px"></div>
            </body></html>"#,
        );
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(dom);
        rt.set_viewport(200.0, 100.0);
        rt.run_page_init();

        let result = rt
            .evaluate(
                r#"
                const box = document.getElementById("box");
                const before = [
                    document.documentElement.scrollHeight,
                    box.getBoundingClientRect().height,
                ];
                box.setAttribute("style", "height:900px;width:80px");
                const after = [
                    document.documentElement.scrollHeight,
                    box.getBoundingClientRect().height,
                    box.getBoundingClientRect().width,
                ];
                return [before, after];
                "#,
            )
            .unwrap();
        let values = result.as_array().expect("result");
        let before = values[0].as_array().expect("before");
        let after = values[1].as_array().expect("after");
        assert!(after[0].as_f64().unwrap() > before[0].as_f64().unwrap());
        assert_eq!(before[1].as_f64(), Some(300.0));
        assert_eq!(after[1].as_f64(), Some(900.0));
        assert_eq!(after[2].as_f64(), Some(80.0));
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn element_text_content_replacement_recomputes_empty_selector() {
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(parse_html(
            r#"<style>#x { width: 10px; height: 5px } #x:empty { width: 30px }</style>
               <div id="x">text</div>"#,
        ));
        rt.run_page_init();

        assert_eq!(
            rt.evaluate(
                r#"(() => {
                    const x = document.getElementById("x");
                    const before = x.getBoundingClientRect().width;
                    x.textContent = "";
                    return [before, x.matches(":empty"), x.getBoundingClientRect().width];
                })()"#,
            )
            .unwrap(),
            serde_json::json!([10, true, 30])
        );
    }

    #[cfg(feature = "render")]
    #[test]
    pub(crate) fn prepared_render_survives_detached_no_op_and_same_viewport_updates() {
        let dom = parse_html(
            r#"<html style="margin:0"><body style="margin:0">
                <div id="box" class="box" style="height:30px;width:40px"></div>
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
        assert!(rt.state.borrow().prepared_render.is_some());

        // Modern frameworks build and decorate substantial detached trees.
        // None of this can affect the connected document's style or geometry.
        rt.evaluate(
            r#"
            const parent = document.createElement('section');
            const child = document.createElement('div');
            child.setAttribute('class', 'box');
            child.setAttribute('style', 'height:900px');
            parent.appendChild(child);
            child.setAttribute('data-state', 'ready');
            "#,
        )
        .unwrap();
        assert!(
            rt.state.borrow().prepared_render.is_some(),
            "detached subtree construction must retain connected layout"
        );

        // Attribute setters still fire their DOM/observer semantics when the
        // assigned value is identical, but layout is not dirtied.
        rt.evaluate(
            r#"
            const box = document.getElementById('box');
            box.setAttribute('class', 'box');
            box.removeAttribute('data-absent');
            "#,
        )
        .unwrap();
        assert!(
            rt.state.borrow().prepared_render.is_some(),
            "no-op connected attributes must retain prepared layout"
        );

        rt.set_viewport(200.0, 100.0);
        assert!(
            rt.state.borrow().prepared_render.is_some(),
            "reapplying the current viewport must not force layout"
        );

        rt.evaluate(
            "document.getElementById('box').setAttribute('style', 'height:60px;width:40px')",
        )
        .unwrap();
        {
            let state = rt.state.borrow();
            assert!(
                state.prepared_render.is_some(),
                "a retained inline-style change must keep the prior style maps until flush"
            );
            assert!(matches!(
                state.pending_style_mutations.as_slice(),
                [obscura_render::RetainedStyleMutation::Attribute(
                    obscura_render::AttributeStyleMutation { name, .. }
                )] if name == "style"
            ));
        }
        assert_eq!(
            rt.evaluate("document.getElementById('box').getBoundingClientRect().height")
                .unwrap()
                .as_f64(),
            Some(60.0)
        );
    }
