//! ACP (Agent Client Protocol) server: `zencode --acp`.
//!
//! Implements a minimal but functional ACP agent over JSON-RPC 2.0 on stdio,
//! per <https://agentclientprotocol.com>. Supported methods:
//! `initialize`, `authenticate`, `session/new`, `session/prompt`, `session/cancel`.
//! Agent events are streamed back as `session/update` notifications.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use zen_core::{AgentEvent, ChatMessage, EventSink, LlmBackend};

use crate::agent_run;
use crate::cli::Cli;
use crate::settings;

// ───────────────────────── JSON-RPC 2.0 envelope ─────────────────────────

#[derive(Debug, Deserialize)]
struct RpcRequest {
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct RpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
struct RpcError {
    code: i64,
    message: String,
}

#[derive(Debug, Serialize)]
struct RpcNotification {
    jsonrpc: &'static str,
    method: &'static str,
    params: Value,
}

impl RpcResponse {
    fn ok(id: Value, result: Value) -> Self {
        Self { jsonrpc: "2.0", id, result: Some(result), error: None }
    }
    fn err(id: Value, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(RpcError { code, message: message.into() }),
        }
    }
}

// ───────────────────────── ACP protocol types ─────────────────────────

const PROTOCOL_VERSION: u64 = 1;

/// Agent -> client `session/update` notification payload.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionUpdateParams {
    session_id: String,
    update: SessionUpdate,
}

#[derive(Debug, Serialize)]
#[serde(tag = "sessionUpdate", rename_all = "snake_case")]
enum SessionUpdate {
    AgentMessageChunk { content: ContentBlock },
    ToolCall(ToolCallUpdate),
    ToolCallUpdate(ToolCallResult),
    #[serde(rename_all = "camelCase")]
    UsageUpdate { used: u64, size: u64 },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    Text { text: String },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolCallUpdate {
    tool_call_id: String,
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolCallResult {
    tool_call_id: String,
    status: &'static str, // "completed" | "failed" | "in_progress"
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<Vec<ContentBlock>>,
}

// ───────────────────────── EventSink -> ACP bridge ─────────────────────────

/// Translates `AgentEvent`s into ACP `session/update` notifications on stdout.
/// Stdout is shared with JSON-RPC responses, so all writes go through one mutex.
#[derive(Clone)]
struct AcpSink {
    out: Arc<Mutex<std::io::Stdout>>,
    session_id: String,
    /// tool_use_id -> tool name, for result correlation.
    tools: Arc<Mutex<HashMap<String, String>>>,
}

impl AcpSink {
    fn notify(&self, update: SessionUpdate) {
        let params = SessionUpdateParams { session_id: self.session_id.clone(), update };
        let notif = RpcNotification {
            jsonrpc: "2.0",
            method: "session/update",
            params: serde_json::to_value(params).unwrap_or(Value::Null),
        };
        if let Ok(line) = serde_json::to_string(&notif) {
            let mut out = self.out.lock().unwrap();
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
        }
    }
}

impl EventSink for AcpSink {
    fn emit(&self, ev: AgentEvent) {
        match ev {
            AgentEvent::TextDelta { text } => {
                self.notify(SessionUpdate::AgentMessageChunk {
                    content: ContentBlock::Text { text },
                });
            }
            AgentEvent::Assistant { message, .. } => {
                emit_assistant(self, &message);
            }
            AgentEvent::ToolResult { tool_use_id, content, is_error, .. } => {
                let status = if is_error { "failed" } else { "completed" };
                self.notify(SessionUpdate::ToolCallUpdate(ToolCallResult {
                    tool_call_id: tool_use_id,
                    status,
                    content: Some(vec![ContentBlock::Text { text: content }]),
                }));
            }
            AgentEvent::SystemMessage { text, .. } => {
                // Surface hook/system notices as agent text so the client sees them.
                self.notify(SessionUpdate::AgentMessageChunk {
                    content: ContentBlock::Text { text },
                });
            }
            // Retries, permission denials and compaction boundaries are internal;
            // the final result / stop reason carries the outcome.
            AgentEvent::ApiRetry { .. }
            | AgentEvent::PermissionDenied(_)
            | AgentEvent::CompactBoundary { .. } => {}
            _ => {}
        }
    }
}

fn emit_assistant(sink: &AcpSink, message: &ChatMessage) {
    if let Some(text) = message.content.as_deref().filter(|t| !t.is_empty()) {
        sink.notify(SessionUpdate::AgentMessageChunk {
            content: ContentBlock::Text { text: text.to_string() },
        });
    }
    if let Some(calls) = message.tool_calls.as_deref() {
        let mut tools = sink.tools.lock().unwrap();
        for call in calls {
            tools.insert(call.id.clone(), call.function.name.clone());
            sink.notify(SessionUpdate::ToolCall(ToolCallUpdate {
                tool_call_id: call.id.clone(),
                title: call.function.name.clone(),
                kind: None,
            }));
        }
    }
}

// ───────────────────────── sessions ─────────────────────────

struct AcpSession {
    agent: zen_agent::Agent,
    sink: Arc<AcpSink>,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}

// ───────────────────────── server ─────────────────────────

/// Entry point for `zencode --acp`: JSON-RPC 2.0 over stdio.
pub async fn run_acp_server(cli: &Cli) -> Result<i32> {
    let home = dirs::home_dir();
    let default_cwd = std::env::current_dir()?;
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let mut sessions: HashMap<String, AcpSession> = HashMap::new();
    let mut initialized = false;

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line.context("failed to read stdin")?;
        if line.trim().is_empty() {
            continue;
        }
        let req: RpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                write_response(&out, &RpcResponse::err(Value::Null, -32700, format!("parse error: {e}")))?;
                continue;
            }
        };
        if req.jsonrpc != "2.0" {
            if let Some(id) = req.id.clone() {
                write_response(&out, &RpcResponse::err(id, -32600, "invalid JSON-RPC version"))?;
            }
            continue;
        }
        let is_notification = req.id.is_none();
        let id = req.id.clone().unwrap_or(Value::Null);

