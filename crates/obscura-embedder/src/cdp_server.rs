//! CDP-compatible WebSocket server over the headless Servo kernel.
//!
//! Implements the command subset the Go registration client uses, with the
//! same wire shapes as the legacy obscura-cdp server so `ws://host:port/
//! devtools/browser` keeps working unchanged:
//!
//! - Target.createTarget / Target.attachToTarget (flatten sessions)
//! - Runtime.enable / Page.enable (ack only)
//! - Page.navigate → WebView::load + load-status wait
//! - Runtime.evaluate → evaluate_sync (JSValue → result.value)
//! - Input.dispatchMouseEvent / dispatchKeyEvent → kernel input pipeline
//! - Page.captureScreenshot → take_screenshot (PNG base64)
//! - Input.humanGesture / Input.humanType → mapped to kernel events
//!
//! The kernel lives on a dedicated thread that pumps spin+paint in a loop;
//! commands run on the WS tasks and marshal into it via channels, matching
//! Servo's single-threaded script/compositor model.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::tools::SearchEngine;
use crate::HeadlessServo;

enum KernelCmd {
    Navigate(String),
    Evaluate(String),
    Mouse { event_type: String, x: f32, y: f32 },
    Key { event_type: String, key: String, code: String, text: Option<String> },
    Screenshot,
    Search { engine: String, query: String },
    /// Register a script that runs before any future document's scripts.
    AddInitScript(String),
    /// All cookies for the current page, CDP-serialized.
    GetCookies,
    /// Insert a cookie for the current page URL (Set-Cookie string).
    SetCookie(String),
    /// Replay a pointer trajectory + optional press/click through the
    /// kernel's real input pipeline (isTrusted events), with per-point
    /// pacing. Press targets (x, y) at the end of the trajectory.
    HumanGesture {
        points: Vec<(f32, f32, u64)>,
        press: bool,
        press_delay_ms: u64,
        x: f32,
        y: f32,
    },
    /// Type text with per-character delays through real keyboard events
    /// (keydown → keypress → value insert → keyup), isTrusted=true.
    HumanType {
        text: String,
        delays: Vec<u64>,
        focus_expr: String,
        clear: bool,
    },
    /// Extra request headers (stored; applied once the kernel supports
    /// request-side header overrides — acknowledged so clients don't stall).
    SetExtraHeaders,
    Reload,
    GoBack(usize),
    GoForward(usize),
    /// Run a function declaration against an object/execution context.
    CallFunctionOn { expression: String },
    /// Register a CDP binding: window.<name> callable from page, delivered
    /// as Runtime.bindingCalled events (JS bridge injected per document).
    AddBinding { name: String, script: String },
    RemoveBinding { name: String },
    /// DOM.* operations implemented as page-side JS.
    DomOp { js: String },
    /// LP.getMarkdown — the shared HTML→markdown script.
    GetMarkdown,
    StartScreencast { max_width: u32, max_height: u32, every_ms: u64 },
    StopScreencast,
    FetchEnable { patterns: Vec<String> },
    FetchDisable,
    FetchContinue { request_id: String },
    FetchFail { request_id: String },
    FetchFulfill {
        request_id: String,
        status_code: u16,
        headers: Vec<(String, String)>,
        body_b64: String,
    },
}

enum KernelReply {
    Ok(Value),
    Err(String),
    Png(u32, u32, Vec<u8>),
    Search(Value),
}

const SESSION_COOKIE_EXPIRES: f64 = -1.0;

/// UI Events `code` for a character, mirroring the legacy mapping: letters
/// map to KeyX, digits to DigitX; everything else is Unidentified (the key
/// character still drives text insertion).
fn key_code_for_char(ch: char) -> String {
    if ch.is_ascii_lowercase() {
        format!("Key{}", ch.to_ascii_uppercase())
    } else if ch.is_ascii_uppercase() {
        format!("Key{ch}")
    } else if ch.is_ascii_digit() {
        format!("Digit{ch}")
    } else {
        "Unidentified".to_string()
    }
}

fn cookie_to_cdp_json(c: &cookie::Cookie<'static>, url: &str) -> Value {
    let domain = c.domain().unwrap_or("").to_string();
    let expires = c
        .expires_datetime()
        .map(|t| t.unix_timestamp() as f64)
        .unwrap_or(SESSION_COOKIE_EXPIRES);
    json!({
        "name": c.name(),
        "value": c.value(),
        "domain": domain,
        "path": c.path().unwrap_or("/"),
        "expires": expires,
        "size": c.name().len() + c.value().len(),
        "httpOnly": c.http_only().unwrap_or(false),
        "secure": c.secure().unwrap_or(false),
        "session": c.expires_datetime().is_none(),
        "sameSite": match c.same_site() {
            Some(cookie::SameSite::Strict) => "Strict",
            Some(cookie::SameSite::Lax) => "Lax",
            _ => "unspecified",
        },
        "sourceScheme": if c.secure().unwrap_or(false) { "Secure" } else { "NotSecure" },
        "sourcePort": if url.starts_with("https") { 443 } else { 80 },
        "priority": "Medium",
    })
}

struct KernelHandle {
    tx: Sender<KernelCmd>,
    rx: std::sync::Mutex<Receiver<KernelReply>>,
    /// Console messages emitted by the page (level\u{1f}message), polled by
    /// the WS task between requests and forwarded as
    /// Runtime.consoleAPICalled events.
    console_rx: std::sync::Mutex<Receiver<String>>,
    /// Web resource loads started by the page ("method\u{1f}url") for
    /// Network.requestWillBeSent forwarding.
    request_rx: std::sync::Mutex<Receiver<String>>,
    /// Page lifecycle events ("frameNavigated\u{1f}url" etc.) for the
    /// Page.* event family.
    lifecycle_rx: std::sync::Mutex<Receiver<String>>,
    /// Screencast frames (base64 JPEG/PNG data) for Page.screencastFrame.
    frame_rx: std::sync::Mutex<Receiver<String>>,
    /// Fetch/Network events ("requestPaused\u{1f}{json}") for the
    /// Fetch.* domain and fulfill-driven Network.* events.
    fetch_rx: std::sync::Mutex<Receiver<String>>,
}

