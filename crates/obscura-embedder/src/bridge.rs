//! CDP-facing bridge over the live Servo kernel. Synchronous wrappers for
//! the async embedder APIs (evaluate, input) — the CDP dispatcher runs
//! these on its own thread and drives the event loop until results land.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use euclid::Point2D;
use servo::embedder_traits::JSValue;
use servo::webrender_api::units::DevicePoint;
use servo::{
    InputEvent, MouseButton, MouseButtonAction, MouseButtonEvent, MouseMoveEvent, WebViewPoint,
};

fn device_point(x: f32, y: f32) -> WebViewPoint {
    WebViewPoint::Device(DevicePoint::new(x, y))
}

use crate::HeadlessServo;

/// Spin the kernel until the closure says done or the deadline passes.
fn pump_until(servo: &HeadlessServo, done: impl Fn() -> bool, deadline: Duration) -> bool {
    let start = Instant::now();
    loop {
        servo.spin();
        servo.render_frame();
        if done() {
            return true;
        }
        if start.elapsed() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(4));
    }
}

impl HeadlessServo {
    /// Evaluate JavaScript and wait for the result (up to `deadline`).
    /// Returns the JSValue serialized as a JSON-ish string, mirroring the
    /// legacy CDP Runtime.evaluate shape the Go client expects.
    pub fn evaluate_sync(&self, script: &str, deadline: Duration) -> Result<String, String> {
        let webview = self.webview().clone();
        let slot: Rc<Cell<Option<Result<String, String>>>> = Rc::new(Cell::new(None));
        let slot2 = slot.clone();
        webview.evaluate_javascript(script, move |result| {
            let s = match result {
                Ok(v) => Ok(jsvalue_to_string(&v)),
                Err(e) => Err(format!("eval error: {e:?}")),
            };
            slot2.set(Some(s));
        });
        let ok = pump_until(self, || slot.get().is_some(), deadline);
        if !ok {
            return Err("evaluate timed out".into());
        }
        slot.take().unwrap_or_else(|| Err("no result".into()))
    }

    /// CDP Input.dispatchMouseEvent mapping onto the kernel's real input
    /// pipeline (true hit-testing, trusted events, default actions).
    pub fn dispatch_mouse(&self, event_type: &str, x: f32, y: f32) {
        let point = device_point(x, y);
        match event_type {
            "mouseMoved" => {
                self.webview()
                    .notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(point)));
            },
            "mousePressed" => {
                self.webview().notify_input_event(InputEvent::MouseButton(
                    MouseButtonEvent::new(MouseButtonAction::Down, MouseButton::Left, point),
                ));
            },
            "mouseReleased" => {
                self.webview().notify_input_event(InputEvent::MouseButton(
                    MouseButtonEvent::new(MouseButtonAction::Up, MouseButton::Left, point),
                ));
            },
            other => log::warn!("unmapped mouse event type: {other}"),
        }
        // Let the input event travel through constellation → script.
        self.spin();
    }
}

fn jsvalue_to_string(v: &JSValue) -> String {
    match v {
        JSValue::Undefined => "undefined".into(),
        JSValue::Null => "null".into(),
        JSValue::Boolean(b) => b.to_string(),
        JSValue::Number(n) => {
            if n.fract() == 0.0 && n.abs() < 1e15 {
                format!("{}", *n as i64)
            } else {
                n.to_string()
            }
        },
        JSValue::String(s) => s.clone(),
        JSValue::Element(s) | JSValue::ShadowRoot(s) | JSValue::Frame(s) | JSValue::Window(s) => {
            s.clone()
        },
        JSValue::Array(items) => {
            let inner: Vec<String> = items.iter().map(jsvalue_to_string).collect();
            format!("[{}]", inner.join(","))
        },
        JSValue::Object(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}:{}", k, jsvalue_to_string(v)))
                .collect();
            format!("{{{}}}", inner.join(","))
        },
    }
}
