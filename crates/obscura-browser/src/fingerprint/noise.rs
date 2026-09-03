//! `NoiseEngine` — deterministic per-session noise for canvas pixels, audio
//! samples, and `getClientRects` measurements.
//!
//! Anti-bot vendors hash canvas/audio output to identify headless browsers.
//! Random noise defeats the hash but breaks if it changes between calls
//! within one session (the vendor sees two different hashes for the same
//! page and flags it). The NoiseEngine is seeded once per session and
//! produces the same perturbation for the same input, so:
//!
//! 1. Two pages in the same session produce the same canvas hash.
//! 2. Two sessions produce different canvas hashes.
//! 3. The noise is small enough to be invisible to humans.
//!
//! This is the approach stygian takes: a seedable PRNG (here SplitMix64)
//! keyed by `(session_seed, surface_tag, input_index)` so each surface has
//! its own independent stream.

/// Stateful PRNG that produces stable noise for a given session seed.
///
/// The engine is intentionally cheap (no allocs, no `Rc<RefCell>`). The
/// injection script carries the seed and re-derives the same stream in JS,
/// so the Rust side is only used for testing/verification and for deciding
/// whether noise is on for a given surface.
pub struct NoiseEngine {
    seed: u64,
}

impl NoiseEngine {
    pub fn new(seed: u64) -> Self {
        Self { seed }
    }

    /// Returns the seed, so the JS injection script can re-derive the same
    /// noise stream inside the page.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// Mixes the session seed with a surface tag and an index to produce a
    /// stable u64. The JS side mirrors this with the same SplitMix64.
    pub fn at(&self, surface: u64, index: u64) -> u64 {
        let mut s = self
            .seed
            .wrapping_add(surface.wrapping_mul(0x9e37_79b9_7f4a_7c15))
            .wrapping_add(index.wrapping_mul(0x517c_c1b7_2722_0a95));
        splitmix64(&mut s)
    }

    /// Returns a deterministic `i32` in `[lo, hi]` for the given surface/index.
    pub fn int_in(&self, surface: u64, index: u64, lo: i32, hi: i32) -> i32 {
        let v = self.at(surface, index);
        let span = (hi as i64 - lo as i64).max(0) as u64 + 1;
        lo + (v % span) as i32
    }

    /// Returns a deterministic `f64` in `[lo, hi)` for the given surface/index.
    pub fn float_in(&self, surface: u64, index: u64, lo: f64, hi: f64) -> f64 {
        let v = self.at(surface, index);
        let t = (v as f64) / (u64::MAX as f64);
        lo + t * (hi - lo)
    }

    /// Canvas: the per-channel perturbation for pixel `i` (0..width*height).
    /// Returns a small signed delta in `[-1, 1]` for R/G/B. Alpha is left
    /// alone so transparent pixels stay transparent.
    pub fn canvas_pixel_delta(&self, channel: u8, pixel_index: u64) -> i32 {
        // surface tag = 0x43_41_4e_56_31 // "CANV1"
        let surface = 0x4341_4e56_3100_0000 | u64::from(channel);
        let v = self.at(surface, pixel_index);
        // Map to {-1, 0, +1} — a single-bit flip is enough to change the hash
        // and stays invisible to the eye.
        (v % 3) as i32 - 1
    }

    /// Audio: the per-sample perturbation added to float frequency data.
    /// Range is `±1e-7` — below the threshold any analyser visualises.
    pub fn audio_sample_delta(&self, sample_index: u64) -> f64 {
        let surface = 0x4155_4449_4f31_0000; // "AUDIO1"
        self.float_in(surface, sample_index, -1e-7, 1e-7)
    }

    /// clientRects: the per-rect x/y/w/h perturbation. `±1` subpixel —
    /// enough to break a deterministic rect hash, invisible to layout.
    pub fn client_rect_delta(&self, rect_index: u64, dim: u8) -> f64 {
        let surface = 0x5245_4354_3100_0000 | u64::from(dim); // "RECT1"
        self.float_in(surface, rect_index, -1.0, 1.0)
    }
}

const fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_noise() {
        let a = NoiseEngine::new(42);
        let b = NoiseEngine::new(42);
        for i in 0..16 {
            assert_eq!(a.canvas_pixel_delta(0, i), b.canvas_pixel_delta(0, i));
            assert_eq!(a.audio_sample_delta(i), b.audio_sample_delta(i));
        }
    }

    #[test]
    fn different_seed_different_noise() {
        let a = NoiseEngine::new(1);
        let b = NoiseEngine::new(2);
        let mut diffs = 0;
        for i in 0..32 {
            if a.canvas_pixel_delta(0, i) != b.canvas_pixel_delta(0, i) {
                diffs += 1;
            }
        }
        assert!(diffs > 16, "seeds produced too-similar streams");
    }

    #[test]
    fn canvas_delta_in_range() {
        let e = NoiseEngine::new(7);
        for i in 0..256 {
            let d = e.canvas_pixel_delta(0, i);
            assert!((-1..=1).contains(&d));
        }
    }

    #[test]
    fn audio_delta_subvisible() {
        let e = NoiseEngine::new(7);
        for i in 0..256 {
            let d = e.audio_sample_delta(i);
            assert!(d.abs() < 1e-6);
        }
    }
}
