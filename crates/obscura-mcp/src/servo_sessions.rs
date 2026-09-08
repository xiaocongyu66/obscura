//! Servo-kernel session pool for the MCP server. Each session is a separate
//! `obscura-embedder serve` child process (own UA/TLS/cookies/history) and
//! speaks CDP over WS — the exact protocol the registration client uses.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

pub struct ServoSession {
    pub child: std::process::Child,
    pub port: u16,
    ws: Option<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>,
    next_id: AtomicU64,
}

pub struct ServoPool {
    pub sessions: HashMap<String, Arc<tokio::sync::Mutex<ServoSession>>>,
    pub active: Option<String>,
    counter: u32,
}

impl ServoPool {
    pub fn new() -> Self {
        Self { sessions: HashMap::new(), active: None, counter: 0 }
    }

    /// Spawn a session process and connect its CDP WS.
    pub async fn spawn_session(&mut self, profile_idx: u32) -> Result<String, String> {
        self.counter += 1;
        let id = format!("servo-{}", self.counter);
        // Port: pick from a high range keyed by counter (no discovery needed
        // since the parent knows the port it passed).
        let port = 9300u16 + ((self.counter % 400) as u16);
        let child = std::process::Command::new(std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("servo-embedder-serve")))
            .unwrap_or_else(|| "obscura-embedder-serve".into()))
            .arg(port.to_string())
            .arg(profile_idx.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn servo session: {e}"))?;
        // Wait for the WS to come up.
        let url = format!("ws://127.0.0.1:{port}/devtools/browser");
        let mut ws = None;
        for _ in 0..60 {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            if let Ok(s) = tokio_tungstenite::connect_async(&url).await {
                ws = Some(s.0);
                break;
            }
        }
        let ws = ws.ok_or("servo session ws never came up")?;
        self.sessions.insert(id.clone(), Arc::new(tokio::sync::Mutex::new(ServoSession {
            child, port, ws: Some(ws), next_id: AtomicU64::new(1),
        })));
        self.active = Some(id.clone());
        Ok(id)
    }

    /// Send one CDP command and wait for its id-matched reply.
    pub async fn cdp_call(session: &mut ServoSession, method: &str, params: Value) -> Result<Value, String> {
        use futures_util::{SinkExt, StreamExt};
        let ws = session.ws.as_mut().ok_or("session ws closed")?;
        let id = session.next_id.fetch_add(1, Ordering::Relaxed);
        let req = json!({ "id": id, "method": method, "params": params });
        ws.send(Message::text(serde_json::to_string(&req).map_err(|e| e.to_string())?))
            .await.map_err(|e| format!("ws send: {e}"))?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(90);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() { return Err("cdp call timeout".into()); }
            let msg = tokio::time::timeout(remaining, ws.next())
                .await.map_err(|_| "cdp recv timeout")?
                .ok_or("ws closed")?.map_err(|e| e.to_string())?;
            if let Message::Text(t) = msg {
                let v: Value = serde_json::from_str(&t).map_err(|e| e.to_string())?;
                if v["id"].as_i64() == Some(id as i64) {
                    if let Some(err) = v.get("error") {
                        return Err(err["message"].as_str().unwrap_or("cdp error").into());
                    }
                    return Ok(v["result"].clone());
                }
                // Ignore events; keep waiting for the matching id.
            }
        }
    }
}

/// tool: servo_session_new — spawn an isolated kernel session.
pub async fn tool_session_new(args: &Value, pool: &mut ServoPool) -> Result<String, String> {
    let profile_idx = args["profileIndex"].as_u64().unwrap_or(rand_light()) as u32;
    let id = pool.spawn_session(profile_idx).await?;
    Ok(format!("session {id} started (profile #{profile_idx})"))
}

/// tool: servo_search — Bing/Baidu structured search in the active session.
pub async fn tool_search(args: &Value, pool: &mut ServoPool) -> Result<Value, String> {
    let id = pool.active.clone().ok_or("no active servo session; call servo_session_new first")?;
    let mut s = pool.sessions.get(&id).cloned().ok_or("session gone")?;
    let mut s = s.lock().await;
    let params = json!({
        "engine": args["engine"].as_str().unwrap_or("bing"),
        "query": args["query"].as_str().ok_or("query required")?,
    });
    let result = ServoPool::cdp_call(&mut s, "Obscura.search", params).await?;
    // Results carry title+url+snippet for precise model targeting.
    Ok(result)
}

/// tool: servo_navigate — navigate the active session.
pub async fn tool_navigate(args: &Value, pool: &mut ServoPool) -> Result<String, String> {
    let id = pool.active.clone().ok_or("no active servo session")?;
    let mut s = pool.sessions.get(&id).cloned().ok_or("session gone")?;
    let mut s = s.lock().await;
    let url = args["url"].as_str().ok_or("url required")?;
    ServoPool::cdp_call(&mut s, "Page.navigate", json!({ "url": url })).await?;
    Ok(format!("navigated to {url}"))
}

/// tool: servo_evaluate — evaluate JS in the active session.
pub async fn tool_evaluate(args: &Value, pool: &mut ServoPool) -> Result<Value, String> {
    let id = pool.active.clone().ok_or("no active servo session")?;
    let mut s = pool.sessions.get(&id).cloned().ok_or("session gone")?;
    let mut s = s.lock().await;
    let expr = args["expression"].as_str().ok_or("expression required")?;
    ServoPool::cdp_call(&mut s, "Runtime.evaluate", json!({ "expression": expr })).await
}

/// tool: servo_screenshot — PNG (base64) of the active session.
pub async fn tool_screenshot(pool: &mut ServoPool) -> Result<Value, String> {
    let id = pool.active.clone().ok_or("no active servo session")?;
    let mut s = pool.sessions.get(&id).cloned().ok_or("session gone")?;
    let mut s = s.lock().await;
    ServoPool::cdp_call(&mut s, "Page.captureScreenshot", json!({})).await
}

/// tool: servo_session_close — kill the child process.
pub async fn tool_session_close(args: &Value, pool: &mut ServoPool) -> Result<String, String> {
    let id = args["sessionId"].as_str().map(|s| s.to_string()).or_else(|| pool.active.clone()).ok_or("no session")?;
    if let Some(s) = pool.sessions.remove(&id) {
        let _ = s.lock().await.child.kill();
        if pool.active.as_deref() == Some(id.as_str()) {
            pool.active = None;
        }
    }
    Ok(format!("session {id} closed"))
}

fn rand_light() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.subsec_nanos() as u64) % 12).unwrap_or(0)
}
