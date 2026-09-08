//! CDP-facing bridge over the live Servo kernel. Synchronous wrappers for
//! the async embedder APIs (evaluate, input) — the CDP dispatcher runs
//! these on its own thread and drives the event loop until results land.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use keyboard_types::{Key, KeyState};
use servo::{
    DevicePoint, DeviceVector2D, InputEvent, JSValue, KeyboardEvent, MouseButton, MouseButtonAction,
    MouseButtonEvent, MouseMoveEvent, Scroll, WebViewPoint, WebViewVector,
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
        use std::cell::RefCell;
        let webview = self.webview().clone();
        let slot: Rc<RefCell<Option<Result<String, String>>>> = Rc::new(RefCell::new(None));
        let slot2 = slot.clone();
        webview.evaluate_javascript(script, move |result| {
            let s = match result {
                Ok(v) => Ok(jsvalue_to_string(&v)),
                Err(e) => Err(format!("eval error: {e:?}")),
            };
            *slot2.borrow_mut() = Some(s);
        });
        let ok = pump_until(self, || slot.borrow().is_some(), deadline);
        if !ok {
            return Err("evaluate timed out".into());
        }
        slot.borrow_mut()
            .take()
            .unwrap_or_else(|| Err("no result".into()))
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
                    MouseButtonEvent::new(MouseButtonAction::Down, MouseButton::Primary, point),
                ));
            },
            "mouseReleased" => {
                self.webview().notify_input_event(InputEvent::MouseButton(
                    MouseButtonEvent::new(MouseButtonAction::Up, MouseButton::Primary, point),
                ));
            },
            other => log::warn!("unmapped mouse event type: {other}"),
        }
        // Let the input event travel through constellation → script.
        self.spin();
    }

    /// CDP Input.dispatchKeyEvent mapping. `key`/`code` per the UI Events
    /// spec; `text` present only for printable keyDowns (char insertion).
    pub fn dispatch_key(&self, event_type: &str, key: &str, code: &str, text: Option<&str>) {
        let kb = keyboard_types::KeyboardEvent {
            key: Key::Character(key.into()),
            code: keyboard_types::Code::from(code.to_string()),
            state: match event_type {
                "keyUp" => KeyState::Up,
                _ => KeyState::Down,
            },
            ..Default::default()
        };
        self.webview()
            .notify_input_event(InputEvent::Keyboard(KeyboardEvent::new(kb)));
        self.spin();
    }

    /// CDP Input.dispatchMouseEvent(mouseWheel) mapping: scroll the
    /// scrollable area under the point.
    pub fn dispatch_wheel(&self, dx: f32, dy: f32, x: f32, y: f32) {
        self.webview().notify_scroll_event(
            Scroll::Delta(WebViewVector::Device(servo::DeviceVector2D::new(dx, dy))),
            device_point(x, y),
        );
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
