//! The session file as a complete event log (p0b, after dsh's "model-visible means logged").
//!
//! [`RecordingBackend`] wraps the model client. For every model call it appends to the session
//! transcript, before the call, a `request` record from which the exact request body can be
//! rebuilt, and after it a `response` (or `response_error`) record. Tool definitions are stored
//! once per distinct set in a `tools` record and referenced by hash.
//!
//! Messages are delta-encoded so the file grows linearly: a request names an earlier request of
//! the same conversation (`base`, matched by its first message, i.e. the system prompt, so a
//! sub-agent's requests form their own chain), how many of its messages it keeps (`keep`), and the
//! messages that follow (`append`). `body_hash` (FNV-1a 64 of the serialized body) lets
//! [`rebuild_requests`] check that what it rebuilt is byte-identical to what was sent.
//!
//! Records:
//! - `{"type":"tools","v":1,"hash":H,"tools":[...]}`
//! - `{"type":"request","v":1,"seq":N,"timestamp":T,"model":M,"max_tokens":X|null,"tools_hash":H|null,
//!    "base":S|null,"keep":K,"append":[...],"body_hash":B,"body_bytes":L}`
//! - `{"type":"response","v":1,"seq":N,"timestamp":T,"response":{"message":..,"usage":..,"finish_reason":..}}`
//! - `{"type":"response_error","v":1,"seq":N,"timestamp":T,"error":"...","request_id":"..."}`
//!   (`request_id` only when the provider sent one, P0-2)
//! - `{"type":"turn_assistant","v":1,"timestamp":T,"assistant":{..}}` and
//!   `{"type":"tool_result","v":1,"timestamp":T,"tool_use_id":I,"content":C,"is_error":E,"final":F}`
//!   (durable tool results): the main agent's turn as it happens, each tool result the moment it
//!   exists. [`resume_history`] uses them to resume a turn that died before it was persisted.
//!
//! None of these has a top-level `message`, so [`crate::session::load_messages`] and every
//! older reader skip them; old sessions without them load exactly as before.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use zen_core::{ChatMessage, ChatRequest, ChatResponse, LlmBackend, ToolDefinition, ToolJournal};

/// Event-log record version.
pub const EVENT_LOG_VERSION: u64 = 1;

/// FNV-1a 64-bit, as 16 hex digits (stable across Rust versions, no dependency).
pub fn fnv1a_hex(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:016x}", h)
}

#[derive(Default)]
struct State {
    /// Path the next records go to.
    path: PathBuf,
    /// False until the file was scanned for its last seq / known tool hashes.
    scanned: bool,
    next_seq: u64,
    tool_hashes: HashSet<String>,
    /// Chain key (hash of the first message) → (seq, messages) of its latest request.
    chains: HashMap<String, (u64, Vec<ChatMessage>)>,
}

/// Shared recorder: one per session, used by the main agent and its sub-agents.
pub struct Recorder {
    state: Mutex<State>,
}

impl Recorder {
    pub fn new(path: PathBuf) -> Arc<Self> {
        Arc::new(Self { state: Mutex::new(State { path, ..Default::default() }) })
    }

    /// Point the recorder at another session file (REPL `/clear`, `/resume`).
    pub fn set_path(&self, path: PathBuf) {
        *self.state.lock().unwrap() = State { path, ..Default::default() };
    }

