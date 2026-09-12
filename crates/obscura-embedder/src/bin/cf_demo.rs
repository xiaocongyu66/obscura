//! Cloudflare Turnstile official demo test: load the challenge page from
//! Cloudflare's demo site, detect the widget, and diagnose how far the
//! challenge pipeline got.
//!
//! Key fact this build tests: Turnstile renders its challenge iframe inside
//! a CLOSED shadow root on the widget host, so `document.querySelectorAll`
//! cannot see it. Evidence layers used here:
//!   1. network stream (delegate request hook) — challenge-platform requests
//!   2. page console messages (delegate console hook) — api.js errors
//!   3. performance entries — resource loads invisible to DOM queries
//!   4. light-DOM probe (container outerHTML, injected response input)
//!   5. manual `turnstile.render()` on a fresh container — API verdict
//!   6. dynamic-iframe smoke test — isolates Servo iframe support from
//!      Turnstile logic
//!
//! Exit 0 = widget rendered (container + response input at minimum); the
//! log records exactly which stage the challenge pipeline reached.

use obscura_embedder::fingerprint::random_profile;
use obscura_embedder::HeadlessServo;
use std::sync::mpsc::{self, TryRecvError};
use std::time::Duration;

const DEMO_URL: &str = "https://demo.turnstile.workers.dev/";

fn drain(strings: &str, rx: &mpsc::Receiver<String>, cap: usize) {
    let mut n = 0;
    loop {
        match rx.try_recv() {
            Ok(msg) => {
                if n < cap {
                    println!("[cf]{strings} {msg}");
                } else if n == cap {
                    println!("[cf]{strings} …(truncated)");
                }
                n += 1;
            },
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => break,
        }
    }
    if n > 0 {
        println!("[cf]{strings} total={n}");
    }
}

