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
//! - `{"type":"response_error","v":1,"seq":N,"timestamp":T,"error":"..."}`
//!
//! None of these has a top-level `message`, so [`crate::session::load_messages`] and every
//! older reader skip them; old sessions without them load exactly as before.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use zen_core::{ChatMessage, ChatRequest, ChatResponse, LlmBackend, ToolDefinition};

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
        let seq = st.next_seq;
        st.next_seq += 1;
        let mut lines: Vec<Value> = vec![];
        let tools_hash = if req.tools.is_empty() {
            None
        } else {
            let h = fnv1a_hex(serde_json::to_string(&req.tools)?.as_bytes());
            if st.tool_hashes.insert(h.clone()) {
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
        st.chains.insert(key, (seq, req.messages.clone()));
        let path = st.path.clone();
        drop(st);
        for l in &lines {
            crate::session::append_record(&path, l)?;
        }
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
            Err(e) => json!({
                "type": "response_error",
                "v": EVENT_LOG_VERSION,
                "seq": seq,
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "error": e.to_string(),
            }),
        };
        if let Err(e) = crate::session::append_record(&path, &rec) {
            eprintln!("warning: could not record model response in {}: {}", path.display(), e);
        }
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
                seen.insert(seq, req.messages);
                out.push((seq, body));
            }
            _ => {}
        }
    }
    Ok(out)
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
            usage: Usage { prompt_tokens: 10, completion_tokens: 2, total_tokens: 12 },
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
}
