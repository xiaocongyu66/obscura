#![allow(unused_imports)]
#![allow(unused_variables)]
#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use deno_core::{JsRuntime, RuntimeOptions};
use obscura_dom::{DomTree, NodeId};

use crate::import_map::ImportMap;
use crate::module_loader::{ModuleLoadActivity, ObscuraModuleLoader};
#[cfg(all(test, feature = "render"))]
use crate::ops::ensure_prepared_render;
use crate::ops::{build_extension, node_is_script, ObscuraState, StoredNetworkResponseBody};
#[cfg(feature = "render")]
use crate::ops::{
    begin_animation_task, clamp_scroll_offset, document_base_url, ensure_resolved_scroll,
};

use super::*;

impl ObscuraJsRuntime {


    pub(crate) fn execute_runtime_script(
        &mut self,
        name: &'static str,
        source: String,
    ) -> Result<deno_core::v8::Global<deno_core::v8::Value>, String> {
        let result = self
            .runtime
            .execute_script(name, source)
            .map_err(|error| error.to_string());
        self.finish_heap_checked(result)
    }

    /// Parse and merge an inline document import map. Rules which would alter
    /// already-observed module resolutions are discarded while unrelated new
    /// rules remain available, matching Chromium's multiple-map model.
    pub fn add_import_map(&self, source: &str, base_url: &str) -> Result<(), String> {
        let map = ImportMap::parse(source, base_url)?;
        self.import_map
            .try_borrow_mut()
            .map_err(|_| "Import map is already borrowed".to_string())?
            .merge(map);
        Ok(())
    }

    pub fn set_cookie_jar(&self, jar: std::sync::Arc<obscura_net::CookieJar>) {
        self.state.borrow_mut().cookie_jar = Some(jar);
    }

    pub fn set_http_client(&self, client: std::sync::Arc<obscura_net::ObscuraHttpClient>) {
        self.state.borrow_mut().http_client = Some(client);
    }

    /// Install the owning page's passive on_request/on_response callback
    /// registry so scripted fetch()/XHR observation is page-scoped (issue #408).
    pub fn set_callbacks(&self, callbacks: std::sync::Arc<obscura_net::CallbackRegistry>) {
        self.state.borrow_mut().callbacks = Some(callbacks);
    }

    /// Install the stealth (wreq) HTTP client so scripted fetch()/XHR is routed
    /// through it in stealth mode (see op_fetch_url / stealth_fetch_all).
    #[cfg(feature = "stealth")]
    pub fn set_stealth_client(&self, client: std::sync::Arc<obscura_net::StealthHttpClient>) {
        self.state.borrow_mut().stealth_client = Some(client);
    }

    pub fn set_dom(&self, dom: DomTree) {
        let mut gs = self.state.borrow_mut();
        gs.dom = Some(dom);
        gs.document_generation = gs.document_generation.wrapping_add(1);
        gs.activity_generation = 0;
        gs.page_in_flight = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        gs.already_started_scripts.borrow_mut().clear();
        // A new document owns a fresh retained scene and resource cache.
        #[cfg(feature = "render")]
        {
            gs.prepared_render = None;
            gs.animation_sample = obscura_render::AnimationSample::default();
            gs.animation_timeline = obscura_render::AnimationTimelineState::default();
            gs.animation_timeline_origin = std::time::Instant::now();
            gs.animation_task_generation = 0;
            gs.animation_sampled_task_generation = 0;
            gs.pending_style_mutations.clear();
            gs.render_resources = obscura_render::RenderResourceCache::default();
            gs.render_image_in_flight.clear();
            gs.stylesheet_cache = obscura_render::StylesheetCache::default();
            gs.dynamic_fonts.clear();
            gs.canvas_surfaces.clear();
            gs.scroll_offset = (0.0, 0.0);
            gs.element_scroll_offsets.clear();
            gs.scroll_generation = 0;
            gs.resolved_scroll = None;
        }
    }

    pub fn set_url(&self, url: &str) {
        let mut state = self.state.borrow_mut();
        if state.url != url {
            state.url = url.to_string();
            #[cfg(feature = "render")]
            {
                // Relative resources use the document URL when no <base> is
                // present. Keep already-fetched absolute bytes, but rebuild
                // candidate selection/layout against the new base.
                state.prepared_render = None;
                state.pending_style_mutations.clear();
                state.resolved_scroll = None;
            }
        }
    }