    /// Append the `tools`/`request` records for `req`; returns its seq.
    fn record_request(&self, req: &ChatRequest) -> Result<u64> {
        let body = serde_json::to_string(req)?;
        let mut st = self.state.lock().unwrap();
        if !st.scanned {
            let (next, hashes) = scan(&st.path);
            st.next_seq = next;
            st.tool_hashes = hashes;
            st.scanned = true;
        }
        // Build all records before touching shared state. The sequence number is only
        // assigned (and `tool_hashes` only marked) after every append succeeds, so a failed
        // write can never leave the in-memory seq / hashes diverged from what's on disk,
        // and concurrent recorders can't interleave seq numbers with another's lines.
        let seq = st.next_seq;
        let mut lines: Vec<Value> = vec![];
        let tools_hash = if req.tools.is_empty() {
            None
        } else {
            let h = fnv1a_hex(serde_json::to_string(&req.tools)?.as_bytes());
            if !st.tool_hashes.contains(&h) {
                lines.push(json!({"type": "tools", "v": EVENT_LOG_VERSION, "hash": h, "tools": req.tools}));
            }
            Some(h)
        };
        let key = match req.messages.first() {
            Some(m) => fnv1a_hex(serde_json::to_string(m)?.as_bytes()),
            None => String::new(),
        };
        let (base, keep) = match st.chains.get(&key) {
            Some((s, prev)) => {
                let k = prev.iter().zip(&req.messages).take_while(|(a, b)| a == b).count();
                (Some(*s), k)
            }
            None => (None, 0),
        };
        lines.push(json!({
            "type": "request",
            "v": EVENT_LOG_VERSION,
            "seq": seq,
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "model": req.model,
            "max_tokens": req.max_tokens,
            "tools_hash": tools_hash,
            "base": base,
            "keep": keep,
            "append": &req.messages[keep..],
            "body_hash": fnv1a_hex(body.as_bytes()),
            "body_bytes": body.len(),
        }));
        let path = st.path.clone();
        // Hold the lock through the appends: seq assignment and the file write are atomic
        // with respect to other recorders, and state below only advances on success.
        for l in &lines {
            crate::session::append_record(&path, l)?;
        }
        st.next_seq = seq + 1;
        if let Some(h) = &tools_hash {
            st.tool_hashes.insert(h.clone());
        }
        st.chains.insert(key, (seq, req.messages.clone()));
        Ok(seq)
    }

    fn record_response(&self, seq: u64, res: &Result<ChatResponse>) {
        let path = self.state.lock().unwrap().path.clone();
        let rec = match res {
            Ok(r) => json!({
                "type": "response",
                "v": EVENT_LOG_VERSION,
                "seq": seq,
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "response": {"message": r.message, "usage": r.usage, "finish_reason": r.finish_reason},
            }),
            Err(e) => {
                let error = e.to_string();
                let mut rec = json!({
                    "type": "response_error",
                    "v": EVENT_LOG_VERSION,
                    "seq": seq,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                    "error": error,
                });
                // The provider's request id, for support tickets (P0-2; add-only field).
                if let Some(id) = zen_agent::request_id_from_error(&error) {
                    rec["request_id"] = json!(id);
                }
                rec
            }
        };
        if let Err(e) = crate::session::append_record(&path, &rec) {
            eprintln!("warning: could not record model response in {}: {}", path.display(), e);
        }
    }
}

impl Recorder {
    /// Append one journal record as a single write of a whole line, under the recorder lock so
    /// parallel tools never interleave. A failed write warns and the turn goes on.
    fn journal(&self, rec: Value) {
        let st = self.state.lock().unwrap();
        let res = (|| -> Result<()> {
            use std::io::Write;
            if let Some(parent) = st.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&st.path)?;
            let mut line = serde_json::to_string(&rec)?;
            line.push('\n');
            f.write_all(line.as_bytes())?;
            f.flush()?;
            Ok(())
        })();
        if let Err(e) = res {
            eprintln!("warning: could not record tool result in {}: {}", st.path.display(), e);
        }
    }
}

impl ToolJournal for Recorder {
    fn assistant(&self, message: &ChatMessage) {
        self.journal(json!({
            "type": "turn_assistant",
            "v": EVENT_LOG_VERSION,
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "assistant": message,
        }));
    }

    fn tool_result(&self, tool_use_id: &str, content: &str, is_error: bool, final_result: bool) {
        self.journal(json!({
            "type": "tool_result",
            "v": EVENT_LOG_VERSION,
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "tool_use_id": tool_use_id,
            "content": content,
            "is_error": is_error,
            "final": final_result,
        }));
    }
}

/// Next seq and the tool hashes already in `path` (a resumed session keeps numbering).
fn scan(path: &Path) -> (u64, HashSet<String>) {
    let mut next = 0;
    let mut hashes = HashSet::new();
    let Ok(text) = std::fs::read_to_string(path) else { return (next, hashes) };
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("request") => {
                if let Some(s) = v.get("seq").and_then(|s| s.as_u64()) {
                    next = next.max(s + 1);
                }
            }
            Some("tools") => {
                if let Some(h) = v.get("hash").and_then(|h| h.as_str()) {
                    hashes.insert(h.to_string());
                }
            }
            _ => {}
        }
    }
    (next, hashes)
}

