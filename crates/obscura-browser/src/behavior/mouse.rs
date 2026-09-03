//! Mouse behavior — bezier-curve trails between two points with per-step
//! jitter, plus click timing that mimics a human (longer pause before the
//! click than between down and up).
//!
//! A real human's mouse path is not a straight line: it curves toward the
//! target, overshoots slightly, then corrects. The path also has
//! micro-jitter from hand tremor. We approximate this with:
//!
//! 1. A quadratic bezier curve through a control point offset perpendicular
//!    to the straight-line path. The offset magnitude is proportional to the
//!    distance (longer paths curve more).
//! 2. Per-step jitter from the NoiseEngine, so the same seed produces the
//!    same trail.
//! 3. Variable step count — longer paths take more steps so the pointer
//!    speed stays in a human range (400-1500 px/s).
//! 4. A small overshoot past the target, then a correction back.

use super::{Millis, NoiseEngine};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseButton {
    None,
    Left,
    Middle,
    Right,
    Back,
    Forward,
}

impl MouseButton {
    pub fn code(self) -> i32 {
        match self {
            MouseButton::None => -1,
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::Back => 3,
            MouseButton::Forward => 4,
        }
    }
    pub fn mask(self) -> u64 {
        match self {
            MouseButton::None => 0,
            MouseButton::Left => 1,
            MouseButton::Right => 2,
            MouseButton::Middle => 4,
            MouseButton::Back => 8,
            MouseButton::Forward => 16,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MouseStep {
    pub x: f64,
    pub y: f64,
    pub delay_ms: Millis,
}

pub struct MouseSequence {
    seed: u64,
}

impl MouseSequence {
    pub fn new(seed: u64) -> Self {
        Self { seed }
    }

    /// Builds a trail from `from` to `to`. The trail has `steps` waypoints,
    /// each with a delay drawn from a realistic range. The caller dispatches
    /// `MouseMove` for each step.
    ///
    /// `steps=0` picks a sensible default based on the distance.
    pub fn trail(&self, from: (f64, f64), to: (f64, f64), steps: usize) -> Vec<MouseStep> {
        let engine = NoiseEngine::new(self.seed);
        let dx = to.0 - from.0;
        let dy = to.1 - from.1;
        let dist = (dx.hypot(dy)).max(1.0);

        // Step count scales with distance so pointer speed stays in a human
        // range. 1 step per ~15px, clamped to [4, 60]. Caller can override.
        let steps = if steps == 0 {
            ((dist / 15.0) as usize).clamp(4, 60)
        } else {
            steps.max(2)
        };

        // Control point: perpendicular offset. Longer paths curve more.
        let mid = (from.0 + dx * 0.5, from.1 + dy * 0.5);
        let perp = (-dy / dist, dx / dist);
        let curve_strength = (dist * 0.15).min(180.0);
        // Sign of the curve is seed-determined so the same seed curves the
        // same way.
        let sign = if engine.at(0x4d55_4f55_5300_0000, 0) % 2 == 0 {
            1.0
        } else {
            -1.0
        };
        let ctrl = (
            mid.0 + perp.0 * curve_strength * sign,
            mid.1 + perp.1 * curve_strength * sign,
        );

        let mut out = Vec::with_capacity(steps + 2);

        // Bezier from t=0 to t=1, plus a small overshoot past t=1 then a
        // correction. Total time scales with distance: roughly dist/1000s
        // → dist/1.5 ms per step at our step count, so ~600-2000px/s.
        let total_ms = (dist * 1.2).clamp(120.0, 1500.0) as Millis;
        let per_step = total_ms / steps as u64;

        for i in 1..=steps {
            let t = i as f64 / steps as f64;
            let (px, py) = bezier(from, ctrl, to, t);
            // Per-step jitter — subpixel, so a hash of the fingerprint
            // doesn't see a perfectly smooth curve.
            let jx = engine.float_in(0x4a_4954_5245_3000, i as u64, -0.5, 0.5);
            let jy = engine.float_in(0x4a_4954_5245_3100, i as u64, -0.5, 0.5);
            out.push(MouseStep {
                x: px + jx,
                y: py + jy,
                delay_ms: per_step + engine.int_in(0x44_454c_4159_0000, i as u64, 0, 20) as Millis,
            });
        }

        // Overshoot: go ~5-15px past, then correct back. Humans overshoot
        // by ~5-10% of the distance on fast movements.
        let overshoot = dist * 0.08;
        let over_target = (to.0 + dx / dist * overshoot, to.1 + dy / dist * overshoot);
        out.push(MouseStep {
            x: over_target.0,
            y: over_target.1,
            delay_ms: per_step / 2,
        });
        out.push(MouseStep {
            x: to.0,
            y: to.1,
            delay_ms: per_step,
        });

        out
    }

    /// Timing for a click at (x, y). Returns (pre_delay, down_to_up_delay)
    /// in ms. Humans pause briefly before clicking, then hold for 60-120ms.
    pub fn click_timing(&self, pos: (u64, u64)) -> (Millis, Millis) {
        let engine = NoiseEngine::new(self.seed);
        let pre = engine.int_in(0x43_4c49_434b_0000, pos.0.wrapping_add(pos.1), 80, 280) as Millis;
        let hold = engine.int_in(0x48_4f4c_4400_0000, pos.0, 60, 130) as Millis;
        (pre, hold)
    }
}

fn bezier(p0: (f64, f64), p1: (f64, f64), p2: (f64, f64), t: f64) -> (f64, f64) {
    let u = 1.0 - t;
    let x = u * u * p0.0 + 2.0 * u * t * p1.0 + t * t * p2.0;
    let y = u * u * p0.1 + 2.0 * u * t * p1.1 + t * t * p2.1;
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trail_starts_near_from_and_ends_at_to() {
        let m = MouseSequence::new(42);
        let trail = m.trail((10.0, 10.0), (200.0, 150.0), 0);
        let first = trail.first().unwrap();
        // First waypoint is close to `from` (bezier at small t).
        assert!((first.x - 10.0).abs() < 30.0);
        let last = trail.last().unwrap();
        // Last waypoint is exactly `to` (the correction step).
        assert!((last.x - 200.0).abs() < 0.01);
        assert!((last.y - 150.0).abs() < 0.01);
    }

    #[test]
    fn same_seed_same_trail() {
        let a = MouseSequence::new(7).trail((0.0, 0.0), (100.0, 100.0), 8);
        let b = MouseSequence::new(7).trail((0.0, 0.0), (100.0, 100.0), 8);
        assert_eq!(a, b);
    }

    #[test]
    fn different_seed_different_trail() {
        let a = MouseSequence::new(1).trail((0.0, 0.0), (100.0, 100.0), 8);
        let b = MouseSequence::new(2).trail((0.0, 0.0), (100.0, 100.0), 8);
        assert_ne!(a, b);
    }

    #[test]
    fn click_timing_in_human_range() {
        let m = MouseSequence::new(99);
        let (pre, hold) = m.click_timing((100, 200));
        assert!((80..=280).contains(&pre));
        assert!((60..=130).contains(&hold));
    }

    #[test]
    fn trail_has_overshoot_then_correction() {
        let m = MouseSequence::new(42);
        let trail = m.trail((0.0, 0.0), (300.0, 0.0), 10);
        // Last two steps: overshoot (x > 300) then correction (x == 300).
        let n = trail.len();
        assert!(trail[n - 2].x > 300.0, "overshoot should exceed target");
        assert!((trail[n - 1].x - 300.0).abs() < 0.01);
    }
}
