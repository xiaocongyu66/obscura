//! Runtime smoke test: boot the headless Servo kernel, navigate to a
//! data: URL, wait for Complete, take an official screenshot (which waits
//! for rendering up-to-date), and assert the page background made it into
//! the pixels. Exit 0 = kernel alive end to end.

use obscura_embedder::HeadlessServo;
use servo::LoadStatus;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

fn main() {
    // logging is set up by the Servo instance itself (needs its channels)
    let viewport = (400, 300);
    let servo = HeadlessServo::new(viewport).expect("boot headless servo");
    let url = "data:text/html,<html><body style='background:red'><h1>obscura</h1></body></html>";
    let complete = servo.navigate(url, Duration::from_secs(30)).expect("navigate");
    let status = servo.webview().load_status();
    println!("load complete={complete} status={status:?}");
    assert_eq!(status, LoadStatus::Complete, "load must reach Complete");

    // Drive the compositor while waiting for the official screenshot —
    // take_screenshot waits for rendering-up-to-date, and the compositor
    // only makes progress while the embedder pumps.
    let result: Rc<Cell<Option<image::RgbaImage>>> = Rc::new(Cell::new(None));
    let slot = result.clone();
    servo.webview().take_screenshot(None, move |res| {
        match res {
            Ok(img) => slot.set(Some(img)),
            Err(e) => eprintln!("screenshot error: {e:?}"),
        }
    });
    let img = loop {
        servo.spin();
        servo.render_frame();
        if let Some(img) = result.take() {
            // Diagnose both buffer orders: read back BEFORE present too.
            let pre = servo.read_back();
            let mut uniq = std::collections::HashSet::new();
            for p in pre.2.chunks_exact(4) {
                uniq.insert((p[0] / 32, p[1] / 32, p[2] / 32, p[3] / 32));
            }
            println!("pre-present readback: {}x{} uniq-colors={} first={:?}",
                pre.0, pre.1, uniq.len(), &pre.2[..8]);
            break img;
        }
        servo.present();
        std::thread::sleep(Duration::from_millis(16));
    };
    let (w, h) = (img.width(), img.height());
    let rgba = img.into_raw();
    println!("screenshot {w}x{h} bytes={}", rgba.len());
    assert_eq!(w, viewport.0);
    assert_eq!(h, viewport.1);

    let red = rgba.chunks_exact(4).filter(|p| p[0] > 180 && p[1] < 100 && p[2] < 100).count();
    let total = (w * h) as usize;
    let ratio = red as f64 / total as f64;
    println!("red-pixel ratio: {ratio:.3}");
    println!("first-pixel: {:?}", &rgba[..12]);
    assert!(ratio > 0.5, "background must be mostly red, got {ratio:.3}");
    println!("SMOKE OK — servo kernel rendered a live page");
}
