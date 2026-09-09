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
use std::time::{Duration, Instant};

use servo::{
    CookieSource, LoadStatus, RenderingContext, Servo, ServoBuilder, SoftwareRenderingContext,
    UserContentManager, WebView, WebViewBuilder, WebViewDelegate,
};
use url::Url;

/// Delegate that records load-status transitions and broadcasts console
/// messages to CDP subscribers (Runtime.consoleAPICalled backing).
pub struct HeadlessDelegate {
    pub load_status: RefCell<Option<LoadStatus>>,
    pub console_tx: Option<std::sync::mpsc::Sender<String>>,
    /// Network.requestWillBeSent feed: one line per web resource the page
    /// starts loading ("method\u{1f}url").
    pub request_tx: Option<std::sync::mpsc::Sender<String>>,
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

    fn load_web_resource(&self, _webview: servo::WebView, load: servo::WebResourceLoad) {
        if let Some(tx) = &self.request_tx {
            let req = load.request();
            let _ = tx.send(format!(
                "{}\u{1f}{}",
                req.method.as_str(),
                req.url
            ));
        }
        // Not intercepted: the load continues as normal.
    }

    fn notify_load_status_changed(&self, _webview: servo::WebView, status: LoadStatus) {
        *self.load_status.borrow_mut() = Some(status);
    }
}

pub struct HeadlessServo {
    servo: Servo,
    rendering_context: Rc<SoftwareRenderingContext>,
    webview: WebView,
    #[allow(dead_code)]
    delegate: Rc<HeadlessDelegate>,
    user_content_manager: Rc<UserContentManager>,
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
        Self::new_full(viewport, profile, console_tx, request_tx)
    }

    /// Full-constructor: explicit fingerprint + console/request event
    /// channels for CDP event forwarding.
    pub fn new_full(
        viewport: (u32, u32),
        profile: &crate::fingerprint::UaProfile,
        console_tx: Option<std::sync::mpsc::Sender<String>>,
        request_tx: std::sync::mpsc::Sender<String>,
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
        if let Some(chrome_version) = profile
            .user_agent
            .split("Chrome/")
            .nth(1)
            .and_then(|rest| rest.split('.').next())
            .and_then(|v| v.parse::<u32>().ok())
        {
            servo::set_active_chrome_version(chrome_version);
        }
        let delegate = Rc::new(HeadlessDelegate {
            load_status: RefCell::new(None),
            console_tx,
            request_tx: Some(request_tx),
        });
        let user_content_manager = Rc::new(UserContentManager::new(&servo));
        let webview =
            WebViewBuilder::new(&servo, rendering_context.clone() as Rc<dyn RenderingContext>)
                .delegate(delegate.clone())
                .user_content_manager(user_content_manager.clone())
                .build();
        Ok(Self {
            servo,
            rendering_context,
            webview,
            delegate,
            user_content_manager,
        })
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

    /// Pump the event loop for `millis` so setTimeout chains (gesture
    /// replays, typing sequences) make progress.
    pub fn settle(&self, millis: u64) {
        let deadline = Instant::now() + Duration::from_millis(millis);
        while Instant::now() < deadline {
            self.servo.spin_event_loop();
            std::thread::sleep(Duration::from_millis(8));
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
        let _ = deadline;
        let img = loop {
            self.spin();
            self.render_frame();
            if let Some(img) = result.borrow_mut().take() {
                break img;
            }
            std::thread::sleep(Duration::from_millis(16));
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
