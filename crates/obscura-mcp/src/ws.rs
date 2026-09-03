//! WebSocket transport for the MCP JSON-RPC server.
//!
//! Each WebSocket connection becomes its own isolated MCP session: its own
//! `BrowserState`, its own fingerprint, its own cookie jar, its own
//! proxy. Two connections never share state, which is what an agent pool
//! needs — each agent gets a fresh identity without having to spawn a
//! separate process.
//!
//! Frames are newline-delimited JSON-RPC (same shape as the stdio
//! transport). Binary frames are rejected; ping/pong/close are handled by
//! `tokio-tungstenite`.
//!
//! ## Threading
//!
//! Each connection runs on its own OS thread with a `current_thread` tokio
//! runtime + `LocalSet`. V8 isolates are `!Send`, so all page state for one
//! connection must stay on one thread. This mirrors `obscura-cdp`'s
//! connection model.

use anyhow::Result;
use futures_util::StreamExt;
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tracing::{info, warn};

use crate::{dispatch, BrowserState};

/// Maximum number of frames queued for sending back to one client.
const MAX_SEND_QUEUE: usize = 1024;

/// Cap on simultaneous connections. Each one holds a V8 isolate + DOM tree,
/// so memory grows fast. Operators wanting more should spawn more processes
/// behind a load balancer (each process gets its own port and identity pool).
const MAX_CONNECTIONS: usize = 64;

/// Bind a WebSocket MCP server at `host:port`. Each accepted WS connection
/// runs in its own thread with its own `BrowserState`.
///
/// This is `async` so it composes with the caller's runtime (the obscura
/// CLI runs it inside its main `current_thread` runtime). The accept loop
/// runs on the calling thread; each connection is dispatched to its own
/// OS thread with its own `current_thread` runtime + `LocalSet` (V8
/// isolates are `!Send`).
pub async fn run(
    host: String,
    port: u16,
    proxy: Option<String>,
    user_agent: Option<String>,
    stealth: bool,
) -> Result<()> {
    let listener = TcpListener::bind((host.as_str(), port)).await?;
    info!("obscura-mcp WebSocket listening on ws://{host}:{port}");

    let live = Arc::new(AtomicUsize::new(0));

    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                warn!("accept failed: {e}");
                continue;
            }
        };

        if live.load(Ordering::Acquire) >= MAX_CONNECTIONS {
            warn!("ws accept {peer} rejected: at cap {}", MAX_CONNECTIONS);
            continue;
        }
        live.fetch_add(1, Ordering::AcqRel);

        let proxy = proxy.clone();
        let user_agent = user_agent.clone();
        let live2 = live.clone();
        let spawn_result = std::thread::Builder::new()
            .name(format!("obscura-mcp-ws-{peer}"))
            .spawn(move || {
                struct Slot(Arc<AtomicUsize>);
                impl Drop for Slot {
                    fn drop(&mut self) {
                        self.0.fetch_sub(1, Ordering::AcqRel);
                    }
                }
                let _slot = Slot(live2.clone());

                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(r) => r,
                    Err(e) => {
                        warn!("ws runtime build failed: {e}");
                        return;
                    }
                };
                let local = tokio::task::LocalSet::new();
                local.block_on(&rt, async move {
                    if let Err(e) = serve_one(stream, proxy, user_agent, stealth).await {
                        warn!("ws session {peer} ended: {e}");
                    }
                });
            });
        if let Err(e) = spawn_result {
            warn!("ws thread spawn failed: {e}");
            live.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

async fn serve_one(
    stream: tokio::net::TcpStream,
    proxy: Option<String>,
    user_agent: Option<String>,
    stealth: bool,
) -> Result<()> {
    let _ = stream.set_nodelay(true);
    let mut cfg = WebSocketConfig::default();
    cfg.write_buffer_size = 0;
    cfg.max_write_buffer_size = 64 << 20;

    let ws_stream = tokio_tungstenite::accept_async_with_config(stream, Some(cfg)).await?;
    let (ws_sender, mut ws_receiver) = ws_stream.split();

    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::channel::<String>(MAX_SEND_QUEUE);

    // Sender task: drains `reply_tx` into the WS sink.
    use futures_util::SinkExt;
    let mut ws_sender = ws_sender;
    let sender_task = tokio::task::spawn_local(async move {
        use tokio_tungstenite::tungstenite::protocol::Message;
        while let Some(msg) = reply_rx.recv().await {
            if ws_sender.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
        }
    });

    let mut state = BrowserState::new(proxy, user_agent, stealth);
    let mut runtime_pump_armed = false;

    while let Some(msg) = ws_receiver.next().await {
        use tokio_tungstenite::tungstenite::protocol::Message;
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                warn!("ws read error: {e}");
                break;
            }
        };

        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Ping(_) => continue, // tungstenite auto-pongs
            Message::Close(_) => break,
            // Binary frames are not part of the MCP protocol.
            _ => continue,
        };

        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }

        // One WS frame can contain multiple newline-delimited JSON-RPC
        // messages (a client batching tool calls).
        for line in trimmed.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let parsed: crate::RpcMessage = match serde_json::from_str(line) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if parsed.id.is_none() {
                continue;
            }
            let id = parsed.id.clone().unwrap_or(Value::Null);
            let response = dispatch(&parsed.method, id, &parsed.params, &mut state).await;
            runtime_pump_armed = state.has_active_page_runtime();

            let mut body = serde_json::to_string(&response)?;
            body.push('\n');
            if reply_tx.send(body).await.is_err() {
                break;
            }
        }

        if runtime_pump_armed {
            match state.advance_active_page_tasks().await {
                Ok(reached_idle) => runtime_pump_armed = !reached_idle,
                Err(error) => {
                    runtime_pump_armed = false;
                    let err_resp = crate::RpcResponse::err(
                        Value::Null,
                        -32000,
                        format!("page task error: {error}"),
                    );
                    if let Ok(body) = serde_json::to_string(&err_resp) {
                        let _ = reply_tx.send(body).await;
                    }
                }
            }
        }
    }

    drop(reply_tx);
    let _ = sender_task.await;
    Ok(())
}

/// Helper exposed for the CLI: returns the URL a client should connect to.
pub fn advertise_url(host: &str, port: u16) -> String {
    format!("ws://{host}:{port}")
}

/// Stdio-transport shim reused by some test harnesses that speak WS on
/// stdin/stdout. Not used by the production server.
#[allow(dead_code)]
async fn pump_stdio(_state: &mut BrowserState) -> Result<()> {
    let _stdin = tokio::io::stdin();
    let _stdout = tokio::io::stdout();
    let _reader = BufReader::new(_stdin);
    let mut _writer = _stdout;
    let _ = _writer.write_all(b"").await?;
    Ok(())
}