    /// Set the document's character encoding (WHATWG canonical name). Backs
    /// `document.characterSet` and the `<a>`/`<area>` URL query encoding
    /// override for legacy-charset documents.
    pub fn set_encoding(&self, encoding: &str) {
        self.state.borrow_mut().encoding = encoding.to_string();
    }

    pub fn set_title(&self, title: &str) {
        self.state.borrow_mut().title = title.to_string();
    }

    /// Set the source document URL exposed as `document.referrer`. Navigation
    /// owns this value; it is not derived from the current URL because direct
    /// navigations and document-initiated navigations have different
    /// referrer semantics.
    pub fn set_referrer(&self, referrer: &str) {
        self.state.borrow_mut().referrer = referrer.to_string();
    }

    pub fn set_blocked_urls(&self, patterns: Vec<String>) {
        self.state.borrow_mut().blocked_urls = patterns;
    }

    pub fn take_pending_navigation(&self) -> Option<(String, String, String)> {
        self.state.borrow_mut().pending_navigation.take()
    }

    pub fn take_pending_binding_calls(&self) -> Vec<(String, String)> {
        std::mem::take(&mut self.state.borrow_mut().pending_binding_calls)
    }

    pub fn get_network_response_body(&self, request_id: &str) -> Option<StoredNetworkResponseBody> {
        self.state
            .borrow()
            .network_response_bodies
            .get(request_id)
            .cloned()
    }

    pub fn clear_network_response_bodies(&self) {
        let mut state = self.state.borrow_mut();
        state.network_response_bodies.clear();
        state.network_response_body_order.clear();
    }

    /// Wire up the interception channel without enabling interception.
    /// Use set_intercept_enabled separately. The two were entangled before
    /// and every navigation auto-enabled interception, which made
    /// `fetch()` from page JS hang forever waiting for a CDP client to
    /// answer Fetch.requestPaused events that the client never asked for.
    pub fn set_intercept_tx(
        &self,
        tx: tokio::sync::mpsc::UnboundedSender<crate::ops::InterceptedRequest>,
    ) {
        let mut state = self.state.borrow_mut();
        state.intercept_tx = Some(tx);
    }

    pub fn set_intercept_enabled(&self, enabled: bool) {
        let mut state = self.state.borrow_mut();
        state.intercept_enabled = enabled;
    }

    pub fn set_user_agent(&mut self, ua: &str) {
        let escaped = ua.replace('\\', "\\\\").replace('\'', "\\'");
        let _ = self.execute_runtime_script(
            "<set-ua>",
            format!("globalThis.__obscura_ua = '{}';", escaped),
        );
    }

    pub fn set_platform(&mut self, platform: &str, ua_platform: &str, ua_platform_version: &str) {
        let p = platform.replace('\'', "\\'");
        let uap = ua_platform.replace('\'', "\\'");
        let uapv = ua_platform_version.replace('\'', "\\'");
        let _ = self.execute_runtime_script(
            "<set-platform>",
            format!(
                "globalThis.__obscura_platform='{}';globalThis.__obscura_ua_platform='{}';globalThis.__obscura_ua_platform_version='{}';",
                p, uap, uapv
            ),
        );
    }

    /// Pins navigator.language(s) to the exit-IP region (mirrors the TZ pin
    /// in the CLI): a Windows UA browsing from an Asia/Shanghai exit while
    /// claiming en-US is a cross-surface mismatch CF fingerprints.
    pub fn set_locale(&mut self, locale: &str, locales: &[&str]) {
        let l = locale.replace('\'', "\\'");
        let arr = locales
            .iter()
            .map(|s| format!("'{}'", s.replace('\'', "\\'")))
            .collect::<Vec<_>>()
            .join(",");
        let _ = self.execute_runtime_script(
            "<set-locale>",
            format!(
                "globalThis.__obscura_locale='{}';globalThis.__obscura_locales=[{}];",
                l, arr
            ),
        );
    }

    pub fn set_stealth(&mut self, enabled: bool) {
        let _ = self.execute_runtime_script(
            "<set-stealth>",
            format!("globalThis.__obscura_stealth = {};", enabled),
        );
    }

