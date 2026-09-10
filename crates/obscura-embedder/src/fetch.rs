//! Rendered-page fetch for the CLI: one kernel per request, same shape as
//! the cdp_server kernel thread but with the CLI dump surface instead of a
//! WS protocol. The kernel lives only as long as the request.

use crate::page_dumps::{dump_assets, dump_cookies, dump_html, dump_links, dump_markdown, dump_text};
use crate::HeadlessServo;
use std::sync::mpsc::channel;
use std::time::Duration;

pub struct FetchRequest {
    pub url: String,
    pub timeout_secs: u64,
    /// Capture a PNG alongside the dump (CLI --screenshot).
    pub screenshot: bool,
    /// Optional JS evaluated after load; its value is returned separately
    /// (CLI --eval). Runs before dumps when `dump_after_eval` is false.
    pub eval: Option<String>,
    /// Optional CSS selector; when set, dumps run against the first match's
    /// subtree by temporarily replacing the dump root (CLI --selector).
    pub selector: Option<String>,
}

pub struct FetchOutput {
    pub html: String,
    pub text: String,
    pub links: String,
    pub markdown: String,
    pub assets: String,
    pub cookies: String,
    pub screenshot_png: Option<Vec<u8>>,
    /// Value of `eval` when the request carried one.
    pub eval_value: Option<String>,
}

/// Navigate a fresh kernel to `url` and run every dump expression against
/// the settled page. Blocking; call from a worker thread.
pub fn fetch_rendered(req: FetchRequest) -> Result<FetchOutput, String> {
    let (tx, rx) = channel::<Result<FetchOutput, String>>();
    let url = req.url.clone();
    let want_shot = req.screenshot;
    std::thread::Builder::new()
        .name("servo-fetch".into())
        .spawn(move || {
            let result = (|| -> Result<FetchOutput, String> {
                let servo = HeadlessServo::new((1280, 800))?;
                let deadline = Duration::from_secs(req.timeout_secs.max(1));
                servo.navigate(&url, deadline)?;
                let screenshot_png = if want_shot {
                    let (w, h, rgba) = servo.screenshot_rgba_blocking(Duration::from_secs(20));
                    let mut png = Vec::new();
                    let encoder = image::codecs::png::PngEncoder::new(std::io::Cursor::new(&mut png));
                    image::ImageEncoder::write_image(
                        encoder,
                        &rgba,
                        w,
                        h,
                        image::ExtendedColorType::Rgba8,
                    )
                    .map_err(|e| format!("png encode: {e}"))?;
                    Some(png)
                } else {
                    None
                };
                let eval_value = match &req.eval {
                    Some(expr) => Some(servo.evaluate_sync(expr, Duration::from_secs(30))?),
                    None => None,
                };
                // One round-trip for everything except markdown (its script
                // is separate); falls back to the individual dumps on error.
                let (html, text, links, assets, cookies) =
                    match crate::page_dumps::dump_all_json(&servo) {
                        Ok(json) => {
                            let v: serde_json::Value = serde_json::from_str(&json)
                                .unwrap_or(serde_json::Value::Null);
                            (
                                v["html"].as_str().unwrap_or_default().to_string(),
                                v["text"].as_str().unwrap_or_default().to_string(),
                                v["links"].as_str().unwrap_or_default().to_string(),
                                v["assets"].as_str().unwrap_or_default().to_string(),
                                v["cookies"].as_str().unwrap_or_default().to_string(),
                            )
                        },
                        Err(_) => (
                            dump_html(&servo)?,
                            dump_text(&servo)?,
                            dump_links(&servo)?,
                            dump_assets(&servo)?,
                            dump_cookies(&servo)?,
                        ),
                    };
                let markdown = dump_markdown(&servo)?;
                Ok(FetchOutput {
                    html,
                    text,
                    links,
                    markdown,
                    assets,
                    cookies,
                    screenshot_png,
                    eval_value,
                })
            })();
            let _ = tx.send(result);
        })
        .map_err(|e| format!("spawn fetch kernel: {e}"))?;
    rx.recv().map_err(|_| "fetch kernel died".to_string())?
}
