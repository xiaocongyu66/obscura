//! obscura-embedder: drives the vendored Servo engine headlessly.
//!
//! Phase 2 of the Servo migration (docs/SERVO_MIGRATION.md): assemble a
//! SoftwareRenderingContext + WebView without any display, pump the event
//! loop ourselves, and expose load/screenshot so the CDP bridge can later
//! point at a live Servo kernel instead of the legacy obscura-js engine.

pub mod bridge;
pub mod cdp_server;
pub mod tools;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use servo::{
    LoadStatus, RenderingContext, Servo, ServoBuilder, SoftwareRenderingContext, WebView,
    WebViewBuilder, WebViewDelegate,
};
use url::Url;

/// Delegate that just records load-status transitions; the harness polls.
pub struct HeadlessDelegate {
    pub load_status: RefCell<Option<LoadStatus>>,
}

impl WebViewDelegate for HeadlessDelegate {
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
}

impl HeadlessServo {
    /// Build a headless Servo instance with a viewport-sized software context.
    pub fn new(viewport: (u32, u32)) -> Result<Self, String> {
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
        // (Bing, Cloudflare) treat as a bot signal. Present a Chrome UA.
        servo.set_preference(
            "user_agent",
            servo::PrefValue::Str(
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36".into(),
            ),
        );
        let delegate = Rc::new(HeadlessDelegate {
            load_status: RefCell::new(None),
        });
        let webview =
            WebViewBuilder::new(&servo, rendering_context.clone() as Rc<dyn RenderingContext>)
                .delegate(delegate.clone())
                .build();
        Ok(Self {
            servo,
            rendering_context,
            webview,
            delegate,
        })
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
                    match (cur.host_str(), target.host_str()) {
                        (Some(a), Some(b)) => a == b || a.ends_with(b) || b.ends_with(a),
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
