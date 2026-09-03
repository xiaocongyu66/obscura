pub mod cdp_watchdog;
pub mod frame;
mod import_map;
pub mod markdown;
pub mod module_loader;
pub mod ops;
pub mod runtime;
pub mod v8_flags;
mod write_stream;

pub use markdown::HTML_TO_MARKDOWN_JS;
pub use v8_flags::set_v8_flags;

/// Global flag: whether to use the Rust (PortableGL) WebGL backend.
/// Set by `--webgl-rust` CLI flag. When false, `op_webgl_create_context`
/// returns false and the JS stub handles WebGL (sufficient for
/// fingerprinting probes). When true, a real GlContext is created and
/// GLSL shaders are compiled + interpreted.
static WEBGL_RUST_BACKEND: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Set the global WebGL backend flag. Called from the CLI when
/// `--webgl-rust` is passed.
pub fn set_webgl_rust_backend(enabled: bool) {
    WEBGL_RUST_BACKEND.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

/// Read the global WebGL backend flag. Used by `op_webgl_create_context`.
pub fn webgl_rust_backend_enabled() -> bool {
    WEBGL_RUST_BACKEND.load(std::sync::atomic::Ordering::Relaxed)
}

// Screenshot rasterization (PNG bytes) from the render layer. Available when the
// render feature (which enables obscura-render/paint) is compiled in.
#[cfg(feature = "render")]
pub use obscura_render::{
    screenshot_png, screenshot_png_scrolled, screenshot_png_scrolled_at_animation_time,
    screenshot_png_scrolled_at_animation_time_with_surface_color,
    validate_capture_region, AnimationSample, AnimationSampleMode, AnimationSampleTime,
    CaptureError, CaptureRegion, CssMediaType, ImageRequestProfile,
    MAX_CAPTURE_DIMENSION, MAX_CAPTURE_PIXELS,
};