/// An [`LlmBackend`] that records every request and response to the session file.
pub struct RecordingBackend {
    pub inner: Arc<dyn LlmBackend>,
    pub recorder: Arc<Recorder>,
}

#[async_trait]
impl LlmBackend for RecordingBackend {
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        // A failed write must not end the run: warn and send the request anyway.
        let seq = match self.recorder.record_request(&req) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("warning: could not record model request: {}", e);
                None
            }
        };
        let res = self.inner.chat(req).await;
        if let Some(seq) = seq {
            self.recorder.record_response(seq, &res);
        }
        res
    }
}

/// Wrap `inner` when the session has a recorder.
pub fn wrap(inner: Arc<dyn LlmBackend>, recorder: Option<&Arc<Recorder>>) -> Arc<dyn LlmBackend> {
    match recorder {
        Some(r) => Arc::new(RecordingBackend { inner, recorder: r.clone() }),
        None => inner,
    }
}

/// Rebuild every recorded request body in `path`, in order, as `(seq, body)`. Each body is
/// checked against its `body_hash`. Sessions written before p0b have no request records and
/// give an empty list.
pub fn rebuild_requests(path: &Path) -> Result<Vec<(u64, String)>> {
    Ok(rebuild(path)?.into_iter().map(|(seq, _, body)| (seq, body)).collect())
}

