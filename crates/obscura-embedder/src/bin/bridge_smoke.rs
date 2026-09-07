//! Bridge smoke: prove evaluate + real input pipeline work over the
//! headless kernel. Navigate → evaluate(change bg to blue) → screenshot
//! asserts blue; click a button via MouseButton events → evaluate asserts
//! the click handler ran.

use obscura_embedder::HeadlessServo;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

fn main() {
    let servo = HeadlessServo::new((400, 300)).expect("boot");
    let url = "data:text/html,<html><body style='background:red' id='b'><button id='btn' onclick='window.clicked=(window.clicked||0)+1'>go</button></body></html>";
    let ok = servo.navigate(url, Duration::from_secs(30)).expect("navigate");
    assert!(ok, "load completed");

    // 0. baseline screenshot BEFORE touching anything: should be red.
    let pre = servo.screenshot_rgba_blocking(Duration::from_secs(15));
    println!("pre-eval first-pixel: {:?}", &pre[..8]);
    let pre_red = pre.chunks_exact(4).filter(|p| p[0] > 180 && p[1] < 100).count();
    println!("pre-eval red ratio: {:.3}", pre_red as f64 / (pre.len() / 4) as f64);

    // 1. evaluate: switch background to blue
    let r = servo.evaluate_sync(
        "document.body.style.background='blue'; document.body.style.background",
        Duration::from_secs(10),
    ).expect("evaluate 1");
    println!("evaluate returned: {r}");
    assert_eq!(r, "blue");

    // take_screenshot is the official readback: it waits for the compositor
    // to be rendering-up-to-date (manual read_to_image races the swap chain
    // and intermittently returns the clear color).
    let result: Rc<RefCell<Option<image::RgbaImage>>> = Rc::new(RefCell::new(None));
    let slot = result.clone();
    servo.webview().take_screenshot(None, move |res| match res {
        Ok(img) => *slot.borrow_mut() = Some(img),
        Err(e) => eprintln!("screenshot error: {e:?}"),
    });
    let img = loop {
        servo.spin();
        servo.render_frame();
        if let Some(img) = result.borrow_mut().take() {
            break img;
        }
        std::thread::sleep(Duration::from_millis(16));
    };
    let rgba = img.into_raw();
    let blue = rgba.chunks_exact(4).filter(|p| p[2] > 180 && p[0] < 100).count();
    let total = (rgba.len() / 4) as f64;
    println!("blue-pixel ratio: {:.3}", blue as f64 / total);
    assert!(blue as f64 / total > 0.5, "bg must now be blue");

    // 2. real input pipeline: click the button at its coordinates
    let r = servo.evaluate_sync(
        "(function(){
  window.__evts = [];
  for (const t of ['mousemove','mousedown','mouseup','click']) {
    document.addEventListener(t, function(e){ window.__evts.push(t+'@'+e.clientX+','+e.clientY+'->'+(e.target.id||e.target.tagName)+'|trusted='+e.isTrusted); }, true);
  }
  var el = document.elementFromPoint(30, 20);
  var b = document.getElementById('btn').getBoundingClientRect();
  return JSON.stringify({hit: el ? el.tagName + '/' + (el.id||'') : null, btn: [b.x, b.y, b.width, b.height]});
})()",
        Duration::from_secs(10),
    ).expect("diagnostic eval");
    println!("hit-test: {r}");
    servo.dispatch_mouse("mouseMoved", 30.0, 20.0);
    servo.dispatch_mouse("mousePressed", 30.0, 20.0);
    servo.dispatch_mouse("mouseReleased", 30.0, 20.0);
    for _ in 0..30 {
        servo.spin();
        servo.render_frame();
        std::thread::sleep(Duration::from_millis(16));
    }
    let evts = servo.evaluate_sync("JSON.stringify(window.__evts||[])", Duration::from_secs(10))
        .expect("events eval");
    println!("captured events: {evts}");
    let clicks = servo.evaluate_sync("String(window.clicked||0)", Duration::from_secs(10))
        .expect("evaluate 2");
    println!("click count: {clicks}");
    assert_eq!(clicks, "1", "trusted click through the kernel must run the handler");

    println!("BRIDGE OK — evaluate + real input pipeline live");
}
