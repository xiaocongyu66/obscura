//! obscura-mcp: MCP server over the Servo-kernel embedder.
//!
//! Tools are all `servo_*`: each session is an isolated `obscura-embedder
//! serve` child process (own UA / ClientHints / TLS / cookies / history).
//! Transports: stdio (default), WebSocket, HTTP — the JSON-RPC shape is
//! unchanged (tools/list, tools/call).

pub mod servo_sessions;
pub mod ws;
pub mod http;

use serde_json::{json, Value};
use servo_sessions::ServoPool;

pub struct BrowserState {
    pub servo_pool: ServoPool,
}

impl BrowserState {
    pub fn new() -> Self {
        Self { servo_pool: ServoPool::new() }
    }
}

#[derive(serde::Serialize)]
pub(crate) struct RpcResponse {
    pub id: Value,
    pub body: Value,
}

impl RpcResponse {
    pub fn ok(id: Value, body: Value) -> Self {
        Self { id: id.clone(), body: json!({ "jsonrpc": "2.0", "id": id, "result": body }) }
    }
    pub fn err(id: Value, code: i64, message: String) -> Self {
        Self { id: id.clone(), body: json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }) }
    }
    pub fn to_value(&self) -> Value {
        self.body.clone()
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct RpcMessage {
    pub method: String,
    pub id: Option<Value>,
    #[serde(default)]
    pub params: Value,
}

pub(crate) async fn dispatch(method: &str, id: Value, params: &Value, state: &mut BrowserState) -> RpcResponse {
    match method {
        "initialize" => RpcResponse::ok(
            id,
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "obscura-servo-mcp", "version": "0.2.0" }
            }),
        ),
        "tools/list" => tools_list(id),
        "tools/call" => tool_call(id, params, state).await,
        "resources/list" => RpcResponse::ok(id, json!({"resources": []})),
        "prompts/list" => RpcResponse::ok(id, json!({"prompts": []})),
        _ => RpcResponse::err(id, -32601, format!("Unknown method: {method}")),
    }
}

fn tools_list(id: Value) -> RpcResponse {
    let tools = json!([
        { "name": "servo_session_new",
          "description": "Start an isolated Servo-kernel session (own UA/ClientHints/TLS/cookies/history). Optional profileIndex.",
          "inputSchema": { "type": "object", "properties": { "profileIndex": { "type": "integer" } } } },
        { "name": "servo_search",
          "description": "Web search via the active session: CN Bing (default) or Baidu. Returns {title,url,snippet} per result.",
          "inputSchema": { "type": "object",
            "properties": { "query": { "type": "string" }, "engine": { "type": "string", "enum": ["bing", "baidu"] } },
            "required": ["query"] } },
        { "name": "servo_navigate",
          "description": "Navigate the active session to a URL and wait for load.",
          "inputSchema": { "type": "object", "properties": { "url": { "type": "string" } }, "required": ["url"] } },
        { "name": "servo_evaluate",
          "description": "Evaluate JavaScript in the active session; returns the value as a string.",
          "inputSchema": { "type": "object", "properties": { "expression": { "type": "string" } }, "required": ["expression"] } },
        { "name": "servo_screenshot",
          "description": "PNG screenshot (base64) of the active session's viewport.",
          "inputSchema": { "type": "object", "properties": {} } },
        { "name": "servo_session_close",
          "description": "Close a Servo session (kills the isolated process).",
          "inputSchema": { "type": "object", "properties": { "sessionId": { "type": "string" } } } },
    ]);
    RpcResponse::ok(id, json!({ "tools": tools }))
}

async fn tool_call(id: Value, params: &Value, state: &mut BrowserState) -> RpcResponse {
    let name = match params.get("name").and_then(Value::as_str) {
        Some(n) => n.to_string(),
        None => return RpcResponse::err(id, -32602, "Missing tool name".into()),
    };
    let args = params.get("arguments").cloned().unwrap_or(Value::Null);
    let pool = &mut state.servo_pool;
    let result: Result<Value, String> = match name.as_str() {
        "servo_session_new" => servo_sessions::tool_session_new(&args, pool).await.map(Value::String),
        "servo_search" => servo_sessions::tool_search(&args, pool).await,
        "servo_navigate" => servo_sessions::tool_navigate(&args, pool).await.map(Value::String),
        "servo_evaluate" => servo_sessions::tool_evaluate(&args, pool).await,
        "servo_screenshot" => servo_sessions::tool_screenshot(pool).await,
        "servo_session_close" => servo_sessions::tool_session_close(&args, pool).await.map(Value::String),
        other => Err(format!("Unknown tool: {other}")),
    };
    match result {
        Ok(content) => {
            let text = match content {
                Value::String(s) => s,
                other => other.to_string(),
            };
            RpcResponse::ok(id, json!({ "content": [{ "type": "text", "text": text }] }))
        },
        Err(e) => RpcResponse::ok(id, json!({
            "content": [{ "type": "text", "text": format!("Error: {e}") }],
            "isError": true
        })),
    }
}

pub async fn run() -> anyhow::Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut writer = stdout;
    let mut state = BrowserState::new();

    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let msg: RpcMessage = match serde_json::from_str(trimmed) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let Some(id) = msg.id else { continue };
        let response = dispatch(&msg.method, id, &msg.params, &mut state).await;
        let mut body = serde_json::to_string(&response.to_value())?;
        body.push('\n');
        writer.write_all(body.as_bytes()).await?;
        writer.flush().await?;
    }
}

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