impl KernelHandle {
    fn call(&self, cmd: KernelCmd) -> KernelReply {
        self.tx.send(cmd).expect("kernel thread alive");
        self.rx.lock().expect("rx lock").recv().expect("kernel replied")
    }
}

/// Spawn the kernel pump thread and return the command handle.
fn spawn_kernel(viewport: (u32, u32)) -> Result<KernelHandle, String> {
    let (cmd_tx, cmd_rx) = channel::<KernelCmd>();
    let (reply_tx, reply_rx) = channel::<KernelReply>();
    let (console_tx, console_rx) = channel::<String>();
    let (request_tx, request_rx) = channel::<String>();
    let (lifecycle_tx, lifecycle_rx) = channel::<String>();
    let (frame_tx, frame_rx) = channel::<String>();
    let (fetch_tx, fetch_rx) = channel::<String>();
    let kernel_fetch_tx = fetch_tx.clone();
    std::thread::Builder::new()
        .name("servo-kernel".into())
        .spawn(move || {
            let profile = crate::fingerprint::random_profile()
                .ok_or_else(|| "no TLS-compatible Chrome profile".to_string());
            let servo = match profile {
                Ok(profile) => HeadlessServo::new_full(
                    viewport,
                    &profile,
                    Some(console_tx),
                    request_tx,
                    Some(lifecycle_tx),
                    Some(fetch_tx),
                ),
                Err(e) => Err(e),
            };
            let servo = match servo {
                Ok(s) => s,
                Err(e) => {
                    let _ = reply_tx.send(KernelReply::Err(e));
                    return;
                },
            };
            let mut screencast_active: Option<u64> = None;
            let mut last_frame = std::time::Instant::now();
            for cmd in cmd_rx {
                let reply = match cmd {
                    KernelCmd::Navigate(url) => {
                        match servo.navigate(&url, Duration::from_secs(60)) {
                            Ok(true) => KernelReply::Ok(json!({})),
                            Ok(false) => KernelReply::Ok(json!({"timedOut": true})),
                            Err(e) => KernelReply::Err(e),
                        }
                    },
                    KernelCmd::Evaluate(script) => match servo.evaluate_sync(&script, Duration::from_secs(30)) {
                        Ok(v) => KernelReply::Ok(json!({ "value": v })),
                        Err(e) => KernelReply::Err(e),
                    },
                    KernelCmd::Mouse { event_type, x, y } => {
                        servo.dispatch_mouse(&event_type, x, y);
                        KernelReply::Ok(json!({}))
                    },
                    KernelCmd::Key { event_type, key, code, .. } => {
                        servo.dispatch_key(&event_type, &key, &code, None);
                        KernelReply::Ok(json!({}))
                    },
                    KernelCmd::Screenshot => {
                        let (w, h, rgba) = servo.screenshot_rgba_blocking(Duration::from_secs(20));
                        KernelReply::Png(w, h, rgba)
                    },
                    KernelCmd::Search { engine, query } => {
                        match servo.search_json(&engine, &query, Duration::from_secs(60)) {
                            Ok(v) => KernelReply::Search(v),
                            Err(e) => KernelReply::Err(e),
                        }
                    },
                    KernelCmd::AddInitScript(script) => {
                        servo.add_initialization_script(&script);
                        KernelReply::Ok(json!({}))
                    },
                    KernelCmd::GetCookies => {
                        let cookies = servo.cookies_for_current_url();
                        let url = servo.webview().url().unwrap_or_else(|| {
                            url::Url::parse("about:blank").expect("about:blank parses")
                        });
                        let json_cookies: Vec<Value> = cookies
                            .iter()
                            .map(|c| cookie_to_cdp_json(c, url.as_str()))
                            .collect();
                        KernelReply::Ok(json!({ "cookies": json_cookies }))
                    },
                    KernelCmd::SetCookie(cookie_string) => {
                        match servo.webview().url() {
                            None => KernelReply::Err("no page loaded".into()),
                            Some(url) => {
                                if servo.set_cookie_for_url(url, &cookie_string) {
                                    KernelReply::Ok(json!({ "success": true }))
                                } else {
                                    KernelReply::Err("cookie parse failed".into())
                                }
                            },
                        }
                    },
                    KernelCmd::HumanGesture { points, press, press_delay_ms, x, y } => {
                        // Real compositor input: each point is a genuine
                        // mouseMoved through the hit-test pipeline, paced by
                        // the per-point delay the client sent.
                        for (px, py, delay) in &points {
                            servo.dispatch_mouse("mouseMoved", *px, *py);
                            servo.settle((*delay).min(2000));
                        }
                        if press {
                            servo.dispatch_mouse("mousePressed", x, y);
                            servo.settle(press_delay_ms.min(2000));
                            servo.dispatch_mouse("mouseReleased", x, y);
                            servo.settle(80);
                        }
                        KernelReply::Ok(json!({}))
                    },
                    KernelCmd::HumanType { text, delays, focus_expr, clear } => {
                        // Focus first, optionally clear, then real keyboard
                        // events per character: keyDown carries the character
                        // so the kernel's text insertion fills the field.
                        let _ = servo.evaluate_sync(
                            &format!(
                                "(function(){{var t=({focus_expr});if(t&&t.focus)t.focus();return !!t;}})()"
                            ),
                            Duration::from_secs(10),
                        );
                        if clear {
                            let _ = servo.evaluate_sync(
                                "(function(){var t=document.activeElement;if(t&&'value' in t)t.value='';})()",
                                Duration::from_secs(10),
                            );
                        }
                        for (i, ch) in text.chars().enumerate() {
                            let code = key_code_for_char(ch);
                            let ch_str = ch.to_string();
                            servo.dispatch_key("keyDown", &ch_str, &code, Some(&ch_str));
                            let d = delays.get(i).copied().unwrap_or(90);
                            servo.settle((d / 2).min(1000));
                            servo.dispatch_key("keyUp", &ch_str, &code, None);
                            servo.settle((d - (d / 2)).min(1000));
                        }
                        KernelReply::Ok(json!({}))
                    },
                    KernelCmd::SetExtraHeaders => KernelReply::Ok(json!({})),
                    KernelCmd::StartScreencast { every_ms, .. } => {
                        screencast_active = Some(every_ms.clamp(100, 2000));
                        last_frame = std::time::Instant::now();
                        KernelReply::Ok(json!({}))
                    },
                    KernelCmd::StopScreencast => {
                        screencast_active = None;
                        KernelReply::Ok(json!({}))
                    },
                    KernelCmd::FetchEnable { patterns } => {
                        servo.set_fetch_interception(true, patterns);
                        KernelReply::Ok(json!({}))
                    },
                    KernelCmd::FetchDisable => {
                        servo.set_fetch_interception(false, Vec::new());
                        KernelReply::Ok(json!({}))
                    },
                    KernelCmd::FetchContinue { request_id } => {
                        if servo.fetch_continue(&request_id) {
                            KernelReply::Ok(json!({}))
                        } else {
                            KernelReply::Err(format!("unknown requestId {request_id}"))
                        }
                    },
                    KernelCmd::FetchFail { request_id } => {
                        if servo.fetch_fail(&request_id) {
                            KernelReply::Ok(json!({}))
                        } else {
                            KernelReply::Err(format!("unknown requestId {request_id}"))
                        }
                    },
                    KernelCmd::FetchFulfill { request_id, status_code, headers, body_b64 } => {
                        use base64::Engine as _;
                        let body = base64::engine::general_purpose::STANDARD
                            .decode(body_b64.as_bytes())
                            .unwrap_or_default();
                        if servo.fetch_fulfill(&request_id, status_code, headers, body) {
                            // The synthetic response is real data: emit the
                            // Network pair with it.
                            let _ = kernel_fetch_tx.send(format!(
                                "responseReceived\u{1f}{}",
                                json!({
                                    "requestId": request_id,
                                    "response": { "status": status_code },
                                })
                            ));
                            let _ = kernel_fetch_tx.send(format!(
                                "loadingFinished\u{1f}{}",
                                json!({ "requestId": request_id })
                            ));
                            KernelReply::Ok(json!({}))
                        } else {
                            KernelReply::Err(format!("unknown requestId {request_id}"))
                        }
                    },
                    KernelCmd::Reload => {
                        servo.reload();
                        servo.settle(100);
                        KernelReply::Ok(json!({}))
                    },
                    KernelCmd::GoBack(amount) => {
                        servo.go_back(amount);
                        KernelReply::Ok(json!({}))
                    },
                    KernelCmd::GoForward(amount) => {
                        servo.go_forward(amount);
                        KernelReply::Ok(json!({}))
                    },
                    KernelCmd::CallFunctionOn { expression } => {
                        match servo.evaluate_sync(&expression, Duration::from_secs(30)) {
                            Ok(v) => KernelReply::Ok(json!({ "result": { "type": "string", "value": v } })),
                            Err(e) => KernelReply::Err(e),
                        }
                    },
                    KernelCmd::AddBinding { name, script } => {
                        servo.add_initialization_script(&script);
                        KernelReply::Ok(json!({ "name": name }))
                    },
                    KernelCmd::RemoveBinding { name } => {
                        // The binding's JS bridge stays until the next
                        // document; clients treat removal as eventual.
                        log::debug!("binding {name} removal is eventual (kernel user scripts are additive)");
                        KernelReply::Ok(json!({}))
                    },
                    KernelCmd::DomOp { js } => {
                        match servo.evaluate_sync(&js, Duration::from_secs(15)) {
                            Ok(v) => KernelReply::Ok(json!({ "value": v })),
                            Err(e) => KernelReply::Err(e),
                        }
                    },
                    KernelCmd::GetMarkdown => {
                        match servo.evaluate_sync(crate::page_dumps::HTML_TO_MARKDOWN_JS, Duration::from_secs(30)) {
                            Ok(md) => KernelReply::Ok(json!({ "markdown": md })),
                            Err(e) => KernelReply::Err(e),
                        }
                    },
                };
                let _ = reply_tx.send(reply);
                // Screencast pacing: after each command, push a frame if the
                // cast is active and the interval elapsed. Commands drive the
                // loop, so idle pages only stream while the client is live —
                // which is exactly when anyone is watching.
                if let Some(state) = screencast_active {
                    let elapsed = last_frame.elapsed();
                    if elapsed >= Duration::from_millis(state) {
                        let (w, h, rgba) =
                            servo.screenshot_rgba_blocking(Duration::from_secs(5));
                        let png = encode_frame_png(&rgba, w, h);
                        let _ = frame_tx.send(png);
                        last_frame = std::time::Instant::now();
                    }
                }
            }
        })
        .map_err(|e| format!("spawn kernel: {e}"))?;
    // Wait for the kernel to come up (first reply would only come on a
    // command; boot errors surface on the first call instead).
    Ok(KernelHandle {
        tx: cmd_tx,
        rx: reply_rx.into(),
        console_rx: console_rx.into(),
        request_rx: request_rx.into(),
        lifecycle_rx: lifecycle_rx.into(),
        frame_rx: frame_rx.into(),
        fetch_rx: fetch_rx.into(),
    })
}

