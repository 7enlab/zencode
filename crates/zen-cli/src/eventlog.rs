use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use zen_core::{ChatMessage, ChatRequest, ChatResponse, LlmBackend, ToolDefinition, ToolJournal};

pub const EVENT_LOG_VERSION: u64 = 1;

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

    path: PathBuf,

    scanned: bool,
    next_seq: u64,
    tool_hashes: HashSet<String>,

    chains: HashMap<String, (u64, Vec<ChatMessage>)>,
}

pub struct Recorder {
    state: Mutex<State>,
}

impl Recorder {
    pub fn new(path: PathBuf) -> Arc<Self> {
        Arc::new(Self { state: Mutex::new(State { path, ..Default::default() }) })
    }

    pub fn set_path(&self, path: PathBuf) {
        *self.state.lock().unwrap() = State { path, ..Default::default() };
    }

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

pub struct RecordingBackend {
    pub inner: Arc<dyn LlmBackend>,
    pub recorder: Arc<Recorder>,
}

#[async_trait]
impl LlmBackend for RecordingBackend {
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {

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

pub fn wrap(inner: Arc<dyn LlmBackend>, recorder: Option<&Arc<Recorder>>) -> Arc<dyn LlmBackend> {
    match recorder {
        Some(r) => Arc::new(RecordingBackend { inner, recorder: r.clone() }),
        None => inner,
    }
}

pub fn rebuild_requests(path: &Path) -> Result<Vec<(u64, String)>> {
    Ok(rebuild(path)?.into_iter().map(|(seq, _, body)| (seq, body)).collect())
}

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

pub const INTERRUPTED_TOOL_RESULT: &str =
    "[Tool call interrupted: the session ended before this tool finished; it may or may not have taken effect.]";

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

    let Some(ai) = tail.iter().rposition(|v| kind(v) == "turn_assistant") else { return Ok(None) };
    let Some(ri) = tail[..ai].iter().rposition(|v| kind(v) == "request") else { return Ok(None) };
    let seq_of = |v: &Value| v["seq"].as_u64();
    let requests = rebuild(path)?;
    let msgs_of = |seq: Option<u64>| requests.iter().find(|(s, _, _)| Some(*s) == seq).map(|(_, m, _)| m.clone());
    let Some(main) = msgs_of(seq_of(&tail[ri])) else { return Ok(None) };

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

pub fn rebuild_last_request(path: &Path) -> Result<Option<String>> {
    Ok(rebuild_requests(path)?.pop().map(|(_, b)| b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use zen_core::*;

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

        assert_eq!(rebuilt[3].1.as_bytes(), sent.last().unwrap().as_bytes());
        assert_eq!(rebuild_last_request(&transcript).unwrap().unwrap(), serde_json::to_string(&side).unwrap());

        let text = std::fs::read_to_string(&transcript).unwrap();
        let recs: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(recs.iter().filter(|r| r["type"] == "tools").count(), 1);
        let reqs: Vec<&Value> = recs.iter().filter(|r| r["type"] == "request").collect();
        assert!(reqs[0]["base"].is_null());
        assert_eq!(reqs[3]["base"], 2);
        assert_eq!(reqs[3]["append"].as_array().unwrap().len(), 2, "assistant call + tool result");
        assert_eq!(recs.iter().filter(|r| r["type"] == "response").count(), 4);

        let history = crate::session::load_history(&transcript).unwrap();
        assert_eq!(history, agent.messages());

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

        let sent = scripted.bodies.lock().unwrap().clone();
        assert_eq!(sent.len(), 5);
        let rebuilt = rebuild_requests(&transcript).unwrap();
        assert_eq!(rebuilt.len(), 5);
        for (i, body) in sent.iter().enumerate() {
            assert_eq!(&rebuilt[i].1, body, "request {} differs", i);
        }
        assert!(sent[1].contains("cut off by the output token limit. Continue exactly"));
        assert!(sent[2].contains("in the middle of a tool call, so no tool was run"));

        let text = std::fs::read_to_string(&transcript).unwrap();
        let recs: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let reasons: Vec<&str> = recs
            .iter()
            .filter(|r| r["type"] == "response")
            .map(|r| r["response"]["finish_reason"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(reasons, ["length", "length", "stop", "length", "stop"]);
        assert!(!sent.iter().any(|b| b.contains("\"c0\"")));

        let history = crate::session::load_history(&transcript).unwrap();
        assert_eq!(history, agent.messages());
        assert_eq!(resume_history(&transcript).unwrap(), (history.clone(), history));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn p0b_3_resumed_session_continues_seq_and_old_sessions_still_load() {
        let dir = tmp("old");
        let path = dir.join("old.jsonl");

        let mut w = crate::session::TranscriptWriter::new(path.clone(), "old", &dir);
        let msgs = vec![ChatMessage::user("hi"), ChatMessage::assistant(Some("hello".into()), None)];
        w.persist_turn(&[], &msgs).unwrap();
        assert!(rebuild_requests(&path).unwrap().is_empty());
        assert_eq!(rebuild_last_request(&path).unwrap(), None);
        assert_eq!(crate::session::load_history(&path).unwrap(), msgs);

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

        let before = agent.messages().to_vec();
        assert!(!agent.run_turn("hello").await.is_error);
        writer.persist_turn(&before, agent.messages()).unwrap();
        let turn1 = agent.messages().to_vec();

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

        let sent = scripted.bodies.lock().unwrap().clone();
        let rebuilt = rebuild_requests(&path).unwrap();
        assert_eq!(rebuilt.iter().map(|(_, b)| b.clone()).collect::<Vec<_>>(), sent);

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
