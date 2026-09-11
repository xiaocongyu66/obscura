//! obscura-embedder: drives the vendored Servo engine headlessly.
//!
//! Phase 2 of the Servo migration (docs/SERVO_MIGRATION.md): assemble a
//! SoftwareRenderingContext + WebView without any display, pump the event
//! loop ourselves, and expose load/screenshot so the CDP bridge can later
//! point at a live Servo kernel instead of the legacy obscura-js engine.

pub mod bridge;
pub mod fetch;
pub mod fingerprint;
mod user_agents;
pub mod cdp_server;
pub mod page_dumps;
pub mod tools;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use servo::{
    CookieSource, LoadStatus, RenderingContext, Servo, ServoBuilder, SoftwareRenderingContext,
    UserContentManager, WebView, WebViewBuilder, WebViewDelegate,
};
use serde_json::json;
use url::Url;

/// Fetch-domain interception state shared between the delegate (which sees
/// every web resource the page loads) and the CDP command loop (which owns
/// the client's continue/fail/fulfill decisions).
///
/// Invariants:
/// - only touched from the kernel thread (delegate callbacks and command
///   handling both run there), so plain Mutex suffices;
/// - a load removed from `pending` and dropped forwards the original
///   request (the responder's default is DoNotIntercept) — that is CDP
///   Fetch.continueRequest;
/// - intercepting then cancelling fails the load (Fetch.failRequest), and
///   intercepting with a response + body + finish fulfills it.
pub struct FetchState {
    pub enabled: bool,
    /// Substring URL filters (empty = intercept everything) — the shape the
    /// Go client and Puppeteer route() actually use.
    pub patterns: Vec<String>,
    pub pending: std::collections::HashMap<String, servo::WebResourceLoad>,
    pub next_id: u64,
}

impl FetchState {
    fn matches(&self, url: &str) -> bool {
        // Interception OFF never holds loads. When ON: empty patterns
        // intercept everything, otherwise substring-match any pattern.
        if !self.enabled {
            return false;
        }
        self.patterns.is_empty()
            || self.patterns.iter().any(|p| url.contains(p.as_str()))
    }
}

/// Delegate that records load-status transitions and broadcasts console
/// messages to CDP subscribers (Runtime.consoleAPICalled backing).
pub struct HeadlessDelegate {
    pub load_status: RefCell<Option<LoadStatus>>,
    pub console_tx: Option<std::sync::mpsc::Sender<String>>,
    /// Network.requestWillBeSent feed: one line per web resource the page
    /// starts loading ("method\u{1f}url").
    pub request_tx: Option<std::sync::mpsc::Sender<String>>,
    /// Page lifecycle feed: one line per event
    /// ("domContentEventFired\u{1f}" / "loadEventFired\u{1f}" /
    ///  "frameNavigated\u{1f}<url>" / "urlChanged\u{1f}<url>").
    pub lifecycle_tx: Option<std::sync::mpsc::Sender<String>>,
    /// Shared Fetch-domain state (enabled flag + pending intercepted loads).
    pub fetch_state: Arc<std::sync::Mutex<FetchState>>,
    /// Fetch/Network event feed ("requestPaused\u{1f}{json}" etc.).
    pub fetch_tx: Option<std::sync::mpsc::Sender<String>>,
    /// Flipped when the compositor reports a new frame is ready
    /// (notify_new_frame_ready); the pump waits on this.
    pub frame_ready: Arc<std::sync::atomic::AtomicBool>,
}

impl WebViewDelegate for HeadlessDelegate {
    fn show_console_message(
        &self,
        _webview: servo::WebView,
        level: servo::ConsoleLogLevel,
        message: String,
    ) {
        if let Some(tx) = &self.console_tx {
            let level_name = match level {
                servo::ConsoleLogLevel::Error => "error",
                servo::ConsoleLogLevel::Warn => "warning",
                _ => "log",
            };
            let _ = tx.send(format!("{level_name}\u{1f}{message}"));
        }
    }