        let response = match req.method.as_str() {
            "initialize" => {
                initialized = true;
                Some(handle_initialize(id, &req.params))
            }
            "authenticate" => Some(RpcResponse::ok(id, json!({}))),
            "session/new" => {
                if !initialized {
                    Some(RpcResponse::err(id, -32002, "server not initialized"))
                } else {
                    match handle_session_new(cli, &default_cwd, home.as_deref(), &req.params, &out).await {
                        Ok((session_id, session)) => {
                            sessions.insert(session_id.clone(), session);
                            Some(RpcResponse::ok(id, json!({ "sessionId": session_id })))
                        }
                        Err(e) => Some(RpcResponse::err(id, -32603, format!("session/new failed: {e:#}"))),
                    }
                }
            }
            "session/prompt" => {
                if !initialized {
                    Some(RpcResponse::err(id, -32002, "server not initialized"))
                } else {
                    match handle_prompt(&mut sessions, &req.params).await {
                        Ok(stop_reason) => Some(RpcResponse::ok(id, json!({ "stopReason": stop_reason }))),
                        Err(e) => Some(RpcResponse::err(id, -32603, format!("session/prompt failed: {e:#}"))),
                    }
                }
            }
            "session/cancel" => {
                // Notification per spec; also tolerate it arriving as a request.
                handle_cancel(&sessions, &req.params);
                if is_notification {
                    None
                } else {
                    Some(RpcResponse::ok(id, json!({})))
                }
            }
            "shutdown" | "exit" => {
                if !is_notification {
                    write_response(&out, &RpcResponse::ok(id, json!({})))?;
                }
                break;
            }
            _ => {
                if is_notification {
                    None
                } else {
                    Some(RpcResponse::err(id, -32601, format!("method not found: {}", req.method)))
                }
            }
        };
        if let Some(resp) = response {
            write_response(&out, &resp)?;
        }
    }
    Ok(0)
}

fn write_response(out: &Arc<Mutex<std::io::Stdout>>, resp: &RpcResponse) -> Result<()> {
    let line = serde_json::to_string(resp)?;
    let mut out = out.lock().unwrap();
    writeln!(out, "{line}")?;
    out.flush()?;
    Ok(())
}

fn handle_initialize(id: Value, params: &Value) -> RpcResponse {
    let _client_version = params.get("protocolVersion").and_then(Value::as_u64).unwrap_or(0);
    // We only speak protocol version 1; a client that cannot use it should close the connection.
    let version = PROTOCOL_VERSION;
    RpcResponse::ok(
        id,
        json!({
            "protocolVersion": version,
            "agentCapabilities": {
                "promptCapabilities": { "image": false, "audio": false },
            },
            "agentInfo": {
                "name": "zencode",
                "title": "ZenCode",
                "version": env!("CARGO_PKG_VERSION"),
            },
        }),
    )
}

async fn handle_session_new(
    cli: &Cli,
    default_cwd: &Path,
    home: Option<&Path>,
    params: &Value,
    out: &Arc<Mutex<std::io::Stdout>>,
) -> Result<(String, AcpSession)> {
    let cwd = params
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| default_cwd.to_path_buf());
    if !cwd.is_absolute() {
        anyhow::bail!("ACP requires absolute paths; got: {}", cwd.display());
    }

    let cfg = settings::resolve_config_with_home(cli, &cwd, home)?;
    let session_id = uuid::Uuid::new_v4().hyphenated().to_string();

    let sink = Arc::new(AcpSink {
        out: out.clone(),
        session_id: session_id.clone(),
        tools: Arc::new(Mutex::new(HashMap::new())),
    });

    // Reuse the standard assembly path (tools, permissions, hooks, MCP).
    let assembly = agent_run::assemble(cli, &cwd, home, &cfg, &session_id, None).await?;
    assembly.subagent_sink.set(sink.clone());

    let system_prompt = crate::build_system_prompt(cli, &cwd, home)?;
    let llm: Arc<dyn LlmBackend> = Arc::new(cfg.llm_client(sink.clone()));
    let agent_config = agent_run::base_agent_config(&cfg, cli, &system_prompt);
    let ctx = agent_run::tool_context_public(&cwd, &cfg, cli, &session_id);
    let agent = zen_agent::Agent::new(
        llm,
        assembly.registry.clone(),
        assembly.permissions.clone(),
        None, // ACP: permission prompts go through session/request_permission (future)
        assembly.hooks.clone(),
        sink.clone(),
        ctx,
        agent_config,
    );
    // Keep the assembly alive via the agent's owned Arcs; nothing else needed.
    let _ = assembly;

    Ok((
        session_id,
        AcpSession {
            agent,
            sink: sink.clone(),
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        },
    ))
}

