//! Scroll behavior — wheel events with momentum, overshoot, and
//! human-like delta magnitudes. Real scrolling has:
//!
//! 1. A burst of wheel events with ~100-300 delta each.
//! 2. Decelerating momentum after the burst.
//! 3. Occasional overshoot past the target, then a correction.
//!
//! The output is a sequence of `BehaviorAction::Wheel` events. The caller
//! dispatches them via `Input.dispatchMouseEvent` with type `mouseWheel`.

use super::{Millis, NoiseEngine};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WheelStep {
    pub dx: f64,
    pub dy: f64,
    pub delay_ms: Millis,
}

pub struct ScrollSequence {
    seed: u64,
}

impl ScrollSequence {
    pub fn new(seed: u64) -> Self {
        Self { seed }
    }

    /// Builds a scroll sequence that moves `total_dy` pixels vertically
    /// (positive = down). The sequence has a burst of large deltas, then
    /// decelerating smaller ones, then a small overshoot + correction.
    pub fn scroll_by(&self, total_dy: f64) -> Vec<WheelStep> {
        let engine = NoiseEngine::new(self.seed);
        if total_dy.abs() < 1.0 {
            return Vec::new();
        }

        let sign = total_dy.signum();
        let mag = total_dy.abs();

        // Number of wheel events: ~1 per 120px, clamped to [3, 30].
        let n = ((mag / 120.0) as usize).clamp(3, 30);

        // Each event's delta starts large and decays. The first few events
        // carry most of the momentum.
        let mut deltas = Vec::with_capacity(n);
        let mut remaining = mag;
        for i in 0..n {
            // Weight: first event ~30% of total, decaying.
            let weight = if i == 0 {
                0.30
            } else {
                (0.30_f64).powi(i as i32 + 1).min(0.15)
            };
            let d = (mag * weight).max(40.0).min(remaining);
            deltas.push(d);
            remaining -= d;
            if remaining < 1.0 {
                break;
            }
        }

        // If we didn't reach the target (weights decayed too fast), dump
        // the remainder in one final event.
        if remaining > 1.0 {
            deltas.push(remaining);
        }

        // Overshoot: ~5% of total past the target, then correct.
        let overshoot = mag * 0.05;
        deltas.push(overshoot);
        deltas.push(overshoot); // correction (opposite sign)

        let mut out = Vec::with_capacity(deltas.len());
        for (i, d) in deltas.iter().enumerate() {
            // Last two deltas: overshoot (same sign as total) then
            // correction (opposite sign).
            let is_correction = i == deltas.len() - 1;
            let is_overshoot = i == deltas.len() - 2;
            let signed = if is_correction {
                -sign * d
            } else {
                sign * d
            };
            // Per-event jitter on the magnitude.
            let jitter = engine.float_in(0x53_4352_4f4c_4c00, i as u64, -3.0, 3.0);
            // Delay: shorter at the start of the burst, longer at the end
            // (deceleration).
            let t = i as f64 / deltas.len() as f64;
            let base_delay = 16.0 + t * 50.0; // 16ms (60fps) -> 66ms
            let delay_jitter = engine.float_in(0x44_454c_4159_0000, i as u64, 0.0, 20.0);

            out.push(WheelStep {
                dx: 0.0,
                dy: signed + jitter,
                delay_ms: (base_delay + delay_jitter) as Millis,
            });
            // Suppress the unused warning cleanly when not in overshoot.
            let _ = is_overshoot;
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_for_tiny_distance() {
        let s = ScrollSequence::new(42);
        assert!(s.scroll_by(0.5).is_empty());
    }

    #[test]
    fn total_scroll_matches_target() {
        let s = ScrollSequence::new(42);
        let steps = s.scroll_by(1000.0);
        let total: f64 = steps.iter().map(|s| s.dy).sum();
        // Total should be ~1000 (with overshoot/correction cancelling).
        assert!((total - 1000.0).abs() < 10.0, "total was {total}");
    }

    #[test]
    fn same_seed_same_scroll() {
        let a = ScrollSequence::new(7).scroll_by(500.0);
        let b = ScrollSequence::new(7).scroll_by(500.0);
        assert_eq!(a, b);
    }

    #[test]
    fn deltas_decelerate() {
        let s = ScrollSequence::new(99);
        let steps = s.scroll_by(2000.0);
        // First delta should be larger than the middle ones (burst > coast).
        // We skip the very last two (overshoot/correction) for this check.
        let n = steps.len();
        if n > 4 {
            let first = steps[0].dy.abs();
            let mid = steps[n / 2].dy.abs();
            assert!(first >= mid, "first={first} should be >= mid={mid}");
        }
    }
}
