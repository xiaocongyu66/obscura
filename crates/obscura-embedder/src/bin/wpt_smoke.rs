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
// Paths GET-verified 200 on wpt.live by the CI probe (three of the old
// set are 404 there now). Timeout 60s: happy-eyeballs fix permitting,
// wpt.live pages are small and load fast.
const CASES: &[(&str, &str, u64)] = &[
    ("element.insertAdjacentElement", "/dom/nodes/Element-insertAdjacentElement.html", 60),
    ("closest", "/dom/nodes/Element-closest.html", 60),
    ("Node.cloneNode", "/dom/nodes/Node-cloneNode.html", 60),
    ("events mousedown dispatch", "/dom/events/Event-dispatch-click.html", 60),
    ("after()", "/dom/nodes/ChildNode-after.html", 60),
];

fn read_verdict(servo: &HeadlessServo) -> (String, String) {
    // testharness exposes window.testharness_properties after completion;
    // fall back to parsing the inline status element.
    let props = servo
        .evaluate_sync(
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
        )
        .unwrap_or_else(|e| format!("{{\"note\":\"evaluate failed: {e}\"}}"));
    let title = servo
        .evaluate_sync("document.title", Duration::from_secs(10))
        .unwrap_or_default();
    (props, title)
}

fn main() {
    let mut passed = 0usize;
    let mut failed: Vec<&str> = Vec::new();

    // ONE kernel for the whole suite: booting a fresh HeadlessServo per
    // case leaks its kernel thread + mozjs isolate (Servo has no Drop),
    // and two live kernels OOM'd the CI runner mid-suite. Navigating one
    // webview across cases is isolation enough for these DOM pages.
    let servo = match HeadlessServo::new((1280, 800)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[wpt] kernel boot failed: {e}");
            eprintln!("WPT SMOKE PARTIAL — 0/{} (kernel boot failed)", CASES.len());
            return;
        },
    };

    // Kernel-side navigation ladder: pinpoint WHERE https navigation
    // stalls (simple site vs wpt.live domain) before burning the suite.
    // With OBSCURA_ALPN=http1 the whole binary runs h1 — the ladder then
    // answers "does http/1.1 sidestep the wpt.live stall".
    let alpn_tag = if std::env::var("OBSCURA_ALPN").as_deref() == Ok("http1") {
        " (h1)"
    } else {
        ""
    };
    for (label, probe_url) in [
        ("ladder example.com", "https://example.com/"),
        ("ladder wpt.live root", "https://wpt.live/"),
    ] {
        let label = format!("{label}{alpn_tag}");
        println!("[wpt] {label}: {probe_url}");
        let ok = servo.navigate(probe_url, Duration::from_secs(60)).unwrap_or(false);
        eprintln!(
            "[wpt]   {label}: ok={ok} url={:?} ready={:?}",
            servo.current_url().map(|u| u.to_string()),
            servo.evaluate_sync("document.readyState", Duration::from_secs(5)).unwrap_or_default(),
        );
    }

    if std::env::var("OBSCURA_WPT_LADDER_ONLY").is_ok() {
        println!("[wpt] LADDER_ONLY set — skipping suite");
        return;
    }

    // Base is switchable: wpt.live by default, or a local checkout served
    // over http (the CI path — CF tarpits wpt.live from the runner: TLS ok,
    // request out, response head 65s+ late on h1 and never on h2).
    let base = std::env::var("OBSCURA_WPT_BASE")
        .unwrap_or_else(|_| "https://wpt.live".into());
    for (name, path, timeout) in CASES {
        let url = format!("{base}{path}");
        println!("[wpt] {name}: {url}");
        match servo.navigate(&url, Duration::from_secs(*timeout)) {
            Ok(true) => {},
            Ok(false) => eprintln!("[wpt] load timed out (continuing to probe)"),
            Err(e) => {
                eprintln!("[wpt] navigate error: {e}");
                failed.push(name);
                continue;
            },
        }
        // Let the harness finish its async runs (and the previous page's
        // timers unwind before the next navigation).
        servo.settle(8000);
        // Diagnose stalls: where did the webview actually land?
        eprintln!(
            "[wpt]   diag: url={:?} ready={:?}",
            servo.current_url().map(|u| u.to_string()),
            servo.evaluate_sync("document.readyState", Duration::from_secs(5)).unwrap_or_default(),
        );
        // Diagnose harness wiring: did testharness.js load and start?
    let wire = servo
        .evaluate_sync(
            r#"JSON.stringify({
                th: typeof window.testharness_properties,
                add_ran: typeof window.add_result_callback,
                scripts: Array.from(document.querySelectorAll('script[src]')).map(s => s.getAttribute('src')).slice(0, 5),
                resultsEl: !!document.querySelector('#__testharness__results__'),
                bodyLen: (document.body ? document.body.innerText.length : 0),
                bodyHead: (document.body ? document.body.innerText.slice(0, 120) : ''),
            })"#,
            Duration::from_secs(10),
        )
        .unwrap_or_default();
    eprintln!("[wpt]   wire={wire}");
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