    fn notify_new_frame_ready(&self, _webview: servo::WebView) {
        use std::sync::atomic::Ordering;
        self.frame_ready.store(true, Ordering::Release);
    }

    fn notify_url_changed(&self, _webview: servo::WebView, url: Url) {
        if let Some(tx) = &self.lifecycle_tx {
            let _ = tx.send(format!("frameNavigated\u{1f}{url}"));
        }
    }

    fn load_web_resource(&self, _webview: servo::WebView, load: servo::WebResourceLoad) {
        if let Some(tx) = &self.request_tx {
            let req = load.request();
            let _ = tx.send(format!(
                "{}\u{1f}{}",
                req.method.as_str(),
                req.url
            ));
        }
        // Fetch-domain gate: when enabled and matching, hold the load and
        // ask the client (Fetch.requestPaused). Otherwise drop the load —
        // the responder's default lets the original request proceed.
        let url = load.request().url.to_string();
        let method = load.request().method.as_str().to_string();
        let is_main = load.request().is_for_main_frame;
        let mut state = self.fetch_state.lock().expect("fetch state");
        if state.matches(&url) {
            let id = state.next_id;
            state.next_id += 1;
            let paused = json!({
                "requestId": format!("interception-{id}"),
                "request": {
                    "url": url,
                    "method": method,
                    "headers": {},
                    "isMainFrame": is_main,
                },
                "resourceType": if is_main { "Document" } else { "Other" },
            });
            state
                .pending
                .insert(format!("interception-{id}"), load);
            if let Some(tx) = &self.fetch_tx {
                let _ = tx.send(format!(
                    "requestPaused\u{1f}{}",
                    serde_json::to_string(&paused).unwrap_or_default()
                ));
            }
        }
    }

    fn notify_load_status_changed(&self, _webview: servo::WebView, status: LoadStatus) {
        if let Some(tx) = &self.lifecycle_tx {
            match status {
                LoadStatus::Started => {
                    let _ = tx.send("frameStartedLoading\u{1f}".to_string());
                },
                LoadStatus::HeadParsed => {
                    let _ = tx.send("domContentEventFired\u{1f}".to_string());
                },
                LoadStatus::Complete => {
                    let _ = tx.send("loadEventFired\u{1f}".to_string());
                    let _ = tx.send("frameStoppedLoading\u{1f}".to_string());
                },
                _ => {},
            }
        }
        *self.load_status.borrow_mut() = Some(status);
    }
}

/// Field order IS teardown order: `webview` must send CloseWebView and
/// `user_content_manager` (which clones the Servo handle) must release
/// before `servo`, so ServoInner's Drop fires last and spins the
/// constellation to a clean Exit. Reordering these leaks the kernel.
pub struct HeadlessServo {
    rendering_context: Rc<SoftwareRenderingContext>,
    webview: WebView,
    #[allow(dead_code)]
    delegate: Rc<HeadlessDelegate>,
    user_content_manager: Rc<UserContentManager>,
    /// Fetch-domain interception state (shared with the delegate).
    fetch_state: Arc<std::sync::Mutex<FetchState>>,
    /// Set by the delegate on `notify_new_frame_ready` — the event-driven
    /// pump waits on this instead of blind-polling at fixed intervals.
    frame_ready: Arc<std::sync::atomic::AtomicBool>,
    servo: Servo,
}

/// Registrable-root approximation: last two labels (or three for
/// co.uk-style suffixes). Good enough for "did the load land" checks.
fn root_domain(host: &str) -> String {
    let parts: Vec<&str> = host.trim_end_matches('.').split('.').collect();
    if parts.len() >= 3 {
        let two_last = parts[parts.len() - 2..].join(".");
        let common = ["co.uk", "com.cn", "com.hk", "com.tw", "com.au", "co.jp", "com.br"];
        if common.iter().any(|s| host.ends_with(*s)) {
            return parts[parts.len() - 3..].join(".");
        }
    }
    if parts.len() >= 2 {
        parts[parts.len() - 2..].join(".")
    } else {
        host.to_string()
    }
}

