//! Bridge smoke: prove evaluate + real input pipeline work over the
//! headless kernel. Navigate → evaluate(change bg to blue) → screenshot
//! asserts blue; click a button via MouseButton events → evaluate asserts
//! the click handler ran.

use obscura_embedder::HeadlessServo;
use std::time::Duration;

fn main() {
    let servo = HeadlessServo::new((400, 300)).expect("boot");
    let url = "data:text/html,<html><body style='background:red' id='b'><button id='btn' onclick='window.clicked=(window.clicked||0)+1'>go</button></body></html>";
    let ok = servo.navigate(url, Duration::from_secs(30)).expect("navigate");
    assert!(ok, "load completed");

    // 1. evaluate: switch background to blue
    let r = servo.evaluate_sync(
        "document.body.style.background='blue'; document.body.style.background",
        Duration::from_secs(10),
    ).expect("evaluate 1");
    println!("evaluate returned: {r}");
    assert_eq!(r, "blue");

    servo.spin();
    servo.render_frame();
    let (_, _, rgba) = servo.read_back();
    let blue = rgba.chunks_exact(4).filter(|p| p[2] > 180 && p[0] < 100).count();
    let total = (rgba.len() / 4) as f64;
    println!("blue-pixel ratio: {:.3}", blue as f64 / total);
    assert!(blue as f64 / total > 0.5, "bg must now be blue");

    // 2. real input pipeline: click the button at its coordinates
    //    (button is at the top-left of body; click near 30, 20)
    servo.dispatch_mouse("mouseMoved", 30.0, 20.0);
    servo.dispatch_mouse("mousePressed", 30.0, 20.0);
    servo.dispatch_mouse("mouseReleased", 30.0, 20.0);
    for _ in 0..30 {
        servo.spin();
        servo.render_frame();
        std::thread::sleep(Duration::from_millis(16));
    }
    let clicks = servo.evaluate_sync("String(window.clicked||0)", Duration::from_secs(10))
        .expect("evaluate 2");
    println!("click count: {clicks}");
    assert_eq!(clicks, "1", "trusted click through the kernel must run the handler");

    println!("BRIDGE OK — evaluate + real input pipeline live");
}