/// [`rebuild_requests`] keeping each request's messages too.
fn rebuild(path: &Path) -> Result<Vec<(u64, Vec<ChatMessage>, String)>> {
    let text = std::fs::read_to_string(path)?;
    let mut tools: HashMap<String, Vec<ToolDefinition>> = HashMap::new();
    let mut seen: HashMap<u64, Vec<ChatMessage>> = HashMap::new();
    let mut out = vec![];
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(line).map_err(|e| anyhow!("{}:{}: {}", path.display(), n + 1, e))?;
        match v.get("type").and_then(|t| t.as_str()) {
            Some("tools") => {
                let h = v["hash"].as_str().unwrap_or_default().to_string();
                tools.insert(h, serde_json::from_value(v["tools"].clone())?);
            }
            Some("request") => {
                let seq = v["seq"].as_u64().ok_or_else(|| anyhow!("line {}: request without seq", n + 1))?;
                let mut messages: Vec<ChatMessage> = match v["base"].as_u64() {
                    Some(b) => {
                        let prev = seen.get(&b).ok_or_else(|| anyhow!("request {seq}: unknown base {b}"))?;
                        let keep = v["keep"].as_u64().unwrap_or(0) as usize;
                        prev.get(..keep).ok_or_else(|| anyhow!("request {seq}: keep {keep} > base length"))?.to_vec()
                    }
                    None => vec![],
                };
                messages.extend(serde_json::from_value::<Vec<ChatMessage>>(v["append"].clone())?);
                let tool_defs = match v["tools_hash"].as_str() {
                    Some(h) => tools.get(h).cloned().ok_or_else(|| anyhow!("request {seq}: unknown tools {h}"))?,
                    None => vec![],
                };
                let req = ChatRequest {
                    model: v["model"].as_str().unwrap_or_default().to_string(),
                    messages,
                    tools: tool_defs,
                    max_tokens: v["max_tokens"].as_u64().map(|x| x as u32),
                };
                let body = serde_json::to_string(&req)?;
                let want = v["body_hash"].as_str().unwrap_or_default();
                if fnv1a_hex(body.as_bytes()) != want {
                    return Err(anyhow!("request {seq}: rebuilt body does not match body_hash"));
                }
                seen.insert(seq, req.messages.clone());
                out.push((seq, req.messages, body));
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Tool message for a call that started but has no recorded result.
pub const INTERRUPTED_TOOL_RESULT: &str =
    "[Tool call interrupted: the session ended before this tool finished; it may or may not have taken effect.]";

/// The history to resume `path` with, as `(committed, history)`.
///
/// `committed` is [`crate::session::load_history`]. When the records after the last committed
/// message hold a turn that never finished (crash, kill, timeout, Ctrl-C), `history` is that turn
/// rebuilt from the log: the last request of the main conversation, then the journaled assistant
/// message and its tool results (final, else raw, else [`INTERRUPTED_TOOL_RESULT`]). Otherwise
/// `history == committed` (always so for sessions without journal records). The caller persists
/// `history` past `committed` so the next resume sees it as ordinary messages.
pub fn resume_history(path: &Path) -> Result<(Vec<ChatMessage>, Vec<ChatMessage>)> {
    let committed = crate::session::load_history(path)?;
    let history = match interrupted_turn(path) {
        Ok(Some(h)) => h,
        Ok(None) => committed.clone(),
        Err(e) => {
            eprintln!("warning: could not recover the interrupted turn of {}: {}", path.display(), e);
            committed.clone()
        }
    };
    Ok((committed, history))
}

fn interrupted_turn(path: &Path) -> Result<Option<Vec<ChatMessage>>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)?;
    let recs: Vec<Value> = text.lines().filter_map(|l| serde_json::from_str(l.trim()).ok()).collect();
    let kind = |v: &Value| v.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string();
    let committed_at = recs.iter().rposition(|v| v.get("message").is_some() || kind(v) == "summary");
    let tail = &recs[committed_at.map_or(0, |i| i + 1)..];
    // The turn's last assistant message, and the main-conversation request that produced it.
    let Some(ai) = tail.iter().rposition(|v| kind(v) == "turn_assistant") else { return Ok(None) };
    let Some(ri) = tail[..ai].iter().rposition(|v| kind(v) == "request") else { return Ok(None) };
    let seq_of = |v: &Value| v["seq"].as_u64();
    let requests = rebuild(path)?;
    let msgs_of = |seq: Option<u64>| requests.iter().find(|(s, _, _)| Some(*s) == seq).map(|(_, m, _)| m.clone());
    let Some(main) = msgs_of(seq_of(&tail[ri])) else { return Ok(None) };
    // A later request of the same conversation (same first message; sub-agents and compaction
    // calls have their own) already holds the assistant message and every result.
    let later = tail[ai + 1..]
        .iter()
        .filter(|v| kind(v) == "request")
        .filter_map(|v| msgs_of(seq_of(v)))
        .rfind(|m| m.first() == main.first());
    let mut messages = match later {
        Some(m) => m,
        None => {
            let assistant: ChatMessage = serde_json::from_value(tail[ai]["assistant"].clone())?;
            let mut m = main;
            m.push(assistant.clone());
            for call in assistant.tool_calls.iter().flatten() {
                let mut found: Option<String> = None;
                for v in &tail[ai + 1..] {
                    if kind(v) == "tool_result" && v["tool_use_id"].as_str() == Some(call.id.as_str()) {
                        let is_final = v["final"].as_bool().unwrap_or(false);
                        if is_final || found.is_none() {
                            found = v["content"].as_str().map(String::from);
                        }
                        if is_final {
                            break;
                        }
                    }
                }
                m.push(ChatMessage::tool(call.id.clone(), found.unwrap_or_else(|| INTERRUPTED_TOOL_RESULT.to_string())));
            }
            m
        }
    };
    if messages.first().is_some_and(|m| m.role == "system") {
        messages.remove(0);
    }
    Ok(Some(messages))
}

/// The body of the last recorded request, if any.
pub fn rebuild_last_request(path: &Path) -> Result<Option<String>> {
    Ok(rebuild_requests(path)?.pop().map(|(_, b)| b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use zen_core::*;

    /// Answers from a script and keeps the exact body of every request (what the HTTP client
    /// would send: `serde_json` of the request).
    struct Scripted {
        replies: Mutex<Vec<ChatResponse>>,
        bodies: Mutex<Vec<String>>,
    }
    #[async_trait]
    impl LlmBackend for Scripted {
        async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
            self.bodies.lock().unwrap().push(serde_json::to_string(&req).unwrap());
            let mut r = self.replies.lock().unwrap();
            if r.is_empty() {
                return Err(anyhow!("script exhausted"));
            }
            Ok(r.remove(0))
        }
    }

    fn reply(content: Option<&str>, call: Option<(&str, &str, &str)>) -> ChatResponse {
        ChatResponse {
            message: ChatMessage::assistant(
                content.map(String::from),
                call.map(|(id, name, args)| {
                    vec![ToolCall {
                        id: id.into(),
                        call_type: "function".into(),
                        function: FunctionCall { name: name.into(), arguments: args.into() },
                    }]
                }),
            ),
            usage: Usage { prompt_tokens: 10, completion_tokens: 2, total_tokens: 12, cache_read_input_tokens: 0, cache_creation_input_tokens: 0 },
            finish_reason: Some("stop".into()),
        }
    }

    struct AllowAll;
    impl PermissionChecker for AllowAll {
        fn check(&self, _: &str, _: &Value, _: bool, _: &ToolContext) -> PermissionDecision {
            PermissionDecision::Allow
        }
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("zen_eventlog_{}_{}", tag, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// P0-2: a provider request id lands in the session file's `response_error` record, and the
    /// API key never does.
    #[tokio::test]
    async fn p0_2_request_id_is_recorded_in_the_session_and_the_key_is_not() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 65536];
                let mut got = Vec::new();
                loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    got.extend_from_slice(&buf[..n]);
                    let txt = String::from_utf8_lossy(&got).to_string();
                    let done = txt.find("\r\n\r\n").is_some_and(|h| {
                        let len = txt[..h]
                            .lines()
                            .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                            .unwrap_or(0);
                        got.len() >= h + 4 + len
                    });
                    if done || n == 0 {
                        break;
                    }
                }
                let body = r#"{"error":{"message":"model not found"}}"#;
                let resp = format!(
                    "HTTP/1.1 404 X\r\nContent-Type: application/json\r\nx-request-id: abc\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        let dir = tmp("reqid");
        let transcript = dir.join("sess.jsonl");
        let recorder = Recorder::new(transcript.clone());
        let client: Arc<dyn LlmBackend> = Arc::new(zen_agent::OpenAiClient::new(
            url,
            Some("sk-live-TOPSECRET".into()),
            Arc::new(NullSink),
        ));
        let backend = wrap(client, Some(&recorder));
        let req = ChatRequest { model: "m".into(), messages: vec![ChatMessage::user("hi")], tools: vec![], max_tokens: None };
        let err = backend.chat(req).await.unwrap_err().to_string();
        assert!(err.contains("[request id: abc]"), "{err}");
        let text = std::fs::read_to_string(&transcript).unwrap();
        let rec: Value = text
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .find(|v| v["type"] == "response_error")
            .expect("response_error record");
        assert_eq!(rec["request_id"], "abc");
        assert!(rec["error"].as_str().unwrap().contains("request id: abc"));
        assert!(!text.contains("TOPSECRET") && !text.to_ascii_lowercase().contains("authorization"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn p0b_3_replay_rebuilds_last_request_byte_identical() {
        let dir = tmp("replay");
        std::fs::write(dir.join("notes.txt"), "alpha\nbeta \"quoted\" ünïcode\n").unwrap();
        let transcript = dir.join("sess.jsonl");
        let scripted = Arc::new(Scripted {
            replies: Mutex::new(vec![
                reply(None, Some(("c1", "Read", r#"{"file_path":"notes.txt"}"#))),
                reply(Some("It says alpha."), None),
                reply(None, Some(("c2", "Read", r#"{"file_path":"notes.txt","offset":2}"#))),
                reply(Some("Line 2 is beta."), None),
            ]),
            bodies: Mutex::new(vec![]),
        });
        let recorder = Recorder::new(transcript.clone());
        let llm = wrap(scripted.clone(), Some(&recorder));
        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(zen_tools::ReadTool));
        let ctx = ToolContext::new(dir.clone(), "sess");
        let config = zen_agent::AgentConfig { system_prompt: "SYSTEM".into(), auto_compact_threshold: None, ..Default::default() };
        let mut agent = zen_agent::Agent::new(
            llm,
            tools,
            Arc::new(AllowAll),
            None,
            Arc::new(NoHooks),
            Arc::new(NullSink),
            ctx,
            config,
        );
        agent.journal = Some(recorder.clone());
        let mut writer = crate::session::TranscriptWriter::new(transcript.clone(), "sess", &dir);
        for prompt in ["what is in notes.txt?", "and line 2?"] {
            let before = agent.messages().to_vec();
            let res = agent.run_turn(prompt).await;
            assert!(!res.is_error, "{}", res.result);
            writer.persist_turn(&before, agent.messages()).unwrap();
        }
        // A sub-agent style request (other system prompt) in between must not break the chain.
        let side = ChatRequest {
            model: "m2".into(),
            messages: vec![ChatMessage::system("SUB"), ChatMessage::user("x")],
            tools: vec![],
            max_tokens: Some(64),
        };
        let _ = recorder.record_request(&side).unwrap();

        let sent = scripted.bodies.lock().unwrap().clone();
        assert_eq!(sent.len(), 4);
        let rebuilt = rebuild_requests(&transcript).unwrap();
        assert_eq!(rebuilt.len(), 5);
        for (i, body) in sent.iter().enumerate() {
            assert_eq!(&rebuilt[i].1, body, "request {} differs", i);
        }
        assert_eq!(rebuilt[4].1, serde_json::to_string(&side).unwrap());
        // The last request of the main conversation, rebuilt from the file alone.
        assert_eq!(rebuilt[3].1.as_bytes(), sent.last().unwrap().as_bytes());
        assert_eq!(rebuild_last_request(&transcript).unwrap().unwrap(), serde_json::to_string(&side).unwrap());

        // Linear growth: tools stored once, later requests only append new messages.
        let text = std::fs::read_to_string(&transcript).unwrap();
        let recs: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(recs.iter().filter(|r| r["type"] == "tools").count(), 1);
        let reqs: Vec<&Value> = recs.iter().filter(|r| r["type"] == "request").collect();
        assert!(reqs[0]["base"].is_null());
        assert_eq!(reqs[3]["base"], 2);
        assert_eq!(reqs[3]["append"].as_array().unwrap().len(), 2, "assistant call + tool result");
        assert_eq!(recs.iter().filter(|r| r["type"] == "response").count(), 4);

        // The history a resume loads is unchanged by the new records.
        let history = crate::session::load_history(&transcript).unwrap();
        assert_eq!(history, agent.messages());
        // Durable tool results: a completed session has journal records, and they change nothing.
        assert!(recs.iter().any(|r| r["type"] == "tool_result"));
        assert!(recs.iter().any(|r| r["type"] == "turn_assistant"));
        assert_eq!(resume_history(&transcript).unwrap(), (history.clone(), history));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn trunc_continuations_are_logged_and_replay_byte_identical() {
        let dir = tmp("trunc");
        std::fs::write(dir.join("notes.txt"), "alpha\n").unwrap();
        let transcript = dir.join("sess.jsonl");
        let cut = |mut r: ChatResponse| {
            r.finish_reason = Some("length".into());
            r
        };
        let scripted = Arc::new(Scripted {
            replies: Mutex::new(vec![
                cut(reply(Some("Let me look at the fi"), None)),
                cut(reply(Some("le first. "), Some(("c0", "Read", r#"{"file_path":"no"#)))),
                reply(None, Some(("c1", "Read", r#"{"file_path":"notes.txt"}"#))),
                cut(reply(Some("It says al"), None)),
                reply(Some("pha."), None),
            ]),
            bodies: Mutex::new(vec![]),
        });
        let recorder = Recorder::new(transcript.clone());
        let llm = wrap(scripted.clone(), Some(&recorder));
        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(zen_tools::ReadTool));
        let ctx = ToolContext::new(dir.clone(), "sess");
        let config = zen_agent::AgentConfig { system_prompt: "SYSTEM".into(), auto_compact_threshold: None, ..Default::default() };
        let mut agent =
            zen_agent::Agent::new(llm, tools, Arc::new(AllowAll), None, Arc::new(NoHooks), Arc::new(NullSink), ctx, config);
        agent.journal = Some(recorder.clone());
        let mut writer = crate::session::TranscriptWriter::new(transcript.clone(), "sess", &dir);
        let res = agent.run_turn("what is in notes.txt?").await;
        assert!(!res.is_error, "{}", res.result);
        assert_eq!(res.result, "It says alpha.");
        writer.persist_turn(&[], agent.messages()).unwrap();

        // Every continuation is a recorded request; replay rebuilds each one byte-identically.
        let sent = scripted.bodies.lock().unwrap().clone();
        assert_eq!(sent.len(), 5);
        let rebuilt = rebuild_requests(&transcript).unwrap();
        assert_eq!(rebuilt.len(), 5);
        for (i, body) in sent.iter().enumerate() {
            assert_eq!(&rebuilt[i].1, body, "request {} differs", i);
        }
        assert!(sent[1].contains("cut off by the output token limit. Continue exactly"));
        assert!(sent[2].contains("in the middle of a tool call, so no tool was run"));
        // The raw cut-off responses (half call included) are in the log with their finish_reason;
        // the half call never reaches a request.
        let text = std::fs::read_to_string(&transcript).unwrap();
        let recs: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let reasons: Vec<&str> = recs
            .iter()
            .filter(|r| r["type"] == "response")
            .map(|r| r["response"]["finish_reason"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(reasons, ["length", "length", "stop", "length", "stop"]);
        assert!(!sent.iter().any(|b| b.contains("\"c0\"")));
        // Resume sees exactly the committed history.
        let history = crate::session::load_history(&transcript).unwrap();
        assert_eq!(history, agent.messages());
        assert_eq!(resume_history(&transcript).unwrap(), (history.clone(), history));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn p0b_3_resumed_session_continues_seq_and_old_sessions_still_load() {
        let dir = tmp("old");
        let path = dir.join("old.jsonl");
        // A pre-p0b session: messages only.
        let mut w = crate::session::TranscriptWriter::new(path.clone(), "old", &dir);
        let msgs = vec![ChatMessage::user("hi"), ChatMessage::assistant(Some("hello".into()), None)];
        w.persist_turn(&[], &msgs).unwrap();
        assert!(rebuild_requests(&path).unwrap().is_empty());
        assert_eq!(rebuild_last_request(&path).unwrap(), None);
        assert_eq!(crate::session::load_history(&path).unwrap(), msgs);

        // Resuming appends request records numbered after any existing ones.
        let req = |m: &str| ChatRequest {
            model: "m".into(),
            messages: vec![ChatMessage::user("hi"), ChatMessage::user(m)],
            tools: vec![],
            max_tokens: None,
        };
        let r1 = Recorder::new(path.clone());
        assert_eq!(r1.record_request(&req("a")).unwrap(), 0);
        let r2 = Recorder::new(path.clone());
        assert_eq!(r2.record_request(&req("b")).unwrap(), 1);
        let rebuilt = rebuild_requests(&path).unwrap();
        assert_eq!(rebuilt[1].1, serde_json::to_string(&req("b")).unwrap());
        assert_eq!(crate::session::load_history(&path).unwrap(), msgs);
        // Durable tool results: an old-format session resumes exactly as it loads.
        assert_eq!(resume_history(&path).unwrap(), (msgs.clone(), msgs));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn p0b_3_tampered_record_is_detected() {
        let dir = tmp("tamper");
        let path = dir.join("t.jsonl");
        let r = Recorder::new(path.clone());
        r.record_request(&ChatRequest { model: "m".into(), messages: vec![ChatMessage::user("x")], tools: vec![], max_tokens: None })
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap().replace("\"model\":\"m\"", "\"model\":\"n\"");
        std::fs::write(&path, text).unwrap();
        assert!(rebuild_requests(&path).unwrap_err().to_string().contains("body_hash"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn reply_calls(calls: &[(&str, &str, &str)]) -> ChatResponse {
        let mut r = reply(None, None);
        r.message.tool_calls = Some(
            calls
                .iter()
                .map(|(id, name, args)| ToolCall {
                    id: (*id).into(),
                    call_type: "function".into(),
                    function: FunctionCall { name: (*name).into(), arguments: (*args).into() },
                })
                .collect(),
        );
        r
    }

    /// A tool that never returns (the turn dies while it runs). Named `Glob` it is read-only and
    /// parallel-safe, so it is batched with a Read; named `Hang` it runs alone after it.
    #[derive(Clone, Copy)]
    struct Hang(&'static str, bool);
    #[async_trait]
    impl Tool for Hang {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            "never returns"
        }
        fn parameters_schema(&self) -> Value {
            json!({"type": "object"})
        }
        fn is_read_only(&self) -> bool {
            self.1
        }
        async fn call(&self, _: Value, _: &ToolContext) -> ToolOutput {
            std::future::pending().await
        }
    }

    fn journaled_agent(dir: &Path, llm: Arc<dyn LlmBackend>, recorder: &Arc<Recorder>, hang: Hang) -> zen_agent::Agent {
        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(zen_tools::ReadTool));
        tools.register(Arc::new(hang));
        let config = zen_agent::AgentConfig { system_prompt: "SYSTEM".into(), auto_compact_threshold: None, ..Default::default() };
        let mut agent = zen_agent::Agent::new(
            llm,
            tools,
            Arc::new(AllowAll),
            None,
            Arc::new(NoHooks),
            Arc::new(NullSink),
            ToolContext::new(dir.to_path_buf(), "sess"),
            config,
        );
        agent.journal = Some(recorder.clone());
        agent
    }

    /// The turn is killed after 1 of its 2 tool calls finished; resuming keeps that result, marks
    /// the other one interrupted, and the next request the model gets carries both.
    async fn killed_after_one_of_two_tools(hang: Hang) {
        let dir = tmp("dtr");
        std::fs::write(dir.join("notes.txt"), "alpha-durable\n").unwrap();
        let path = dir.join("sess.jsonl");
        let hang_name = hang.0;
        let scripted = Arc::new(Scripted {
            replies: Mutex::new(vec![
                reply(Some("hi"), None),
                reply_calls(&[("c1", "Read", r#"{"file_path":"notes.txt"}"#), ("c2", hang_name, "{}")]),
            ]),
            bodies: Mutex::new(vec![]),
        });
        let recorder = Recorder::new(path.clone());
        let mut agent = journaled_agent(&dir, wrap(scripted.clone(), Some(&recorder)), &recorder, hang);
        let mut writer = crate::session::TranscriptWriter::new(path.clone(), "sess", &dir);
        // Turn 1 completes and is persisted.
        let before = agent.messages().to_vec();
        assert!(!agent.run_turn("hello").await.is_error);
        writer.persist_turn(&before, agent.messages()).unwrap();
        let turn1 = agent.messages().to_vec();
        // Turn 2 dies while the second tool runs: the future is dropped, nothing is persisted.
        let killed = tokio::time::timeout(std::time::Duration::from_millis(800), agent.run_turn("read it")).await;
        assert!(killed.is_err(), "turn should still be running");
        drop(agent);

        assert_eq!(crate::session::load_history(&path).unwrap(), turn1, "committed history unchanged");
        let (committed, history) = resume_history(&path).unwrap();
        assert_eq!(committed, turn1);
        assert_eq!(history.len(), turn1.len() + 4, "{history:#?}");
        assert_eq!(&history[..turn1.len()], &turn1[..]);
        assert_eq!(history[turn1.len()], ChatMessage::user("read it"));
        assert_eq!(history[turn1.len() + 1].tool_calls.as_ref().unwrap().len(), 2);
        let r1 = &history[turn1.len() + 2];
        assert_eq!(r1.tool_call_id.as_deref(), Some("c1"));
        assert!(r1.content.as_deref().unwrap().contains("alpha-durable"), "{r1:?}");
        assert_eq!(history[turn1.len() + 3], ChatMessage::tool("c2", INTERRUPTED_TOOL_RESULT));
        // Completed requests still replay byte-identically with the journal records in the file.
        let sent = scripted.bodies.lock().unwrap().clone();
        let rebuilt = rebuild_requests(&path).unwrap();
        assert_eq!(rebuilt.iter().map(|(_, b)| b.clone()).collect::<Vec<_>>(), sent);

        // Resume: the recovered turn is persisted, the next request carries the finished result.
        let mut w2 = crate::session::TranscriptWriter::new(path.clone(), "sess", &dir);
        w2.persist_turn(&committed, &history).unwrap();
        assert_eq!(crate::session::load_history(&path).unwrap(), history);
        assert_eq!(resume_history(&path).unwrap().1, history, "nothing recovered twice");
        let resumed_llm = Arc::new(Scripted { replies: Mutex::new(vec![reply(Some("done"), None)]), bodies: Mutex::new(vec![]) });
        let r2 = Recorder::new(path.clone());
        let mut resumed = journaled_agent(&dir, wrap(resumed_llm.clone(), Some(&r2)), &r2, Hang(hang_name, hang.1));
        resumed.set_messages(history.clone());
        let res = resumed.run_turn("continue").await;
        assert!(!res.is_error, "{}", res.result);
        let body = resumed_llm.bodies.lock().unwrap()[0].clone();
        assert!(body.contains("alpha-durable"), "{body}");
        assert!(body.contains("Tool call interrupted"), "{body}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn dtr_killed_after_one_of_two_sequential_tools_resume_keeps_the_finished_result() {
        killed_after_one_of_two_tools(Hang("Hang", false)).await;
    }

    #[tokio::test]
    async fn dtr_killed_mid_parallel_batch_resume_keeps_the_finished_result() {
        killed_after_one_of_two_tools(Hang("Glob", true)).await;
    }
}