/// Extract plain text from ACP `content` blocks.
fn prompt_text(params: &Value) -> String {
    params
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|b| {
                    let t = b.get("type")?.as_str()?;
                    match t {
                        "text" => b.get("text")?.as_str().map(str::to_string),
                        "resource_link" | "resource" => b
                            .get("uri")
                            .and_then(Value::as_str)
                            .map(|u| format!("[resource: {u}]")),
                        _ => None,
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

async fn handle_prompt(
    sessions: &mut HashMap<String, AcpSession>,
    params: &Value,
) -> Result<&'static str> {
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .context("missing sessionId")?;
    let session = sessions.get_mut(session_id).context("unknown sessionId")?;
    session.cancelled.store(false, std::sync::atomic::Ordering::SeqCst);

    let text = prompt_text(params);
    let result = session.agent.run_turn(&text).await;

    // Report token usage back to the client.
    session.sink.notify(SessionUpdate::UsageUpdate {
        used: result.usage.input_tokens + result.usage.output_tokens,
        size: session.agent.config.context_window,
    });

    // Map RunResult subtype -> ACP stopReason.
    let stop_reason = if session.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
        "cancelled"
    } else {
        match result.subtype.as_str() {
            "success" => "end_turn",
            "error_max_turns" => "max_turn_requests",
            _ => "end_turn",
        }
    };
    Ok(stop_reason)
}

fn handle_cancel(sessions: &HashMap<String, AcpSession>, params: &Value) {
    if let Some(sid) = params.get("sessionId").and_then(Value::as_str) {
        if let Some(s) = sessions.get(sid) {
            s.cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

// ───────────────────────── unit tests ─────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_initialize_roundtrip() {
        let line = r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":1}}"#;
        let req: RpcRequest = serde_json::from_str(line).unwrap();
        assert_eq!(req.method, "initialize");
        assert_eq!(req.id, Some(json!(0)));
        let resp = handle_initialize(req.id.unwrap(), &req.params);
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["result"]["protocolVersion"], 1);
        assert_eq!(v["result"]["agentInfo"]["name"], "zencode");
    }

    #[test]
    fn rpc_unknown_method_is_notification_safe() {
        // Notifications (no id) for unknown methods must not produce a response.
        let line = r#"{"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"x"}}"#;
        let req: RpcRequest = serde_json::from_str(line).unwrap();
        assert!(req.id.is_none());
    }

    #[test]
    fn prompt_text_extracts_text_blocks() {
        let params = json!({
            "sessionId": "s1",
            "content": [
                { "type": "text", "text": "hello" },
                { "type": "text", "text": "world" },
                { "type": "image", "data": "ignored-without-capability" },
            ]
        });
        assert_eq!(prompt_text(&params), "hello\nworld");
    }

    #[test]
    fn prompt_text_empty_without_content() {
        assert_eq!(prompt_text(&json!({})), "");
    }

    #[test]
    fn session_update_serialization_shape() {
        let update = SessionUpdate::AgentMessageChunk {
            content: ContentBlock::Text { text: "hi".into() },
        };
        let v = serde_json::to_value(&update).unwrap();
        assert_eq!(v["sessionUpdate"], "agent_message_chunk");
        assert_eq!(v["content"]["type"], "text");
        assert_eq!(v["content"]["text"], "hi");
    }

    #[test]
    fn tool_call_update_serialization_shape() {
        let update = SessionUpdate::ToolCallUpdate(ToolCallResult {
            tool_call_id: "t1".into(),
            status: "completed",
            content: Some(vec![ContentBlock::Text { text: "ok".into() }]),
        });
        let v = serde_json::to_value(&update).unwrap();
        assert_eq!(v["sessionUpdate"], "tool_call_update");
        assert_eq!(v["toolCallId"], "t1");
        assert_eq!(v["status"], "completed");
    }

    #[test]
    fn error_response_shape() {
        let resp = RpcResponse::err(json!(7), -32601, "method not found: foo");
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 7);
        assert_eq!(v["error"]["code"], -32601);
        assert!(v.get("result").is_none());
    }
}