    /// Set the CSS viewport exposed to page JavaScript. This must run before
    /// `run_page_init` for navigation-time responsive code; it may also be
    /// called later by CDP emulation to update the live window surfaces.
    pub fn set_viewport(&mut self, width: f64, height: f64) {
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return;
        }
        #[cfg(feature = "render")]
        {
            let mut state = self.state.borrow_mut();
            let viewport = (width as f32, height as f32);
            if state.viewport != viewport {
                state.viewport = viewport;
                state.prepared_render = None;
                state.pending_style_mutations.clear();
                state.resolved_scroll = None;
            }
        }
        let _ = self.execute_runtime_script(
            "<set-viewport>",
            format!(
                "globalThis.__obscura_viewport_w={width};\
                 globalThis.__obscura_viewport_h={height};\
                 globalThis.innerWidth={width};globalThis.innerHeight={height};\
                 if(globalThis.visualViewport){{\
                   globalThis.visualViewport.width={width};\
                   globalThis.visualViewport.height={height};\
                 }}\
                 if(typeof globalThis.__obscura_recompute_intersections==='function'){{\
                   globalThis.__obscura_recompute_intersections();\
                 }}\
                 if(typeof globalThis.__obscura_recompute_resizes==='function'){{\
                   globalThis.__obscura_recompute_resizes();\
                 }}",
            ),
        );
    }

    /// Override the physical screen metrics exposed to page JavaScript.
    /// Unlike the CSS viewport, CDP only changes these when both optional
    /// screen dimensions are supplied. Passing `None` restores the native
    /// screen surface while keeping the viewport override intact.
    pub fn set_screen_size_override(&mut self, size: Option<(f64, f64)>, emulated: bool) {
        let script = match size {
            Some((width, height))
                if width.is_finite()
                    && height.is_finite()
                    && width > 0.0
                    && height > 0.0 =>
            {
                format!(
                    "globalThis.__obscura_set_screen_override({width},{height},{emulated});"
                )
            }
            _ => format!(
                "globalThis.__obscura_set_screen_override(null,null,{emulated});"
            ),
        };
        let _ = self.execute_runtime_script("<set-screen-size>", script);
    }

    /// Current clamped root scroll offset shared by CSSOM geometry and paint.
    #[cfg(feature = "render")]
    pub fn scroll_offset(&self) -> (f32, f32) {
        let mut state = self.state.borrow_mut();
        let requested = state.scroll_offset;
        clamp_scroll_offset(&mut state, requested)
    }

    /// Select the document-timeline instant used by the next render flush.
    /// Returns false for invalid times and preserves the current sample.
    #[cfg(feature = "render")]
    pub fn set_animation_sample_time(
        &self,
        sample: obscura_render::AnimationSampleTime,
    ) -> bool {
        self.set_animation_sample(obscura_render::AnimationSample::document(
            sample.milliseconds,
        ))
    }

    #[cfg(feature = "render")]
    pub fn set_animation_sample(&self, sample: obscura_render::AnimationSample) -> bool {
        if !sample.time.milliseconds.is_finite() || sample.time.milliseconds < 0.0 {
            return false;
        }
        let mut state = self.state.borrow_mut();
        if state.animation_sample != sample {
            let forward_document_sample =
                sample.mode == obscura_render::AnimationSampleMode::DocumentTime
                && state.animation_sample.mode == obscura_render::AnimationSampleMode::DocumentTime
                && sample.time.milliseconds > state.animation_sample.time.milliseconds;
            if forward_document_sample
                && state.pending_style_mutations.is_empty()
                && state.prepared_render.as_mut().is_some_and(|prepared| {
                    prepared.advance_inactive_animation_sample_time(sample.time)
                })
            {
                state.animation_sample = sample;
                return true;
            }
            state.animation_sample = sample;
            if !forward_document_sample {
                state.prepared_render = None;
                state.pending_style_mutations.clear();
            }
            state.resolved_scroll = None;
        }
        true
    }

    /// Select the CSS media type for the next synchronous render flush.
    /// Changing media invalidates geometry and the compiled stylesheet key but
    /// leaves the live DOM, scroll offsets, and resource bytes untouched.
    #[cfg(feature = "render")]
    pub fn set_render_media(
        &self,
        media: obscura_render::CssMediaType,
    ) -> obscura_render::CssMediaType {
        let mut state = self.state.borrow_mut();
        let previous = state.render_media;
        if previous != media {
            state.render_media = media;
            state.prepared_render = None;
            state.resolved_scroll = None;
        }
        previous
    }

    #[cfg(feature = "render")]
    pub fn animation_sample_time(&self) -> obscura_render::AnimationSampleTime {
        self.state.borrow().animation_sample.time
    }

    #[cfg(feature = "render")]
    pub fn live_animation_sample(&self) -> obscura_render::AnimationSample {
        let state = self.state.borrow();
        obscura_render::AnimationSample::document(
            (state.animation_timeline_origin.elapsed().as_secs_f64() * 1_000.0)
                .min(f64::from(f32::MAX)) as f32,
        )
    }

    #[cfg(feature = "render")]
    pub fn reset_animation_timeline(&self) {
        let mut state = self.state.borrow_mut();
        state.animation_timeline_origin = std::time::Instant::now();
        state.animation_timeline = obscura_render::AnimationTimelineState::default();
        state.animation_sample = obscura_render::AnimationSample::default();
        state.prepared_render = None;
        state.pending_style_mutations.clear();
        state.resolved_scroll = None;
    }

    /// Read animation damage from the last prepared frame without causing a
    /// style/layout flush. Screencast scheduling uses this to avoid rasterizing
    /// static pages on every compositor tick.
    #[cfg(feature = "render")]
    pub fn prepared_has_active_css_animations(&self) -> bool {
        self.state
            .borrow()
            .prepared_render
            .as_ref()
            .is_some_and(|prepared| prepared.has_active_css_animations())
    }

    /// Capture the live render viewport from the same prepared layout used by
    /// CSSOM geometry. A mismatched ad-hoc viewport/base returns `None` so the
    /// browser layer can retain its compatibility one-shot path.
    #[cfg(feature = "render")]
    pub fn screenshot_prepared(
        &self,
        viewport: (f32, f32),
        base_url: Option<&str>,
    ) -> Option<Vec<u8>> {
        self.screenshot_prepared_with_surface_color(
            viewport,
            base_url,
            [255, 255, 255, 255],
        )
    }

    #[cfg(feature = "render")]
    pub fn screenshot_prepared_with_surface_color(
        &self,
        viewport: (f32, f32),
        base_url: Option<&str>,
        surface_color: [u8; 4],
    ) -> Option<Vec<u8>> {
        let mut state = self.state.borrow_mut();
        let effective_base = document_base_url(&state);
        if viewport != state.viewport || base_url != effective_base.as_deref() {
            return None;
        }
        with_sync_render_loading_disabled(&mut state, |state| {
            ensure_resolved_scroll(state)?;
            let ObscuraState {
                dom,
                prepared_render,
                render_resources,
                resolved_scroll,
                canvas_surfaces,
                ..
            } = state;
            let (_, scroll) = resolved_scroll.as_ref()?;
            let canvas_surfaces = RuntimeCanvasSurfaceSource(canvas_surfaces);
            obscura_render::screenshot_prepared_with_scroll_and_surface_color_and_canvas_surfaces(
                dom.as_ref()?,
                prepared_render.as_mut()?,
                render_resources,
                scroll,
                surface_color,
                &canvas_surfaces,
            )
        })
    }

    /// Capture a document-space rectangle without changing the live viewport,
    /// root scroll, element scroll offsets, or retained layout. The current
    /// resolved scroll snapshot supplies fixed/sticky and nested-scroll state.
    #[cfg(feature = "render")]
    pub fn screenshot_prepared_region(
        &self,
        region: obscura_render::CaptureRegion,
    ) -> Result<Vec<u8>, obscura_render::CaptureError> {
        self.screenshot_prepared_region_with_surface_color(region, [255, 255, 255, 255])
    }

    #[cfg(feature = "render")]
    pub fn screenshot_prepared_region_with_surface_color(
        &self,
        region: obscura_render::CaptureRegion,
        surface_color: [u8; 4],
    ) -> Result<Vec<u8>, obscura_render::CaptureError> {
        let mut state = self.state.borrow_mut();
        with_sync_render_loading_disabled(&mut state, |state| {
            ensure_resolved_scroll(state).ok_or(obscura_render::CaptureError::PaintFailed)?;
            let ObscuraState {
                dom,
                prepared_render,
                render_resources,
                resolved_scroll,
                canvas_surfaces,
                ..
            } = state;
            let (_, scroll) = resolved_scroll
                .as_ref()
                .ok_or(obscura_render::CaptureError::PaintFailed)?;
            let canvas_surfaces = RuntimeCanvasSurfaceSource(canvas_surfaces);
            obscura_render::screenshot_prepared_region_with_scroll_and_surface_color_and_canvas_surfaces(
                dom.as_ref()
                    .ok_or(obscura_render::CaptureError::PaintFailed)?,
                prepared_render
                    .as_mut()
                    .ok_or(obscura_render::CaptureError::PaintFailed)?,
                render_resources,
                scroll,
                region,
                surface_color,
                &canvas_surfaces,
            )
        })
    }

    /// Capture a document-space rectangle with the PDF print-background
    /// policy without mutating the page DOM or retained geometry.
    #[cfg(feature = "render")]
    pub fn screenshot_prepared_region_with_backgrounds(
        &self,
        region: obscura_render::CaptureRegion,
        paint_backgrounds: bool,
    ) -> Result<Vec<u8>, obscura_render::CaptureError> {
        let mut state = self.state.borrow_mut();
        with_sync_render_loading_disabled(&mut state, |state| {
            ensure_resolved_scroll(state).ok_or(obscura_render::CaptureError::PaintFailed)?;
            let ObscuraState {
                dom,
                prepared_render,
                render_resources,
                resolved_scroll,
                canvas_surfaces,
                ..
            } = state;
            let (_, scroll) = resolved_scroll
                .as_ref()
                .ok_or(obscura_render::CaptureError::PaintFailed)?;
            let canvas_surfaces = RuntimeCanvasSurfaceSource(canvas_surfaces);
            obscura_render::screenshot_prepared_region_with_scroll_and_backgrounds_and_canvas_surfaces(
                dom.as_ref()
                    .ok_or(obscura_render::CaptureError::PaintFailed)?,
                prepared_render
                    .as_mut()
                    .ok_or(obscura_render::CaptureError::PaintFailed)?,
                render_resources,
                scroll,
                region,
                paint_backgrounds,
                &canvas_surfaces,
            )
        })
    }

    /// Capture one immutable document slice as if its origin were the root
    /// scroll position of a virtual viewport. This leaves the live page scroll
    /// untouched while giving fixed and sticky descendants page-local paint
    /// geometry, which paginated raster PDF export requires.
    #[cfg(feature = "render")]
    pub fn screenshot_prepared_region_at_scroll_with_backgrounds(
        &self,
        region: obscura_render::CaptureRegion,
        root_scroll: (f32, f32),
        paint_backgrounds: bool,
    ) -> Result<Vec<u8>, obscura_render::CaptureError> {
        let mut state = self.state.borrow_mut();
        with_sync_render_loading_disabled(&mut state, |state| {
            ensure_resolved_scroll(state).ok_or(obscura_render::CaptureError::PaintFailed)?;
            let ObscuraState {
                dom,
                prepared_render,
                render_resources,
                element_scroll_offsets,
                canvas_surfaces,
                ..
            } = state;
            let dom = dom
                .as_ref()
                .ok_or(obscura_render::CaptureError::PaintFailed)?;
            let scroll = prepared_render
                .as_ref()
                .ok_or(obscura_render::CaptureError::PaintFailed)?
                .resolve_scroll_state_for_viewport(
                    dom,
                    root_scroll,
                    element_scroll_offsets,
                    (region.width, region.height),
                );
            let canvas_surfaces = RuntimeCanvasSurfaceSource(canvas_surfaces);
            obscura_render::screenshot_prepared_region_with_scroll_and_backgrounds_and_canvas_surfaces(
                dom,
                prepared_render
                    .as_mut()
                    .ok_or(obscura_render::CaptureError::PaintFailed)?,
                render_resources,
                &scroll,
                region,
                paint_backgrounds,
                &canvas_surfaces,
            )
        })
    }

    /// Return the retained layout's scrollable document size without changing
    /// the live viewport or scroll position. PDF/full-document consumers use
    /// this to paginate document-space captures from the same geometry.
    #[cfg(feature = "render")]
    pub fn prepared_content_size(&self) -> Option<(f32, f32)> {
        let mut state = self.state.borrow_mut();
        with_sync_render_loading_disabled(&mut state, |state| {
            ensure_resolved_scroll(state)?;
            state
                .prepared_render
                .as_ref()
                .map(|render| render.content_size())
        })
    }

    /// Return the exact responsive candidates selected for live `<img>`
    /// elements and `<video poster>` resources without loading them. The
    /// browser layer can then fetch them concurrently through the page-owned
    /// transport before synchronous layout or paint observes the cache.
    #[cfg(feature = "render")]
    pub fn pending_render_image_urls(&self) -> Vec<(String, crate::ops::ImageRequestProfile)> {
        let state = self.state.borrow();
        let base_url = document_base_url(&state);
        let Some(dom) = state.dom.as_ref() else {
            return Vec::new();
        };
        let mut urls = Vec::new();
        for id in dom.descendants(dom.document()) {
            let Some(node) = dom.get_node(id) else {
                continue;
            };
            let Some(element) = node.as_element() else {
                continue;
            };
            let candidate = match element.local.as_ref() {
                "img" => state
                    .render_resources
                    .cached_image_element_metadata(dom, id, state.viewport, base_url.as_deref())
                    .map(|(url, _, known, _)| {
                        let profile = match node
                            .get_attribute("crossorigin")
                            .map(|value| value.trim().to_ascii_lowercase())
                            .as_deref()
                        {
                            Some("use-credentials") => {
                                crate::ops::ImageRequestProfile::CorsInclude
                            }
                            Some(_) => crate::ops::ImageRequestProfile::CorsSameOrigin,
                            None => crate::ops::ImageRequestProfile::NoCorsInclude,
                        };
                        (url, profile, known)
                    }),
                "video" => state
                    .render_resources
                    .cached_video_poster_metadata(dom, id, base_url.as_deref())
                    .map(|(url, profile, known, _)| (url, profile, known)),
                _ => None,
            };
            let Some((url, profile, known)) = candidate else {
                continue;
            };
            if !known && !url.starts_with("data:") {
                urls.push((url, profile));
            }
        }
        urls.sort();
        urls.dedup();
        urls
    }

    /// Insert one page-transport resource outcome into the retained renderer
    /// cache. Successful image/font bytes queue one resource-dependent layout
    /// refresh while preserving computed styles and any DOM damage already
    /// queued. A negative outcome cannot change geometry and preserves the
    /// retained layout/scroll.
    #[cfg(feature = "render")]
    pub fn seed_render_resource(&mut self, url: String, bytes: Option<Vec<u8>>) {
        let mut state = self.state.borrow_mut();
        match bytes {
            Some(bytes) => {
                state.render_resources.seed(url, bytes);
                crate::ops::invalidate_render_resource_geometry(&mut state);
            }
            None => state.render_resources.seed_missing(url),
        }
    }

    #[cfg(feature = "render")]
    pub fn seed_render_image_resource(
        &mut self,
        url: String,
        profile: crate::ops::ImageRequestProfile,
        bytes: Option<Vec<u8>>,
    ) {
        let mut state = self.state.borrow_mut();
        match bytes {
            Some(bytes) if obscura_render::image_intrinsic_dimensions(&bytes).is_some() => {
                let needs_geometry = match (&state.prepared_render, &state.dom) {
                    (Some(prepared), Some(dom)) => {
                        prepared.image_resource_needs_geometry(dom, &url, profile)
                    }
                    _ => true,
                };
                state.render_resources.seed_image(url, profile, bytes);
                state.activity_generation = state.activity_generation.wrapping_add(1);
                if needs_geometry {
                    crate::ops::invalidate_render_resource_geometry(&mut state);
                }
            }
            _ => state.render_resources.seed_image_missing(url, profile),
        }
    }

    #[cfg(feature = "render")]
    pub fn render_resource_is_known(&self, url: &str) -> bool {
        self.state.borrow().render_resources.has_live_outcome(url)
    }

    #[cfg(feature = "render")]
    pub fn render_image_resource_is_known(
        &self,
        url: &str,
        profile: crate::ops::ImageRequestProfile,
    ) -> bool {
        self.state
            .borrow()
            .render_resources
            .has_live_image_outcome(url, profile)
    }

    /// Run __obscura_init() after all per-page properties (UA, platform, stealth, etc.)
    /// have been set. Must be called once per page setup, after all set_* methods.
    pub fn run_page_init(&mut self) {
        let _ = self.execute_runtime_script(
            "<obscura:page-init>",
            "globalThis.__obscura_init();".to_string(),
        );
    }

    /// Override the coordinates the navigator.geolocation shim reports. The
    /// values are injected as numeric globals the bootstrap reads; when unset it
    /// keeps the built-in default. Callers validate the range before calling.
    pub fn set_geolocation(&mut self, latitude: f64, longitude: f64) {
        let _ = self.execute_runtime_script(
            "<set-geo>",
            format!(
                "globalThis.__obscura_geo_lat={};globalThis.__obscura_geo_lon={};",
                latitude, longitude
            ),
        );
    }

    pub(crate) fn execute_classic_script(&mut self, name: &str, source: &str) -> Result<(), String> {
        self.begin_javascript_task();
        // JsRuntime::execute_script in deno_core 0.350 restricts `name` to a
        // &'static str. Browser script URLs are runtime data, and V8 uses this
        // origin as import()'s referrer, so compile in the runtime's main
        // context directly instead of substituting the fixed "<script>" name.
        let result = (|| {
            let scope = &mut self.runtime.handle_scope();
            let source = deno_core::v8::String::new(scope, source)
                .ok_or_else(|| "JS error: source allocation failed".to_string())?;
            let name = deno_core::v8::String::new(scope, name)
                .ok_or_else(|| "JS error: script URL allocation failed".to_string())?;
            let origin = deno_core::v8::ScriptOrigin::new(
                scope,
                name.into(),
                0,
                0,
                false,
                0,
                None,
                false,
                false,
                false,
                None,
            );
            let scope = &mut deno_core::v8::TryCatch::new(scope);
            let script = deno_core::v8::Script::compile(scope, source, Some(&origin));
            let Some(script) = script else {
                if scope.is_execution_terminating() {
                    scope.cancel_terminate_execution();
                    return Err("JS error: Uncaught Error: execution terminated".to_string());
                }
                return match scope.exception() {
                    Some(exception) => {
                        let error = deno_core::error::JsError::from_v8_exception(scope, exception);
                        Err(format!("JS error: {error}"))
                    }
                    None => {
                        Err("JS error: script compilation failed without an exception".to_string())
                    }
                };
            };
            if script.run(scope).is_none() {
                if scope.is_execution_terminating() {
                    scope.cancel_terminate_execution();
                    return Err("JS error: Uncaught Error: execution terminated".to_string());
                }
                return match scope.exception() {
                    Some(exception) => {
                        let error = deno_core::error::JsError::from_v8_exception(scope, exception);
                        Err(format!("JS error: {error}"))
                    }
                    None => {
                        Err("JS error: script execution failed without an exception".to_string())
                    }
                };
            }
            Ok(())
        })();
        self.finish_heap_checked(result)
    }

    pub fn execute_script(&mut self, name: &str, source: &str) -> Result<(), String> {
        self.execute_classic_script(name, source)
    }

    pub fn execute_script_guarded(&mut self, name: &str, source: &str) -> Result<(), String> {
        if source.len() < 10_000 {
            self.execute_script(name, source)
        } else {
            self.execute_script_with_timeout(name, source, std::time::Duration::from_secs(5))
        }
    }

    pub fn execute_script_with_timeout(
        &mut self,
        name: &str,
        source: &str,
        timeout: std::time::Duration,
    ) -> Result<(), String> {
        if timeout.is_zero() {
            return self.execute_classic_script(name, source);
        }

        let isolate_handle = self.runtime.v8_isolate().thread_safe_handle();

        let pair = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let pair_clone = pair.clone();

        let watchdog_name = name.to_string();
        let watchdog = std::thread::spawn(move || {
            let (lock, cvar) = &*pair_clone;
            let mut cancelled = lock.lock().unwrap();
            let deadline = std::time::Instant::now() + timeout;

            loop {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    tracing::warn!(
                        "execute_script_with_timeout: script '{}' exceeded {:?}, terminating",
                        watchdog_name,
                        timeout
                    );
                    isolate_handle.terminate_execution();
                    return;
                }

                let result = cvar.wait_timeout(cancelled, remaining).unwrap();
                cancelled = result.0;
                if *cancelled {
                    return;
                }
            }
        });

        let result = self.execute_classic_script(name, source);

        {
            let (lock, cvar) = &*pair;
            let mut cancelled = lock.lock().unwrap();
            *cancelled = true;
            cvar.notify_one();
        }
        let _ = watchdog.join();

        match result {
            Ok(()) => Ok(()),
            Err(msg) => {
                if msg.contains("Uncaught Error: execution terminated") {
                    tracing::warn!("Script killed after {}s timeout", timeout.as_secs());
                    Ok(())
                } else {
                    Err(msg)
                }
            }
        }
    }

    pub async fn run_event_loop(&mut self) -> Result<(), String> {
        self.begin_javascript_task();
        // A browser performs a microtask checkpoint at the end of each task.
        // deno_core's event loop may return immediately when no async op is
        // pending, leaving an already-resolved Promise continuation stranded
        // (document.fonts.load(...).then(...), framework post-render hooks,
        // and hydration follow-ups all rely on this boundary).
        self.runtime.v8_isolate().perform_microtask_checkpoint();
        let result = self
            .runtime
            .run_event_loop(deno_core::PollEventLoopOptions::default())
            .await
            .map_err(|e| format!("Event loop error: {}", e));
        self.runtime.v8_isolate().perform_microtask_checkpoint();
        self.finish_heap_checked(result)
    }

    /// Whether the serialized dynamic-script queue is still fetching or
    /// evaluating a script. The queue stays private to the bootstrap closure;
    /// Rust reads it through a hidden status function so page declarations
    /// cannot collide with or overwrite the queue itself.
    pub fn has_pending_dynamic_scripts(&mut self) -> bool {
        let pending_dom_script = self
            .evaluate("globalThis.__obscura_hasPendingDynamicScripts?.() === true")
            .ok()
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        // A short tail bridges the parser/evaluator hand-off between one
        // fetched module and the next dependency. It is below the lifecycle's
        // existing 500ms fast-settle floor, so static entry graphs pay no new
        // latency while lazy import graphs remain observable. deno_core's
        // dynamic-module evaluation/TLA counters are private; the event-loop
        // pump itself remains responsible for that non-fetch portion.
        pending_dom_script
            || self
                .module_load_activity
                .is_pending_or_recent(std::time::Duration::from_millis(100))
    }

    /// Whether a connected dynamic script prepared before the document load
    /// event still has fetch/evaluation/load-or-error work outstanding.
    ///
    /// This intentionally excludes `import()` and scripts created by a load
    /// handler. Those are ordinary post-load enhancement work and should only
    /// be driven when an automation caller explicitly asks the page to settle.
    pub fn has_pending_load_delaying_scripts(&mut self) -> bool {
        self.evaluate("globalThis.__obscura_hasPendingLoadDelayingScripts?.() === true")
            .ok()
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
    }

    /// Generation of observable connected-document mutations. This excludes
    /// detached-tree construction and no-op writes, which cannot affect a
    /// screenshot or DOM dump.
    pub fn activity_generation(&self) -> u64 {
        self.state.borrow().activity_generation
    }

    pub(crate) fn has_pending_network_requests(&self) -> bool {
        let state = self.state.borrow();
        state
            .page_in_flight
            .load(std::sync::atomic::Ordering::Relaxed)
            > 0
    }

    pub(crate) fn next_pending_timeout_delay_ms(&mut self) -> Option<f64> {
        self.evaluate("globalThis.__obscura_nextPendingTimeoutDelay?.() ?? -1")
        .ok()
        .and_then(|value| value.as_f64())
        .filter(|delay| *delay >= 0.0)
    }

    /// Arm a hard wall-clock backstop on synchronous V8 work. A page stuck in a
    /// synchronous loop or a microtask storm pins the OS thread inside V8, so
    /// `tokio::time::timeout` (which can only cancel at await points) never
    /// fires. This spawns a watchdog thread that terminates the isolate once
    /// `budget` elapses, forcing V8 to throw an uncatchable error and hand
    /// control back. Always balance with [`Self::disarm_watchdog`].
    pub fn arm_watchdog(&mut self, budget: std::time::Duration) -> WatchdogToken {
        spawn_watchdog(self.runtime.v8_isolate().thread_safe_handle(), budget)
    }

    /// Stop a watchdog armed by [`Self::arm_watchdog`]. If it had already fired
    /// (terminated the isolate), clear V8's termination flag so the isolate is
    /// usable again, and return `true`.
    pub fn disarm_watchdog(&mut self, token: WatchdogToken) -> bool {
        let fired = token.stop();
        if fired {
            self.runtime.v8_isolate().cancel_terminate_execution();
            tracing::warn!("V8 watchdog fired: terminated a synchronous overrun");
        }
        fired
    }

    /// This runtime's V8 isolate handle (captured at construction, stable for
    /// the isolate's life). Lets the CDP dispatcher arm a per-command watchdog
    /// from `&self`.
    pub fn isolate_handle(&self) -> IsolateHandle {
        self.isolate_handle.clone()
    }

    /// Clear V8's termination flag after a watchdog armed externally (via the
    /// isolate handle) fired, so the isolate is usable for the next command.
    /// No-op when the isolate is not terminating.
    pub fn cancel_termination(&mut self) {
        self.runtime.v8_isolate().cancel_terminate_execution();
    }

}