impl HeadlessServo {
    /// Build a headless Servo instance with a viewport-sized software context.
    pub fn new(viewport: (u32, u32)) -> Result<Self, String> {
        let profile = crate::fingerprint::random_profile()
            .ok_or("no TLS-compatible Chrome profile")?;
        Self::new_with_profile(viewport, &profile)
    }

    /// Like [`Self::new`] but routes page console messages to `console_tx`
    /// (level\u{1f}message) for CDP Runtime.consoleAPICalled forwarding.
    pub fn new_with_console(
        viewport: (u32, u32),
        console_tx: Option<std::sync::mpsc::Sender<String>>,
    ) -> Result<Self, String> {
        let profile = crate::fingerprint::random_profile()
            .ok_or("no TLS-compatible Chrome profile")?;
        Self::new_with_profile_and_console(viewport, &profile, console_tx)
    }

    /// Boot with an explicit fingerprint profile: the UA preference carries
    /// the profile's Chrome UA so the HTTP layer and the JS-visible
    /// navigator agree.
    pub fn new_with_profile(
        viewport: (u32, u32),
        profile: &crate::fingerprint::UaProfile,
    ) -> Result<Self, String> {
        Self::new_with_profile_and_console(viewport, profile, None)
    }

    /// Boot with an explicit fingerprint profile and an optional console
    /// message channel (level\u{1f}message) for CDP event forwarding.
    pub fn new_with_profile_and_console(
        viewport: (u32, u32),
        profile: &crate::fingerprint::UaProfile,
        console_tx: Option<std::sync::mpsc::Sender<String>>,
    ) -> Result<Self, String> {
        let (request_tx, _request_rx) = std::sync::mpsc::channel::<String>();
        Self::new_full(viewport, profile, console_tx, request_tx, None, None)
    }

    /// Full-constructor: explicit fingerprint + console/request event
    /// channels for CDP event forwarding.
    pub fn new_full(
        viewport: (u32, u32),
        profile: &crate::fingerprint::UaProfile,
        console_tx: Option<std::sync::mpsc::Sender<String>>,
        request_tx: std::sync::mpsc::Sender<String>,
        lifecycle_tx: Option<std::sync::mpsc::Sender<String>>,
        fetch_tx: Option<std::sync::mpsc::Sender<String>>,
    ) -> Result<Self, String> {
        let size = dpi::PhysicalSize::new(viewport.0, viewport.1);
        let rendering_context = Rc::new(
            SoftwareRenderingContext::new(size).map_err(|e| format!("rendering context: {e:?}"))?,
        );
        rendering_context
            .make_current()
            .map_err(|e| format!("make_current: {e:?}"))?;

        let servo = ServoBuilder::default().build();
        servo.setup_logging();
        // Servo's default UA carries a "Servo/" token that anti-bot layers
        // (Bing, Cloudflare) treat as a bot signal. Present a Chrome UA —
        // and keep the TLS ClientHello on the same Chrome version as the UA.
        servo.set_preference("user_agent", servo::PrefValue::Str(profile.user_agent.clone()));
        // :has() support ships with stylo but is pref-gated off; the WPT
        // closest suite exercises ':has(> :scope)' so turn it on.
        servo.set_preference(
            "layout.css.has-selector.enabled",
            servo::PrefValue::Boolean(true),
        );
        if let Some(chrome_version) = profile
            .user_agent
            .split("Chrome/")
            .nth(1)
            .and_then(|rest| rest.split('.').next())
            .and_then(|v| v.parse::<u32>().ok())
        {
            servo::set_active_chrome_version(chrome_version);
        }
        use std::sync::atomic::AtomicBool;
        let fetch_state = Arc::new(std::sync::Mutex::new(FetchState {
            enabled: false,
            patterns: Vec::new(),
            pending: std::collections::HashMap::new(),
            next_id: 1,
        }));
        let frame_ready = Arc::new(AtomicBool::new(false));
        let delegate = Rc::new(HeadlessDelegate {
            load_status: RefCell::new(None),
            console_tx,
            request_tx: Some(request_tx),
            lifecycle_tx,
            fetch_state: fetch_state.clone(),
            fetch_tx,
            frame_ready: frame_ready.clone(),
        });
        let user_content_manager = Rc::new(UserContentManager::new(&servo));
        let webview =
            WebViewBuilder::new(&servo, rendering_context.clone() as Rc<dyn RenderingContext>)
                .delegate(delegate.clone())
                .user_content_manager(user_content_manager.clone())
                .build();
        Ok(Self {
            rendering_context,
            webview,
            delegate,
            user_content_manager,
            fetch_state,
            frame_ready,
            servo,
        })
    }

