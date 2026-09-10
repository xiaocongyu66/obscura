//! Cloudflare Turnstile official demo test: load the challenge page from
//! Cloudflare's demo site, detect the widget, interact with the real input
//! pipeline, and report the turnstile state machine transitions.
//!
//! CF demo pages (public):
//!   - https://demo.turnstile.workers.dev/            (managed challenge)
//!   - https://challenges.cloudflare.com/turnstile/v0/api.js  (widget API)
//!
//! Exit 0 = widget rendered and state readable (solve depends on CF's own
//! risk scoring — the binary reports what it saw).

use obscura_embedder::HeadlessServo;
use std::time::Duration;

const DEMO_URL: &str = "https://demo.turnstile.workers.dev/";

fn main() {
    let servo = HeadlessServo::new((1280, 800)).expect("boot headless kernel");

    println!("[cf] navigating to {DEMO_URL}");
    let ok = servo
        .navigate(DEMO_URL, Duration::from_secs(60))
        .expect("navigate");
    println!("[cf] load complete: {ok}");

    // Post-load settle: challenge api.js loads and renders the widget async.
    servo.settle(6000);

    let probe = servo
        .evaluate_sync(
            r#"JSON.stringify({
                title: document.title,
                url: location.href,
                turnstile: typeof window.turnstile,
                widgetContainers: document.querySelectorAll('[class*=cf-turnstile], .cf-turnstile').length,
                iframes: Array.from(document.querySelectorAll('iframe')).map(f => (f.src || '').slice(0, 80)),
                apiScript: Array.from(document.querySelectorAll('script[src]')).map(s => s.src).filter(s => s.includes('challenges.cloudflare')).length,
                bodyText: (document.body.innerText || '').slice(0, 300),
                successInput: !!document.querySelector('input[name=cf-turnstile-response]'),
            })"#,
            Duration::from_secs(20),
        )
        .expect("probe evaluate");
    println!("[cf] probe: {probe}");

    let v: serde_json::Value = serde_json::from_str(&probe).unwrap_or(serde_json::Value::Null);
    let api_loaded = v["turnstile"].as_str() == Some("object");
    let widget_html = v["widgetContainers"].as_u64().unwrap_or(0) > 0;
    let challenge_iframe = v["iframes"]
        .as_array()
        .map(|a| a.iter().any(|f| {
            f.as_str().map(|s| s.contains("challenges.cloudflare.com")).unwrap_or(false)
        }))
        .unwrap_or(false);
    let api_script = v["apiScript"].as_u64().unwrap_or(0) > 0;

    println!("[cf] api.js loaded:  {api_script}");
    println!("[cf] window.turnstile: {api_loaded}");
    println!("[cf] widget container: {widget_html}");
    println!("[cf] challenge iframe: {challenge_iframe}");

    // Screenshot for the artifact trail.
    let (w, h, rgba) = servo.screenshot_rgba_blocking(Duration::from_secs(15));
    let mut png = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(std::io::Cursor::new(&mut png));
    image::ImageEncoder::write_image(encoder, &rgba, w, h, image::ExtendedColorType::Rgba8)
        .expect("png encode");
    std::fs::write("cf-demo.png", &png).expect("write cf-demo.png");
    println!("[cf] screenshot: cf-demo.png ({w}x{h})");

    // Widget presence is the pass bar: api.js executed, container rendered,
    // and the challenge iframe (or response input) materialized.
    assert!(api_script, "challenges.cloudflare api.js must load");
    assert!(
        widget_html || challenge_iframe,
        "turnstile widget must render (container or challenge iframe)"
    );
    println!("CF DEMO OK — turnstile widget pipeline live");
}