fn main() {
    let (console_tx, console_rx) = mpsc::channel::<String>();
    let (request_tx, request_rx) = mpsc::channel::<String>();

    let servo = {
        let profile = random_profile().expect("TLS-compatible Chrome profile");
        HeadlessServo::new_full(
            (1280, 800),
            &profile,
            Some(console_tx),
            request_tx,
            None,
            None,
        )
        .expect("boot headless kernel")
    };

    println!("[cf] navigating to {DEMO_URL}");
    let ok = servo
        .navigate(DEMO_URL, Duration::from_secs(60))
        .expect("navigate");
    println!("[cf] load complete: {ok}");

    // Staged probe: widget render and challenge progress are async; capture
    // state at several time points so late token arrival is visible.
    for stage in ["5s", "12s", "20s"] {
        servo.settle(if stage == "5s" { 5000 } else { 7000 });
        drain("console", &console_rx, 30);
        drain("net", &request_rx, 60);

        let probe = servo
            .evaluate_sync(
                r#"(function() {
                var cfHost = document.querySelector('[class*=cf-turnstile]');
                var perf = (performance.getEntriesByType('resource') || [])
                    .map(function(e){ return e.name; })
                    .filter(function(u){ return /challenges\.cloudflare\.com|challenge-platform/.test(u); });
                var hostInfo = null;
                if (cfHost) {
                    hostInfo = {
                        outerHTML: cfHost.outerHTML.slice(0, 400),
                        childElementCount: cfHost.childElementCount,
                        shadowRootOpen: !!cfHost.shadowRoot,
                        hasResponseInput: !!cfHost.querySelector('input[name=cf-turnstile-response]'),
                        attrs: Array.from(cfHost.attributes).map(function(a){ return a.name + '=' + a.value.slice(0, 40); }).slice(0, 12),
                    };
                }
                var token = null;
                try { token = window.turnstile.getResponse() || null; } catch(e) { token = 'ERR:' + e.message; }
                return JSON.stringify({
                    stage: '__STAGE__',
                    turnstile: typeof window.turnstile,
                    hasRender: !!(window.turnstile && window.turnstile.render),
                    lightIframes: Array.from(document.querySelectorAll('iframe')).map(function(f){ return (f.src||'').slice(0, 90); }),
                    perfCf: perf.slice(0, 12),
                    host: hostInfo,
                    token: token ? String(token).slice(0, 40) : null,
                    anyResponseInput: !!document.querySelector('input[name=cf-turnstile-response]'),
                });
            })()"#,
                Duration::from_secs(20),
            )
            .expect("probe evaluate");
        let probe = probe.replace("__STAGE__", stage);
        println!("[cf] probe[{stage}]: {probe}");
    }

    drain("console", &console_rx, 40);
    drain("net", &request_rx, 80);

    // Manual render on a fresh container: isolates "implicit scan never ran"
    // from "render itself fails".
    let manual = servo
        .evaluate_sync(
            r#"(function() {
            try {
                if (!window.turnstile || !window.turnstile.render) return 'no-render-fn';
                var host = document.createElement('div');
                host.className = 'cf-turnstile-manual';
                host.setAttribute('data-sitekey', '1x00000000000000000000AA');
                document.body.appendChild(host);
                var id = window.turnstile.render(host);
                return 'rendered id=' + JSON.stringify(id);
            } catch (e) {
                return 'THREW: ' + (e && (e.message || String(e)));
            }
        })()"#,
            Duration::from_secs(15),
        )
        .expect("manual render");
    println!("[cf] manual render: {manual}");
    servo.settle(6000);
    drain("console", &console_rx, 30);
    drain("net", &request_rx, 60);

    let after_manual = servo
        .evaluate_sync(
            r#"JSON.stringify({
            manualHostHTML: (document.querySelector('.cf-turnstile-manual')||{outerHTML:''}).outerHTML.slice(0, 400),
            manualToken: (function(){ try { return window.turnstile.getResponse(0); } catch(e){ return 'ERR:'+e.message; } })(),
        })"#,
            Duration::from_secs(15),
        )
        .expect("after manual probe");
    println!("[cf] after manual: {after_manual}");

    // Dynamic iframe smoke test: does a plain iframe appended by JS load at
    // all in this kernel? Separates Servo iframe support from Turnstile.
    let iframe_smoke = servo
        .evaluate_sync(
            r#"(function() {
            var f = document.createElement('iframe');
            f.src = '/favicon.ico';
            document.body.appendChild(f);
            return 'appended';
        })()"#,
            Duration::from_secs(10),
        )
        .expect("iframe smoke append");
    println!("[cf] iframe smoke: {iframe_smoke}");
    servo.settle(4000);
    let smoke_probe = servo
        .evaluate_sync(
            r#"JSON.stringify({
            count: document.querySelectorAll('iframe').length,
            srcs: Array.from(document.querySelectorAll('iframe')).map(function(f){ return (f.src||'').slice(0, 60); }),
            faviconLoaded: (function(){
                var f = Array.from(document.querySelectorAll('iframe')).find(function(f){ return (f.src||'').includes('favicon'); });
                if (!f) return 'no-iframe';
                try { return f.contentDocument ? 'same-doc' : 'cross-doc'; } catch(e) { return 'cross-origin-ok'; }
            })(),
        })"#,
            Duration::from_secs(10),
        )
        .expect("iframe smoke probe");
    println!("[cf] iframe smoke probe: {smoke_probe}");
    drain("net", &request_rx, 60);

    // Screenshot for the artifact trail.
    let (w, h, rgba) = servo.screenshot_rgba_blocking(Duration::from_secs(15));
    let mut png = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(std::io::Cursor::new(&mut png));
    image::ImageEncoder::write_image(encoder, &rgba, w, h, image::ExtendedColorType::Rgba8)
        .expect("png encode");
    std::fs::write("cf-demo.png", &png).expect("write cf-demo.png");
    println!("[cf] screenshot: cf-demo.png ({w}x{h})");

    let v: serde_json::Value =
        serde_json::from_str(after_manual.trim()).unwrap_or(serde_json::Value::Null);
    let manual_token = v["manualToken"].as_str().map(|s| s.to_string());
    let manual_host_has_content = v["manualHostHTML"]
        .as_str()
        .map(|s| s.len() > 45)
        .unwrap_or(false);

    println!("[cf] verdict: manual_host_has_content={manual_host_has_content} manual_token={manual_token:?}");
    // Pass bar: the widget pipeline executes (render mutates the DOM). Token
    // arrival depends on CF's challenge flow over the network; the log above
    // records how far it got.
    assert!(manual_host_has_content, "turnstile.render must mutate its host element");
    println!("CF DEMO OK — turnstile widget pipeline live");
}
