//! Assembles the final `addScriptToEvaluateOnNewDocument` payload from all
//! surface scripts. This is the single entry point used by `Page` when it
//! creates a new realm.

use crate::fingerprint::identity::Fingerprint;
use crate::fingerprint::scripts;

/// Builds the full injection script for one fingerprint.
///
/// Order matters: identity properties (UA, platform, language) go first so
/// any surface that reads them (e.g. speech voices, font list) sees the
/// spoofed values. Canvas/audio noise goes last so the spoofed `getImageData`
/// sees the patched `getParameter` results.
pub fn build_injection_script(fp: &Fingerprint) -> String {
    let (w, h) = fp.screen_resolution;
    let mut parts = Vec::with_capacity(16);

    // Identity.
    parts.push(scripts::screen_script(w, h));
    parts.push(scripts::timezone_script(&fp.timezone));
    parts.push(scripts::language_script(
        &fp.language,
        &fp.secondary_language,
        &fp.user_agent,
    ));
    parts.push(scripts::hardware_script(
        &fp.platform,
        fp.hardware_concurrency,
        fp.device_memory,
    ));

    // WebGL.
    if let (Some(v), Some(r)) = (&fp.webgl_vendor, &fp.webgl_renderer) {
        parts.push(scripts::webgl_script(v, r));
    }

    // Deterministic noise surfaces.
    if fp.canvas_noise {
        parts.push(scripts::canvas_noise_script(fp.noise_seed));
    }
    if fp.audio_noise {
        parts.push(scripts::audio_noise_script(fp.noise_seed));
    }
    parts.push(scripts::client_rects_noise_script(fp.noise_seed));

    // Stable spoof surfaces.
    parts.push(scripts::connection_script());
    parts.push(scripts::storage_estimate_script());
    parts.push(scripts::battery_script());
    parts.push(scripts::plugins_script());
    parts.push(scripts::media_devices_script());
    parts.push(scripts::webrtc_leak_script());
    parts.push(scripts::speech_voices_script(&fp.platform));
    parts.push(scripts::font_measurement_script(
        fp.noise_seed,
        &fp.platform,
        &fp.fonts,
    ));
    parts.push(scripts::permissions_script());

    format!("(function() {{\n{}\n}})();", parts.join("\n\n"))
}