    /// Pump until the compositor reports a new frame or `timeout` elapses.
    /// Busy-spins the event loop (no sleep) so latency tracks the kernel:
    /// frames surface the moment they're ready; idle waits cost one spin
    /// per poll tick instead of waking on a fixed clock.
    pub fn pump_until_frame(&self, timeout: Duration) -> bool {
        use std::sync::atomic::Ordering;
        let start = Instant::now();
        loop {
            if self
                .frame_ready
                .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
            self.servo.spin_event_loop();
            if self
                .frame_ready
                .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
            if start.elapsed() >= timeout {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Render + present once a frame is ready (or timeout), then read back.
    /// Replaces the fixed 16ms poll loops at call sites.
    pub fn render_when_ready(&self, timeout: Duration) {
        if self.pump_until_frame(timeout) {
            self.render_frame();
            self.present();
        } else {
            self.render_frame();
            self.present();
        }
    }

    /// Enable/disable Fetch-domain interception (Fetch.enable/disable).
    /// Patterns are substring URL filters; empty means every request.
    pub fn set_fetch_interception(&self, enabled: bool, patterns: Vec<String>) {
        let mut state = self.fetch_state.lock().expect("fetch state");
        state.enabled = enabled;
        state.patterns = patterns;
        if !enabled {
            // Dropping pending loads releases them (DoNotIntercept default).
            state.pending.clear();
        }
    }

    /// Fetch.continueRequest: release a held load unmodified.
    pub fn fetch_continue(&self, request_id: &str) -> bool {
        self.fetch_state
            .lock()
            .expect("fetch state")
            .pending
            .remove(request_id)
            .is_some()
    }

    /// Fetch.failRequest: cancel the held load (network error on the page).
    pub fn fetch_fail(&self, request_id: &str) -> bool {
        let mut state = self.fetch_state.lock().expect("fetch state");
        match state.pending.remove(request_id) {
            Some(load) => {
                let url = load.request().url.clone();
                let intercepted = load.intercept(servo::WebResourceResponse::new(url));
                let _ = intercepted.cancel();
                true
            },
            None => false,
        }
    }

    /// Fetch.fulfillRequest: answer the held load with a synthetic response.
    /// Returns true when the load was held; the caller emits
    /// Network.responseReceived/loadingFinished with the same data.
    pub fn fetch_fulfill(
        &self,
        request_id: &str,
        status_code: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> bool {
        let mut state = self.fetch_state.lock().expect("fetch state");
        let Some(load) = state.pending.remove(request_id) else {
            return false;
        };
        let url = load.request().url.clone();
        let mut header_map = http::HeaderMap::new();
        for (name, value) in headers {
            if let (Ok(name), Ok(value)) = (
                http::HeaderName::from_bytes(name.as_bytes()),
                http::HeaderValue::from_str(&value),
            ) {
                header_map.insert(name, value);
            }
        }
        let mut intercepted = load.intercept(
            servo::WebResourceResponse::new(url)
                .headers(header_map)
                .status_code(
                    http::StatusCode::from_u16(status_code)
                        .unwrap_or(http::StatusCode::OK),
                ),
        );
        if !body.is_empty() {
            intercepted.send_body_data(body);
        }
        intercepted.finish();
        true
    }

    /// Register a script that runs before any future document's scripts —
    /// the `Page.addScriptToEvaluateOnNewDocument` backing. Also evaluated
    /// once immediately so scripts registered mid-session apply to the live
    /// document the way a CDP client expects after its first navigation.
    pub fn add_initialization_script(&self, script: &str) {
        self.user_content_manager
            .add_script(Rc::new(servo::UserScript::new(script.to_string(), None)));
        let _ = self.evaluate_sync(script, Duration::from_secs(10));
    }

    /// All cookies the kernel's jar holds for the webview's current URL.
    /// Returns raw cookie-rs `Cookie`s; `cdp_cookies_json` serializes them.
    pub fn cookies_for_current_url(&self) -> Vec<cookie::Cookie<'static>> {
        let url = self
            .webview
            .url()
            .unwrap_or_else(|| Url::parse("about:blank").expect("about:blank always parses"));
        self.servo
            .site_data_manager()
            .cookies_for_url(url, CookieSource::NonHTTP)
    }

    /// Insert a cookie for `url` from a Set-Cookie-style string
    /// ("name=value; Path=/; Domain=example.com; Secure; HttpOnly").
    pub fn set_cookie_for_url(&self, url: url::Url, cookie_string: &str) -> bool {
        let parsed = match cookie::Cookie::parse(cookie_string.to_owned()) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("set_cookie_for_url: parse failed: {e}");
                return false;
            },
        };
        self.servo
            .site_data_manager()
            .set_cookie_for_url(url, parsed, None);
        true
    }

    /// Reload the current page.
    pub fn reload(&self) {
        self.webview.reload();
    }

    /// Navigate back `amount` steps in this webview's history.
    pub fn go_back(&self, amount: usize) {
        self.webview.go_back(amount);
    }

    /// Navigate forward `amount` steps in this webview's history.
    pub fn go_forward(&self, amount: usize) {
        self.webview.go_forward(amount);
    }

    /// The webview's current URL, if one is loaded.
    pub fn current_url(&self) -> Option<Url> {
        self.webview.url()
    }

    /// The page title as last reported by the kernel.
    pub fn page_title(&self) -> Option<String> {
        self.webview.page_title()
    }

    /// Focus the webview (real focus events for typing pipelines).
    pub fn focus(&self) {
        self.webview.focus();
    }

    /// Pump the event loop for `millis` so setTimeout chains (gesture
    /// replays, typing sequences) make progress.
    pub fn settle(&self, millis: u64) {
        let deadline = Instant::now() + Duration::from_millis(millis);
        while Instant::now() < deadline {
            self.servo.spin_event_loop();
            if self
                .frame_ready
                .compare_exchange(true, false, std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Acquire)
                .is_ok()
            {
                self.render_frame();
                self.present();
                continue;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Pump the Servo event loop once.
    pub fn spin(&self) {
        self.servo.spin_event_loop();
    }

    /// Render a compositor frame into the software framebuffer.
    pub fn render_frame(&self) {
        self.webview.paint();
    }

    /// Swap the back buffer to the readable side.
    pub fn present(&self) {
        let _ = self.rendering_context.present();
    }

    pub fn webview(&self) -> &WebView {
        &self.webview
    }

    /// Navigate to a URL and pump the event loop until load completes or the
    /// deadline passes. Returns whether the load reached `LoadStatus::Complete`.
    pub fn navigate(&self, url: &str, deadline: Duration) -> Result<bool, String> {
        let target = Url::parse(url).map_err(|e| format!("url parse: {e}"))?;
        // Let the constellation finish registering the browsing context from
        // the builder's NewWebView(about:blank) before sending LoadUrl —
        // otherwise it warns "LoadUrl for unknown browsing context" and the
        // load never starts.
        for _ in 0..50 {
            self.servo.spin_event_loop();
            std::thread::sleep(Duration::from_millis(10));
        }
        self.webview.load(target.clone());
        let start = Instant::now();
        loop {
            self.servo.spin_event_loop();
            // Event-driven assist: while a frame is pending, render it
            // immediately instead of waiting for the next poll tick.
            if self
                .frame_ready
                .compare_exchange(true, false, std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Acquire)
                .is_ok()
            {
                self.render_frame();
                self.present();
            }
            // Complete alone is not enough: the about:blank initial load also
            // completes, so require the visible URL to have reached the
            // target host as well (redirects keep the host suffix family).
            let at_target = self
                .webview
                .url()
                .map(|cur| {
                    // Redirects move hosts freely (cn.bing.com →
                    // www.bing.com), so compare registrable root domains.
                    match (cur.host_str(), target.host_str()) {
                        (Some(a), Some(b)) => root_domain(a) == root_domain(b),
                        (None, None) => target.scheme() == cur.scheme(),
                        _ => false,
                    }
                })
                .unwrap_or(false);
            if at_target && self.webview.load_status() == LoadStatus::Complete {
                return Ok(true);
            }
            if start.elapsed() > deadline {
                log::warn!(
                    "navigate deadline: url={:?} status={:?}",
                    self.webview.url(),
                    self.webview.load_status()
                );
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(8));
        }
    }

    /// Read the current framebuffer into RGBA bytes (width * height * 4).
    pub fn read_back(&self) -> (u32, u32, Vec<u8>) {
        let size2d = self.rendering_context.size2d();
        let rect = servo::DeviceIntRect::from_origin_and_size(
            servo::DeviceIntPoint::zero(),
            servo::DeviceIntSize::new(size2d.width as i32, size2d.height as i32),
        );
        match self.rendering_context.read_to_image(rect) {
            Some(img) => (img.width(), img.height(), img.into_raw()),
            None => (size2d.width, size2d.height, vec![0; (size2d.width * size2d.height * 4) as usize]),
        }
    }

    /// Blocking screenshot via the official take_screenshot path, pumping
    /// the loop until the callback lands.
    pub fn screenshot_rgba_blocking(&self, deadline: Duration) -> (u32, u32, Vec<u8>) {
        let result: Rc<RefCell<Option<image::RgbaImage>>> = Rc::new(RefCell::new(None));
        let slot = result.clone();
        self.webview().take_screenshot(None, move |res| match res {
            Ok(img) => *slot.borrow_mut() = Some(img),
            Err(e) => eprintln!("screenshot error: {e:?}"),
        });
        let start = Instant::now();
        let img = loop {
            assert!(
                start.elapsed() < deadline,
                "screenshot_rgba_blocking: compositor did not produce a frame within {deadline:?}"
            );
            self.servo.spin_event_loop();
            self.render_frame();
            if let Some(img) = result.borrow_mut().take() {
                break img;
            }
            // Busy at 2ms: screenshot latency matters more than idle CPU.
            std::thread::sleep(Duration::from_millis(2));
        };
        (img.width(), img.height(), img.into_raw())
    }

    /// Read the current framebuffer into RGBA bytes (width * height * 4).
    pub fn screenshot_rgba(&self) -> Result<(u32, u32, Vec<u8>), String> {
        let size2d = self.rendering_context.size2d();
        let rect = servo::DeviceIntRect::from_origin_and_size(
            servo::DeviceIntPoint::zero(),
            servo::DeviceIntSize::new(size2d.width as i32, size2d.height as i32),
        );
        let image = self
            .rendering_context
            .read_to_image(rect)
            .ok_or("read_to_image returned None")?;
        let w = image.width();
        let h = image.height();
        Ok((w, h, image.into_raw()))
    }
}

/// Deterministic teardown. Field order does the heavy lifting — `webview`
/// drops before `servo` (CloseWebView first), `user_content_manager`
/// releases its Servo clone before the last reference goes away — and
/// this final spin drains the constellation's shutdown handshake so the
/// mozjs isolate and GL context are freed here, not at process exit.
impl Drop for HeadlessServo {
    fn drop(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            self.servo.spin_event_loop();
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
