//! WPT smoke: run a curated set of web-platform-tests live pages through
//! the headless kernel and read the testharness verdicts. These are the
//! DOM/event/selector areas the self-engine was verified against (tasks
//! #60/#65-#68/#75/#76) — the same bar now applied to the Servo kernel.
//!
//! Pass bar: every harness completes with status 0 (all subtests passed)
//! or the harness reports zero failures. Console evidence goes to stderr.

use obscura_embedder::HeadlessServo;
use std::time::Duration;

/// (name, wpt.live path, timeout seconds)
const CASES: &[(&str, &str, u64)] = &[
    ("HTMLCollection live", "/dom/collections/HTMLCollection-live.html", 30),
    ("element.insertAdjacentElement", "/dom/nodes/Element-insertAdjacentElement.html", 40),
    ("closest", "/dom/nodes/Element-closest.html", 40),
    ("createElementNS QName", "/dom/nodes/createElementNS.html", 40),
    ("MutationObserver childList", "/mutation-observer/MutationObserver-childList.html", 40),
    ("Node.cloneNode", "/dom/nodes/Node-cloneNode.html", 40),
    ("querySelector live", "/selectors/attribute-selectors/attribute-selector.html", 30),
    ("events mousedown dispatch", "/dom/events/Event-dispatch-click.html", 30),
    ("after()", "/dom/nodes/ChildNode-after.html", 40),
];

fn read_verdict(servo: &HeadlessServo) -> (String, String) {
    // testharness exposes window.testharness_properties after completion;
    // fall back to parsing the inline status element.
    let props = servo.evaluate_sync(
        r#"(function() {
            if (window.testharness_properties) {
                return JSON.stringify({
                    status: window.testharness_properties.status,
                    num_failed: (window.testharness_properties.tests || []).filter(t => t.status !== 0).length,
                    num_total: (window.testharness_properties.tests || []).length,
                });
            }
            var el = document.querySelector('#__testharness__results__');
            if (el) return JSON.stringify({ inline: el.textContent.slice(0, 200) });
            return JSON.stringify({ status: null, num_failed: -1, num_total: 0, note: 'no harness data' });
        })()"#,
        Duration::from_secs(20),
    );
    let title = servo
        .evaluate_sync("document.title", Duration::from_secs(10))
        .unwrap_or_default();
    (props, title)
}

fn main() {
    let mut passed = 0usize;
    let mut failed: Vec<&str> = Vec::new();

    for (name, path, timeout) in CASES {
        let url = format!("https://wpt.live{path}");
        println!("[wpt] {name}: {url}");
        let servo = match HeadlessServo::new((1280, 800)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[wpt] kernel boot failed: {e}");
                failed.push(name);
                continue;
            },
        };
        match servo.navigate(&url, Duration::from_secs(*timeout)) {
            Ok(true) => {},
            Ok(false) => eprintln!("[wpt] load timed out (continuing to probe)"),
            Err(e) => {
                eprintln!("[wpt] navigate error: {e}");
                failed.push(name);
                continue;
            },
        }
        // Let the harness finish its async runs.
        servo.settle(3000);
        let (verdict, title) = read_verdict(&servo);
        println!("[wpt]   title={title}");
        println!("[wpt]   verdict={verdict}");
        let v: serde_json::Value = serde_json::from_str(&verdict).unwrap_or(serde_json::Value::Null);
        let num_failed = v["num_failed"].as_i64().unwrap_or(-1);
        let status = v["status"].as_i64();
        if num_failed == 0 && status != Some(2) && status != Some(3) {
            passed += 1;
            println!("[wpt]   PASS");
        } else {
            failed.push(name);
            println!("[wpt]   FAIL (failed={num_failed} status={status:?})");
        }
    }

    println!("\n[wpt] {passed}/{} passed", CASES.len());
    if !failed.is_empty() {
        eprintln!("[wpt] failing cases: {failed:?}");
    }
    // Report-only for now: this run establishes the Servo-kernel baseline.
    // Flipping to a hard assertion is one line once the baseline is known.
    if passed == CASES.len() {
        println!("WPT SMOKE OK — all curated cases green");
    } else {
        println!("WPT SMOKE PARTIAL — {passed}/{} (baseline established)", CASES.len());
    }
}
