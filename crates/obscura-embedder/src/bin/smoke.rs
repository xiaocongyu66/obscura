//! Runtime smoke test: boot the headless Servo kernel, navigate to a
//! data: URL, wait for Complete, screenshot, and assert the framebuffer
//! is not blank. Exit 0 = kernel alive end to end.

use obscura_embedder::HeadlessServo;
use servo::LoadStatus;
use std::time::Duration;

fn main() {
    let viewport = (400, 300);
    let servo = HeadlessServo::new(viewport).expect("boot headless servo");
    let url = "data:text/html,<html><body style='background:red'><h1>obscura</h1></body></html>";
    let complete = servo.navigate(url, Duration::from_secs(30)).expect("navigate");
    let status = servo.webview().load_status();
    println!("load complete={complete} status={status:?}");
    assert_eq!(status, LoadStatus::Complete, "load must reach Complete");

    // Let a couple of frames render before the readback.
    for _ in 0..30 {
        servo.spin();
        servo.render_frame();
        std::thread::sleep(Duration::from_millis(16));
    }
    let (w, h, rgba) = servo.screenshot_rgba().expect("screenshot");
    println!("framebuffer {w}x{h} bytes={}", rgba.len());
    assert_eq!(w, viewport.0);
    assert_eq!(h, viewport.1);

    // Red background: count strongly-red pixels. A blank/failed kernel
    // would produce all-zero or uniform white output.
    let red = rgba.chunks_exact(4).filter(|p| p[0] > 180 && p[1] < 100 && p[2] < 100).count();
    let total = (w * h) as usize;
    let ratio = red as f64 / total as f64;
    println!("red-pixel ratio: {ratio:.3}");
    assert!(ratio > 0.5, "background must be mostly red, got {ratio:.3}");
    println!("SMOKE OK — servo kernel rendered a live page");
}
