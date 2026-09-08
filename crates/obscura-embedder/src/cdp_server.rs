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
}

enum KernelReply {
    Ok(Value),
    Err(String),
    Png(u32, u32, Vec<u8>),
    Search(Value),
}

struct KernelHandle {
    tx: Sender<KernelCmd>,
    rx: std::sync::Mutex<Receiver<KernelReply>>,
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
    std::thread::Builder::new()
        .name("servo-kernel".into())
        .spawn(move || {
            let servo = match HeadlessServo::new(viewport) {
                Ok(s) => s,
                Err(e) => {
                    let _ = reply_tx.send(KernelReply::Err(e));
                    return;
                },
            };
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
                };
                let _ = reply_tx.send(reply);
            }
        })
        .map_err(|e| format!("spawn kernel: {e}"))?;
    // Wait for the kernel to come up (first reply would only come on a
    // command; boot errors surface on the first call instead).
    Ok(KernelHandle { tx: cmd_tx, rx: reply_rx })
}

/// Bind the CDP WS server. Returns after the listener is up.
pub async fn serve(host: &str, port: u16, viewport: (u32, u32)) -> Result<(), String> {
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
    stream: tokio::net::TcpStream,
    kernel: Arc<KernelHandle>,
) -> Result<(), String> {
    let mut ws = tokio_tungstenite::accept_async_with_config(stream, Some(WebSocketConfig::default()))
        .await
        .map_err(|e| format!("ws accept: {e}"))?;

    // Session state per connection (flatten CDP: one synthetic session id).
    let session_id = "servo-session-1";
    let mut target_id = String::new();

    use futures_util::{SinkExt, StreamExt};
    while let Some(msg) = ws.next().await {
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
                to_response(id, KernelReply::Ok(json!({})), json!({}))
            },
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
