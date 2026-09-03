//! Behavior simulation — human-like mouse, keyboard, and scroll input.
//!
//! Anti-bot vendors flag instant clicks, straight-line mouse movement, and
//! perfectly timed keypresses. This module generates input sequences that
//! mimic a human: bezier-curve mouse trails with jitter, keypress delays
//! drawn from a realistic distribution, scroll with momentum and overshoot.
//!
//! Every sequence is reproducible from a seed so a session can replay the
//! same trajectory, and two sessions diverge. The NoiseEngine is reused
//! for the per-step jitter.
//!
//! Layout:
//! - [`mouse`] — bezier-curve trails, hover jitter, click timing
//! - [`keyboard`] — keypress delay distribution, typo + correction
//! - [`scroll`] — momentum, overshoot, human-like wheel deltas
//! - [`planner`] — high-level actions (click_at, type_into, scroll_to)
//!   that compose the primitives into a full sequence
//!
//! The output is a list of [`BehaviorEvent`]s. The caller (CDP layer, MCP
//! layer, or a Go client) is responsible for dispatching them — this module
//! is pure computation so it can be tested without a V8 isolate.

pub mod keyboard;
pub mod mouse;
pub mod planner;
pub mod scroll;

pub use keyboard::KeyboardSequence;
pub use mouse::{MouseButton, MouseSequence, MouseStep};
pub use planner::{plan_click, plan_move, plan_press_key, plan_scroll, plan_type};
pub use scroll::ScrollSequence;

/// Re-export the NoiseEngine so callers don't need to reach into the
/// fingerprint module to build a behavior session.
pub use crate::fingerprint::NoiseEngine;

/// Time unit for behavior events. Milliseconds because that's what CDP's
/// `Input.dispatchMouseEvent` and `setTimeout` both use.
pub type Millis = u64;

/// A single atomic input event with a delay before it. The delay is
/// relative to the previous event in the sequence, not absolute. This
/// matches how a real input stream looks: "wait 120ms, then move to (x,y),
/// wait 40ms, then press button".
#[derive(Debug, Clone, PartialEq)]
pub struct BehaviorEvent {
    /// Delay before this event fires, in milliseconds.
    pub delay_ms: Millis,
    /// What to do.
    pub action: BehaviorAction,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BehaviorAction {
    /// Move the pointer to (x, y). Does not press anything.
    MouseMove { x: f64, y: f64 },
    /// Press a mouse button at (x, y).
    MouseDown { x: f64, y: f64, button: MouseButton },
    /// Release a mouse button at (x, y).
    MouseUp { x: f64, y: f64, button: MouseButton },
    /// Press a key (keydown + keypress). `key` is a KeyDefinition-like
    /// string: "a", "Enter", "Shift", etc.
    KeyDown { key: String, code: String, modifiers: u8 },
    /// Release a key.
    KeyUp { key: String, code: String, modifiers: u8 },
    /// Insert text at the caret (the `Input.insertText` CDP method).
    InsertText { text: String },
    /// Scroll the wheel by (dx, dy) at (x, y).
    Wheel { x: f64, y: f64, dx: f64, dy: f64 },
}

/// Modifiers bitmask. Matches CDP/USB HID bit assignments:
/// 1 = alt, 2 = ctrl, 4 = meta, 8 = shift.
pub mod modifiers {
    pub const NONE: u8 = 0;
    pub const ALT: u8 = 1;
    pub const CTRL: u8 = 2;
    pub const META: u8 = 4;
    pub const SHIFT: u8 = 8;
}

#[cfg(test)]
mod smoke {
    use super::*;

    #[test]
    fn noise_engine_reused() {
        let e = NoiseEngine::new(42);
        // Just confirm the re-export compiles and works.
        assert_eq!(e.canvas_pixel_delta(0, 0), e.canvas_pixel_delta(0, 0));
    }
}