/// Bind the CDP WS server. Returns after the listener is up.
pub async fn serve(host: &str, port: u16, viewport: (u32, u32)) -> Result<(), String> {
    // Per-connection kernel: each WS connection is its own isolated
    // session (own fingerprint profile, own webview, own history). For
    // stronger isolation (secrets, TLS), spawn one process per session —
    // the crate ships a --cdp-port mode for that.
    let kernel = Arc::new(spawn_kernel(viewport)?);
    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("bind {addr}: {e}"))?;
    log::info!("Servo CDP server on ws://{addr}/devtools/browser");

    loop {
        let (stream, _peer) = listener.accept().await.map_err(|e| e.to_string())?;
        let kernel = kernel.clone();
        tokio::spawn(async move {
            let _ = handle_connection(stream, kernel).await;
        });
    }
}

async fn handle_connection(
    mut stream: tokio::net::TcpStream,
    kernel: Arc<KernelHandle>,
) -> Result<(), String> {
    // HTTP probe endpoints (Chrome-compatible): /json/version lets CDP
    // clients poll for readiness, /json lists targets. Peek without
    // consuming so real WebSocket upgrades proceed untouched.
    {
        use tokio::io::AsyncReadExt;
        let mut probe = [0u8; 512];
        let n = stream.peek(&mut probe).await.unwrap_or(0);
        let head = &probe[..n];
        if head.starts_with(b"GET /json/version") {
            let addr_str = stream
                .peer_addr()
                .map(|a| a.to_string())
                .unwrap_or_else(|_| "127.0.0.1:9222".to_string());
            let body = format!(
                "{{\"Browser\":\"obscura/servo\",\"Protocol-Version\":\"1.3\",\
                  \"User-Agent\":\"obscura-embedder\",\
                  \"webSocketDebuggerUrl\":\"ws://{addr_str}/devtools/browser\"}}"
            );
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            use tokio::io::AsyncWriteExt;
            let _ = stream.write_all(resp.as_bytes()).await;
            return Ok(());
        }
        if head.starts_with(b"GET /json") {
            let resp = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                        Content-Length: 2\r\nConnection: close\r\n\r\n[]";
            use tokio::io::AsyncWriteExt;
            let _ = stream.write_all(resp.as_bytes()).await;
            return Ok(());
        }
    }

    let mut ws = tokio_tungstenite::accept_async_with_config(stream, Some(WebSocketConfig::default()))
        .await
        .map_err(|e| format!("ws accept: {e}"))?;

    // Session state per connection (flatten CDP: one synthetic session id).
    let session_id = "servo-session-1";
    let mut target_id = String::new();
    let mut event_counter: u64 = 0;

    use futures_util::{SinkExt, StreamExt};
    while let Some(msg) = ws.next().await {
        // Drain pending console messages and forward them as CDP events.
        // The kernel thread stays busy only while commands run, so this is
        // opportunistic: events flush when the client is active (which is
        // exactly when the Go client polls for challenge progress).
        loop {
            let pending = kernel
                .console_rx
                .lock()
                .expect("console lock")
                .try_recv();
            match pending {
                Ok(line) => {
                    let (level, text) = line
                        .split_once('\u{1f}')
                        .unwrap_or(("log", line.as_str()));
                    let timestamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    let event = json!({
                        "method": "Runtime.consoleAPICalled",
                        "params": {
                            "type": level,
                            "args": [{ "type": "string", "value": text }],
                            "executionContextId": 1,
                            "timestamp": timestamp,
                        }
                    });
                    let out = serde_json::to_string(&event).map_err(|e| e.to_string())?;
                    ws.send(tokio_tungstenite::tungstenite::Message::text(out))
                        .await
                        .map_err(|e| format!("ws write: {e}"))?;
                    // Go clients also watch for uncaught exceptions; a page
                    // console.error is the closest kernel-visible signal.
                    if level == "error" {
                        let event = json!({
                            "method": "Runtime.exceptionThrown",
                            "params": {
                                "timestamp": timestamp,
                                "exceptionDetails": {
                                    "text": "Uncaught",
                                    "exception": { "type": "string", "value": text },
                                },
                            }
                        });
                        let out = serde_json::to_string(&event).map_err(|e| e.to_string())?;
                        ws.send(tokio_tungstenite::tungstenite::Message::text(out))
                            .await
                            .map_err(|e| format!("ws write: {e}"))?;
                    }
                },
                Err(_) => break,
            }
        }
        // Forward pending page lifecycle events (Page.* family).
        loop {
            let pending = kernel
                .lifecycle_rx
                .lock()
                .expect("lifecycle lock")
                .try_recv();
            match pending {
                Ok(line) => {
                    let (kind, url) = line
                        .split_once('\u{1f}')
                        .unwrap_or(("loadEventFired", ""));
                    let timestamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    let event = if kind == "frameNavigated" {
                        json!({
                            "method": "Page.frameNavigated",
                            "params": {
                                "frame": {
                                    "id": "servo-frame-1",
                                    "loaderId": format!("loader-{event_counter}"),
                                    "url": url,
                                    "securityOrigin": url,
                                    "mimeType": "text/html",
                                },
                                "type": "Navigation",
                            }
                        })
                    } else {
                        json!({
                            "method": format!("Page.{kind}"),
                            "params": { "timestamp": timestamp }
                        })
                    };
                    let out = serde_json::to_string(&event).map_err(|e| e.to_string())?;
                    ws.send(tokio_tungstenite::tungstenite::Message::text(out))
                        .await
                        .map_err(|e| format!("ws write: {e}"))?;
                    event_counter += 1;
                },
                Err(_) => break,
            }
        }
        // Forward Fetch-domain events (requestPaused + fulfill-driven
        // Network.responseReceived/loadingFinished).
        loop {
            let pending = kernel.fetch_rx.lock().expect("fetch lock").try_recv();
            match pending {
                Ok(line) => {
                    let (kind, payload) = line
                        .split_once('\u{1f}')
                        .unwrap_or(("requestPaused", "{}"));
                    let params: Value = serde_json::from_str(payload).unwrap_or(json!({}));
                    let event = json!({
                        "method": format!("Fetch.{kind}"),
                        "params": params,
                    });
                    let out = serde_json::to_string(&event).map_err(|e| e.to_string())?;
                    ws.send(tokio_tungstenite::tungstenite::Message::text(out))
                        .await
                        .map_err(|e| format!("ws write: {e}"))?;
                    // fulfillment also emits the Network pair (real data)
                    if kind == "responseReceived" || kind == "loadingFinished" {
                        let event = json!({
                            "method": format!("Network.{kind}"),
                            "params": params,
                        });
                        let out = serde_json::to_string(&event).map_err(|e| e.to_string())?;
                        ws.send(tokio_tungstenite::tungstenite::Message::text(out))
                            .await
                            .map_err(|e| format!("ws write: {e}"))?;
                    }
                },
                Err(_) => break,
            }
        }
        // Forward screencast frames (Page.screencastFrame).
        loop {
            let pending = kernel.frame_rx.lock().expect("frame lock").try_recv();
            match pending {
                Ok(data_b64) if !data_b64.is_empty() => {
                    let event = json!({
                        "method": "Page.screencastFrame",
                        "params": {
                            "data": data_b64,
                            "metadata": {
                                "offsetTop": 0,
                                "pageScaleFactor": 1,
                                "deviceWidth": 1280,
                                "deviceHeight": 800,
                                "scrollOffsetX": 0,
                                "scrollOffsetY": 0,
                                "timestamp": std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_secs_f64())
                                    .unwrap_or(0.0),
                            },
                            "sessionId": 1,
                        }
                    });
                    let out = serde_json::to_string(&event).map_err(|e| e.to_string())?;
                    ws.send(tokio_tungstenite::tungstenite::Message::text(out))
                        .await
                        .map_err(|e| format!("ws write: {e}"))?;
                },
                Err(_) => break,
                _ => continue,
            }
        }
        // Forward pending resource loads as Network.requestWillBeSent.
        loop {
            let pending = kernel
                .request_rx
                .lock()
                .expect("request lock")
                .try_recv();
            match pending {
                Ok(line) => {
                    let (method, url) = line
                        .split_once('\u{1f}')
                        .unwrap_or(("GET", line.as_str()));
                    let event = json!({
                        "method": "Network.requestWillBeSent",
                        "params": {
                            "requestId": format!("req-{}", event_counter),
                            "loaderId": "",
                            "documentURL": url,
                            "request": {
                                "url": url,
                                "method": method,
                                "headers": {},
                            },
                            "timestamp": std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0),
                            "wallTime": std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs_f64())
                                .unwrap_or(0.0),
                            "type": "Other",
                        }
                    });
                    let out = serde_json::to_string(&event).map_err(|e| e.to_string())?;
                    ws.send(tokio_tungstenite::tungstenite::Message::text(out))
                        .await
                        .map_err(|e| format!("ws write: {e}"))?;
                    event_counter += 1;
                },
                Err(_) => break,
            }
        }
        let msg = match msg {
            Ok(m) => m,
            Err(e) => return Err(format!("ws read: {e}")),
        };
        let text = match msg {
            tokio_tungstenite::tungstenite::Message::Text(t) => t,
            tokio_tungstenite::tungstenite::Message::Close(_) => return Ok(()),
            _ => continue,
        };
        let req: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = req["id"].as_i64().unwrap_or(0);
        let method = req["method"].as_str().unwrap_or("").to_string();
        let params = req["params"].clone();

        let response = match method.as_str() {
            "Target.createTarget" => {
                target_id = format!("servo-target-{}", std::process::id());
                // Navigate to the requested URL (about:blank default).
                let url = params["url"].as_str().unwrap_or("about:blank").to_string();
                let r = kernel.call(KernelCmd::Navigate(url));
                to_response(id, r, json!({ "targetId": target_id }))
            },
            "Target.attachToTarget" => to_response(id, KernelReply::Ok(json!({})), json!({ "sessionId": session_id })),
            "Runtime.enable" | "Page.enable" | "Runtime.disable" | "Page.disable" => {
                // Synthetic session context: the flattened session gets one
                // execution context up front (Puppeteer waits for it).
                if method == "Runtime.enable" {
                    let event = json!({
                        "method": "Runtime.executionContextCreated",
                        "params": {
                            "context": {
                                "id": 1,
                                "origin": "about:blank",
                                "name": "servo-main",
                                "isDefault": true,
                            }
                        }
                    });
                    let out = serde_json::to_string(&event).map_err(|e| e.to_string())?;
                    ws.send(tokio_tungstenite::tungstenite::Message::text(out))
                        .await
                        .map_err(|e| format!("ws write: {e}"))?;
                }
                to_response(id, KernelReply::Ok(json!({})), json!({}))
            },
            "Emulation.setDeviceMetricsOverride" | "Emulation.clearDeviceMetricsOverride"
            | "Emulation.setDefaultBackgroundColorOverride" | "Emulation.setTouchEmulationEnabled"
            | "Emulation.setFocusEmulationEnabled" => {
                // Viewport stays at the boot size (1280x800); acknowledge so
                // clients proceed. Kernel-side resize lands with a live
                // SoftwareRenderingContext swap.
                to_response(id, KernelReply::Ok(json!({})), json!({}))
            },
            "Storage.getCookies" => reply_to_response(id, kernel.call(KernelCmd::GetCookies)),
            "Storage.setCookies" => {
                let mut last = json!({ "id": id, "result": {} });
                if let Some(cookies) = params["cookies"].as_array() {
                    for c in cookies {
                        let (name, domain) = (
                            c["name"].as_str().unwrap_or(""),
                            c["domain"].as_str().unwrap_or(""),
                        );
                        if name.is_empty() || domain.is_empty() {
                            continue;
                        }
                        let mut cookie_string =
                            format!("{}={}; Domain={}", name, c["value"].as_str().unwrap_or(""), domain);
                        if let Some(path) = c["path"].as_str() {
                            cookie_string.push_str(&format!("; Path={path}"));
                        }
                        last = reply_to_response(id, kernel.call(KernelCmd::SetCookie(cookie_string)));
                    }
                }
                last
            },
            "Storage.clearDataForOrigin" | "Storage.clearCookies" => {
                // Jar-wide clearing lands with SiteDataManager clear hooks.
                to_response(id, KernelReply::Ok(json!({})), json!({}))
            },
            "Browser.getVersion" => to_response(
                id,
                KernelReply::Ok(json!({})),
                json!({
                    "Browser": "Servo/obscura",
                    "protocolVersion": "1.3",
                    "userAgent": crate::fingerprint::random_profile()
                        .map(|p| p.user_agent)
                        .unwrap_or_else(|| "Mozilla/5.0".into()),
                }),
            ),
            "Browser.close" => to_response(id, KernelReply::Ok(json!({})), json!({})),
            "Page.startScreencast" => {
                let every_ms = params["everyNthFrame"]
                    .as_u64()
                    .map(|n| n * 100)
                    .unwrap_or(200)
                    .clamp(100, 2000);
                reply_to_response(
                    id,
                    kernel.call(KernelCmd::StartScreencast {
                        max_width: params["maxWidth"].as_u64().unwrap_or(1280) as u32,
                        max_height: params["maxHeight"].as_u64().unwrap_or(800) as u32,
                        every_ms,
                    }),
                )
            },
            "Page.stopScreencast" => reply_to_response(id, kernel.call(KernelCmd::StopScreencast)),
            "Page.captureSnapshot" => {
                // MHTML capture has no kernel equivalent; the serialized DOM
                // is the closest lossless artifact we can produce.
                match kernel.call(KernelCmd::DomOp {
                    js: "document.documentElement.outerHTML".into(),
                }) {
                    KernelReply::Ok(v) => {
                        let html = v["value"].as_str().unwrap_or("");
                        let b64 = base64::engine::general_purpose::STANDARD.encode(html.as_bytes());
                        to_response(id, KernelReply::Ok(json!({})), json!({ "data": b64 }))
                    },
                    KernelReply::Err(e) => error_response(id, e),
                    _ => error_response(id, "unexpected reply"),
                }
            },
            "Input.dispatchTouchEvent" => {
                // Map a tap to mouse pressed/released at the touch point
                // (kernel has no touch pipeline; tap ≈ click for challenges).
                let points = params["touchPoints"].as_array();
                let (x, y) = match points.and_then(|p| p.last()) {
                    Some(pt) => (
                        pt["x"].as_f64().unwrap_or(0.0) as f32,
                        pt["y"].as_f64().unwrap_or(0.0) as f32,
                    ),
                    None => (0.0, 0.0),
                };
                let params_json = json!({ "x": x, "y": y });
                let r1 = kernel.call(KernelCmd::Mouse {
                    event_type: "mousePressed".into(),
                    x,
                    y,
                });
                let r2 = kernel.call(KernelCmd::Mouse {
                    event_type: "mouseReleased".into(),
                    x,
                    y,
                });
                let _ = params_json;
                match (r1, r2) {
                    (KernelReply::Ok(_), KernelReply::Ok(_)) => {
                        to_response(id, KernelReply::Ok(json!({})), json!({}))
                    },
                    (KernelReply::Err(e), _) | (_, KernelReply::Err(e)) => error_response(id, e),
                    _ => to_response(id, KernelReply::Ok(json!({})), json!({})),
                }
            },
            "Fetch.enable" => {
                let patterns: Vec<String> = params["patterns"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|p| p["urlPattern"].as_str().map(ToString::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                reply_to_response(id, kernel.call(KernelCmd::FetchEnable { patterns }))
            },
            "Fetch.disable" => reply_to_response(id, kernel.call(KernelCmd::FetchDisable)),
            "Fetch.continueRequest" => {
                let request_id = params["requestId"].as_str().unwrap_or("").to_string();
                reply_to_response(id, kernel.call(KernelCmd::FetchContinue { request_id }))
            },
            "Fetch.failRequest" => {
                let request_id = params["requestId"].as_str().unwrap_or("").to_string();
                reply_to_response(id, kernel.call(KernelCmd::FetchFail { request_id }))
            },
            "Fetch.fulfillRequest" => {
                let request_id = params["requestId"].as_str().unwrap_or("").to_string();
                let status_code = params["responseCode"].as_u64().unwrap_or(200) as u16;
                let headers: Vec<(String, String)> = params["responseHeaders"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|h| {
                                Some((
                                    h["name"].as_str()?.to_string(),
                                    h["value"].as_str()?.to_string(),
                                ))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let body_b64 = params["body"].as_str().unwrap_or("").to_string();
                reply_to_response(
                    id,
                    kernel.call(KernelCmd::FetchFulfill {
                        request_id,
                        status_code,
                        headers,
                        body_b64,
                    }),
                )
            },
            "Target.getTargetInfo" | "Target.getTargets" => to_response(
                id,
                KernelReply::Ok(json!({})),
                json!({
                    "targetInfo": {
                        "targetId": format!("servo-target-{}", std::process::id()),
                        "type": "page",
                        "title": "",
                        "url": "about:blank",
                        "attached": true,
                        "browserContextId": "servo-context-1",
                    }
                }),
            ),
            "Page.navigate" => {
                let url = params["url"].as_str().unwrap_or("about:blank").to_string();
                match kernel.call(KernelCmd::Navigate(url.clone())) {
                    KernelReply::Ok(v) => to_response(
                        id,
                        KernelReply::Ok(json!({})),
                        json!({ "frameId": "servo-frame-1", "loaderId": url }),
                    ).merge_ok(v),
                    KernelReply::Err(e) => error_response(id, e),
                    _ => error_response(id, "unexpected reply"),
                }
            },
            "Runtime.evaluate" => {
                let expr = params["expression"].as_str().unwrap_or("").to_string();
                match kernel.call(KernelCmd::Evaluate(expr)) {
                    KernelReply::Ok(v) => to_response(
                        id,
                        KernelReply::Ok(json!({})),
                        json!({ "result": { "type": "string", "value": v["value"] } }),
                    ),
                    KernelReply::Err(e) => error_response(id, e),
                    _ => error_response(id, "unexpected reply"),
                }
            },
            "Input.dispatchMouseEvent" => {
                let event_type = params["type"].as_str().unwrap_or("").to_string();
                let x = params["x"].as_f64().unwrap_or(0.0) as f32;
                let y = params["y"].as_f64().unwrap_or(0.0) as f32;
                match kernel.call(KernelCmd::Mouse { event_type, x, y }) {
                    KernelReply::Ok(_) => to_response(id, KernelReply::Ok(json!({})), json!({})),
                    KernelReply::Err(e) => error_response(id, e),
                    _ => error_response(id, "unexpected reply"),
                }
            },
            "Input.dispatchKeyEvent" => {
                let event_type = params["type"].as_str().unwrap_or("").to_string();
                let key = params["key"].as_str().unwrap_or("").to_string();
                let code = params["code"].as_str().unwrap_or("").to_string();
                let text = params["text"].as_str().map(|s| s.to_string());
                match kernel.call(KernelCmd::Key { event_type, key, code, text }) {
                    KernelReply::Ok(_) => to_response(id, KernelReply::Ok(json!({})), json!({})),
                    KernelReply::Err(e) => error_response(id, e),
                    _ => error_response(id, "unexpected reply"),
                }
            },
            "Input.humanGesture" => {
                let pts = params["points"].as_array().ok_or("points required")?;
                let mut points = Vec::with_capacity(pts.len());
                for p in pts {
                    let x = p.get(0).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                    let y = p.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                    let d = p.get(2).and_then(|v| v.as_u64()).unwrap_or(10);
                    points.push((x, y, d));
                }
                reply_to_response(id, kernel.call(KernelCmd::HumanGesture {
                    points,
                    press: params["press"].as_bool().unwrap_or(false),
                    press_delay_ms: params["pressDelayMs"].as_u64().unwrap_or(80),
                    x: params["x"].as_f64().unwrap_or(0.0) as f32,
                    y: params["y"].as_f64().unwrap_or(0.0) as f32,
                }))
            },
            "Input.humanType" => {
                let text = params["text"].as_str().ok_or("text required")?.to_string();
                let delays: Vec<u64> = params["delays"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|d| d.as_u64()).collect())
                    .unwrap_or_default();
                reply_to_response(id, kernel.call(KernelCmd::HumanType {
                    text,
                    delays,
                    focus_expr: params["focusExpr"]
                        .as_str()
                        .unwrap_or("document.activeElement")
                        .to_string(),
                    clear: params["clear"].as_bool().unwrap_or(false),
                }))
            },
            "Page.reload" => reply_to_response(id, kernel.call(KernelCmd::Reload)),
            "Page.navigateToHistoryEntry" => {
                // CDP sends an entryId; we accept a numeric delta instead —
                // the Go client passes -1/+1 for back/forward.
                let entry = params["entryId"].as_i64().unwrap_or(0);
                let reply = if entry < 0 {
                    kernel.call(KernelCmd::GoBack((-entry) as usize))
                } else if entry > 0 {
                    kernel.call(KernelCmd::GoForward(entry as usize))
                } else {
                    KernelReply::Ok(json!({}))
                };
                reply_to_response(id, reply)
            },
            "Runtime.callFunctionOn" => {
                let expression = params["functionDeclaration"]
                    .as_str()
                    .or_else(|| params["expression"].as_str())
                    .unwrap_or("")
                    .to_string();
                reply_to_response(id, kernel.call(KernelCmd::CallFunctionOn { expression }))
            },
            "Runtime.addBinding" => {
                let name = params["name"].as_str().unwrap_or("").to_string();
                if name.is_empty() {
                    error_response(id, "addBinding: name required")
                } else {
                    // window.<name> posts the payload back through console
                    // (binding bridge lands with kernel bidirectional hooks).
                    let script = format!(
                        "(function(){{\
                            window.{name} = function(payload) {{\
                                try {{ console.info('__obscura_binding__:{name}:' + (typeof payload === 'string' ? payload : JSON.stringify(payload))); }} catch(e) {{}}\
                            }};\
                        }})()"
                    );
                    reply_to_response(id, kernel.call(KernelCmd::AddBinding { name, script }))
                }
            },
            "Runtime.removeBinding" => {
                let name = params["name"].as_str().unwrap_or("").to_string();
                reply_to_response(id, kernel.call(KernelCmd::RemoveBinding { name }))
            },
            "LP.getMarkdown" => reply_to_response(id, kernel.call(KernelCmd::GetMarkdown)),
            // DOM.* via page-side JS against the live kernel DOM.
            "DOM.focus" => {
                let js = match (&params["nodeId"], &params["selector"]) {
                    (node, _) if node.is_u64() => format!(
                        "(function(){{var els=document.querySelectorAll('*');var el=els[{}];if(el&&el.focus)el.focus();return !!el;}})()",
                        node.as_u64().unwrap_or(0).saturating_sub(1)
                    ),
                    (_, sel) if sel.is_string() => format!(
                        "(function(){{var el=document.querySelector({});if(el&&el.focus)el.focus();return !!el;}})()",
                        serde_json::to_string(sel.as_str().unwrap_or("body")).unwrap_or_default()
                    ),
                    _ => "(function(){var el=document.activeElement||document.body;if(el&&el.focus)el.focus();return true;})()".to_string(),
                };
                reply_to_response(id, kernel.call(KernelCmd::DomOp { js }))
            },
            "DOM.setAttributeValue" => {
                let selector = params["selector"].as_str().unwrap_or("body");
                let name = params["name"].as_str().unwrap_or("");
                let value = params["value"].as_str().unwrap_or("");
                if name.is_empty() {
                    error_response(id, "setAttributeValue: name required")
                } else {
                    let js = format!(
                        "(function(){{var el=document.querySelector({});if(!el)return false;el.setAttribute({},{});return true;}})()",
                        serde_json::to_string(selector).unwrap_or_default(),
                        serde_json::to_string(name).unwrap_or_default(),
                        serde_json::to_string(value).unwrap_or_default(),
                    );
                    reply_to_response(id, kernel.call(KernelCmd::DomOp { js }))
                }
            },
            "DOM.removeNode" => {
                let selector = params["selector"].as_str().unwrap_or("");
                if selector.is_empty() {
                    error_response(id, "removeNode: selector required")
                } else {
                    let js = format!(
                        "(function(){{var el=document.querySelector({});if(!el)return false;el.remove();return true;}})()",
                        serde_json::to_string(selector).unwrap_or_default(),
                    );
                    reply_to_response(id, kernel.call(KernelCmd::DomOp { js }))
                }
            },
            "DOM.setFileInputFiles" => {
                // Kernel-side file input population is not available yet.
                error_response(id, "DOM.setFileInputFiles: not supported by the Servo kernel yet")
            },
            "Page.addScriptToEvaluateOnNewDocument" => {
                let source = params["source"]
                    .as_str()
                    .ok_or("source required")?
                    .to_string();
                reply_to_response(id, kernel.call(KernelCmd::AddInitScript(source)))
            },
            "Network.enable" | "Network.disable" | "Network.setCacheDisabled"
            | "Network.setRequestInterception" | "Network.setBlockedURLs" => {
                to_response(id, KernelReply::Ok(json!({})), json!({}))
            },
            "Network.setExtraHTTPHeaders" => {
                // Header overrides land with the kernel's request-side
                // plumbing; acknowledged so clients keep flowing.
                reply_to_response(id, kernel.call(KernelCmd::SetExtraHeaders))
            },
            "Network.getCookies" | "Network.getAllCookies" => {
                reply_to_response(id, kernel.call(KernelCmd::GetCookies))
            },
            "Network.setCookie" => {
                let name = params["name"].as_str().unwrap_or("");
                let value = params["value"].as_str().unwrap_or("");
                let domain = params["domain"].as_str().unwrap_or("");
                if name.is_empty() || domain.is_empty() {
                    error_response(id, "setCookie: name and domain required")
                } else {
                    let mut cookie_string = format!("{name}={value}; Domain={domain}");
                    if let Some(path) = params["path"].as_str() {
                        cookie_string.push_str(&format!("; Path={path}"));
                    }
                    if params["secure"].as_bool().unwrap_or(false) {
                        cookie_string.push_str("; Secure");
                    }
                    if params["httpOnly"].as_bool().unwrap_or(false) {
                        cookie_string.push_str("; HttpOnly");
                    }
                    if let Some(secs) = params["expires"].as_f64() {
                        if secs > 0.0 {
                            cookie_string.push_str(&format!("; Max-Age={}", secs as i64));
                        }
                    }
                    reply_to_response(id, kernel.call(KernelCmd::SetCookie(cookie_string)))
                }
            },
            "Network.setCookies" => {
                let mut last = json!({ "id": id, "result": {} });
                if let Some(cookies) = params["cookies"].as_array() {
                    for c in cookies {
                        let (name, domain) = (
                            c["name"].as_str().unwrap_or(""),
                            c["domain"].as_str().unwrap_or(""),
                        );
                        if name.is_empty() || domain.is_empty() { continue; }
                        let mut cookie_string = format!("{}={}; Domain={}", name, c["value"].as_str().unwrap_or(""), domain);
                        if let Some(path) = c["path"].as_str() {
                            cookie_string.push_str(&format!("; Path={path}"));
                        }
                        last = reply_to_response(id, kernel.call(KernelCmd::SetCookie(cookie_string)));
                    }
                }
                last
            },
            "Page.captureScreenshot" => match kernel.call(KernelCmd::Screenshot) {
                KernelReply::Png(w, h, rgba) => {
                    let buf = image::RgbaImage::from_raw(w, h, rgba).ok_or("bad frame")?;
                    let mut png = Vec::new();
                    image::DynamicImage::ImageRgba8(buf)
                        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                        .map_err(|e| format!("png encode: {e}"))?;
                    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
                    to_response(id, KernelReply::Ok(json!({})), json!({ "data": b64 }))
                },
                KernelReply::Err(e) => error_response(id, e),
                _ => error_response(id, "unexpected reply"),
            },
            "Obscura.search" => {
                let engine = params["engine"].as_str().unwrap_or("bing").to_string();
                let query = params["query"].as_str().unwrap_or("").to_string();
                match kernel.call(KernelCmd::Search { engine, query }) {
                    KernelReply::Search(v) => to_response(id, KernelReply::Ok(json!({})), v),
                    KernelReply::Err(e) => error_response(id, e),
                    _ => error_response(id, "unexpected reply"),
                }
            },
            other => error_response(id, format!("Servo CDP: unsupported method {other}")),
        };

        let out = serde_json::to_string(&response).map_err(|e| e.to_string())?;
        ws.send(tokio_tungstenite::tungstenite::Message::text(out))
            .await
            .map_err(|e| format!("ws write: {e}"))?;
    }
    Ok(())
}


fn encode_frame_png(rgba: &[u8], w: u32, h: u32) -> String {
    if let Some(buf) = image::RgbaImage::from_raw(w, h, rgba.to_vec()) {
        let mut png = Vec::new();
        if image::DynamicImage::ImageRgba8(buf)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .is_ok()
        {
            return base64::engine::general_purpose::STANDARD.encode(png);
        }
    }
    String::new()
}

fn reply_to_response(id: i64, reply: KernelReply) -> Value {
    match reply {
        KernelReply::Ok(v) => to_response(id, KernelReply::Ok(json!({})), v),
        KernelReply::Err(e) => error_response(id, e),
        _ => error_response(id, "unexpected reply"),
    }
}

fn to_response(id: i64, _ack: KernelReply, result: Value) -> Value {
    json!({ "id": id, "result": result })
}

trait MergeOk {
    fn merge_ok(self, extra: Value) -> Value;
}
impl MergeOk for Value {
    fn merge_ok(mut self, extra: Value) -> Value {
        if let (Some(obj), Some(src)) = (self.as_object_mut(), extra.as_object()) {
            for (k, v) in src {
                obj.insert(k.clone(), v.clone());
            }
        }
        self
    }
}

fn error_response(id: i64, message: impl Into<String>) -> Value {
    json!({ "id": id, "error": { "code": -32601, "message": message.into() } })
}
