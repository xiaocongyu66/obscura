//! Browser fingerprint generation and JS injection.
//!
//! Ported from stygian-browser (greysquirr3l/stygian) — complete implementation
//! with deterministic NoiseEngine for canvas/audio, full navigator spoofing,
//! WebGL vendor/renderer/extensions, WebRTC IP leak prevention, mediaDevices,
//! storage, battery, plugins, fonts.
//!
//! Layout:
//! - [`identity`] — `Fingerprint` struct + seed-based deterministic generation
//! - [`noise`] — `NoiseEngine` (canvas/audio/clientRects deterministic perturbation)
//! - [`scripts`] — JS injection script generators for each surface
//! - [`inject`] — assembles the final `addScriptToEvaluateOnNewDocument` payload

pub mod identity;
pub mod noise;
pub mod scripts;
pub mod inject;

pub use identity::{Fingerprint, FingerprintProfile};
pub use inject::build_injection_script;
pub use noise::NoiseEngine;

/// Convenience: random fingerprint + its injection script in one call.
pub fn random_injection_script() -> String {
    Fingerprint::random().injection_script()
}
