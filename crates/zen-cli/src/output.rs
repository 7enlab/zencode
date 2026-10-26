
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use zen_core::{AgentEvent, RunResult};






pub struct Buffered {
    pub out: Arc<Mutex<Vec<u8>>>,
    passthrough: bool,
}
impl Buffered {
    pub fn new() -> Self {
        Self { out: Arc::new(Mutex::new(Vec::new())), passthrough: false }
    }

    pub fn stdout() -> Self {
        Self { out: Arc::new(Mutex::new(Vec::new())), passthrough: true }
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.out.lock().unwrap()).to_string()
    }
    pub fn write_str(&self, s: &str) {
        self.write_bytes(s.as_bytes());
    }
    pub fn write_line(&self, s: &str) {
        let mut line = Vec::with_capacity(s.len() + 1);
        line.extend_from_slice(s.as_bytes());
        line.push(b'\n');
        self.write_bytes(&line);
    }
    fn write_bytes(&self, bytes: &[u8]) {


        let mut g = self.out.lock().unwrap();
        if self.passthrough {
            let mut so = std::io::stdout().lock();

            let _ = so.write_all(bytes).and_then(|_| so.flush());
        } else {
            g.write_all(bytes).unwrap();
        }
    }
}
impl Default for Buffered {
    fn default() -> Self {
        Self::new()
    }
}



pub struct TextSink {
    pub buf: Arc<Buffered>,
    pub is_repl: bool,
}

impl TextSink {
    pub fn new_repl(buf: Arc<Buffered>) -> Self {
        Self { buf, is_repl: true }
    }
    pub fn new_headless(buf: Arc<Buffered>) -> Self {
        Self { buf, is_repl: false }
    }

    fn tool_short(tool_name: &str) -> &str {
        match tool_name {
            "Read" => "Read",
            "Write" => "Write",
            "Edit" => "Edit",
            "Bash" => "Bash",
            "Glob" => "Glob",
            "Grep" => "Grep",
            "WebFetch" => "WebFetch",
            "TodoWrite" => "TodoWrite",
            "Skill" => "Skill",
            "Task" => "Task",
            other => other,
        }
    }
}

impl zen_core::EventSink for TextSink {
    fn emit(&self, ev: AgentEvent) {
        match ev {
            AgentEvent::TextDelta { text } => {
                self.buf.write_str(&text);
            }
            AgentEvent::Assistant { message, .. } => {
                if let Some(content) = &message.content {
                    if !content.is_empty() {
                        self.buf.write_str(content);
                    }
                }



                if self.is_repl {
                    if let Some(calls) = &message.tool_calls {
                        for tc in calls {
                            self.buf.write_line(&format!(
                                "● {}(...) {}",
                                Self::tool_short(&tc.function.name),
                                tc.function.arguments
                            ));
                        }
                    }
                }
            }
            AgentEvent::ToolResult {
                tool_use_id, content, ..
            } => {
                if self.is_repl {
                    self.buf.write_line(&format!("  → [{}] {}", tool_use_id, content));
                }

            }
            AgentEvent::PermissionDenied(d) => {
                let _ = d;
            }
            AgentEvent::ApiRetry { .. } => {}
            AgentEvent::CompactBoundary { .. } => {}

            AgentEvent::SystemMessage { event, text } => {
                eprintln!("[{} hook] {}", event, text);
            }

            _ => {}
        }
    }
}



pub struct CapturingSink {
    pub buf: Arc<Buffered>,
}
impl CapturingSink {
    pub fn new() -> Self {
        Self { buf: Arc::new(Buffered::new()) }
    }
}
impl Default for CapturingSink {
    fn default() -> Self {
        Self::new()
    }
}
impl zen_core::EventSink for CapturingSink {
    fn emit(&self, ev: AgentEvent) {
        match ev {
            AgentEvent::TextDelta { text } => self.buf.write_str(&text),
            AgentEvent::Assistant { message, .. } => {
                if let Some(c) = &message.content {
                    self.buf.write_str(c);
                }
            }
            AgentEvent::ToolResult { .. } => {}
            AgentEvent::PermissionDenied(_) => {}
            AgentEvent::ApiRetry { .. } => {}
            AgentEvent::CompactBoundary { .. } => {}
            AgentEvent::SystemMessage { .. } => {}
            _ => {}
        }
    }
}




pub const STREAM_JSON_VERSION: u32 = 1;



pub struct StreamJsonSink {
    pub buf: Arc<Buffered>,
    pub session_id: String,
    pub cwd: String,
    pub model: String,
    pub tools: Vec<String>,
    pub mcp_servers: Vec<(String, String)>,
    pub permission_mode: String,
    pub slash_commands: Vec<String>,
    initialized: AtomicBool,
    include_partial: bool,


    prompt_input: Mutex<Option<String>>,

    mcp_errors: Mutex<Vec<(String, String)>>,

    folder_trusted: Mutex<Option<bool>>,
}

impl StreamJsonSink {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        buf: Arc<Buffered>,
        session_id: String,
        cwd: String,
        model: String,
        tools: Vec<String>,
        mcp_servers: Vec<(String, String)>,
        permission_mode: String,
        slash_commands: Vec<String>,
    ) -> Self {
        Self::with_partial(buf, session_id, cwd, model, tools, mcp_servers, permission_mode, slash_commands, false)
    }



    #[allow(clippy::too_many_arguments)]
    pub fn with_partial(
        buf: Arc<Buffered>,
        session_id: String,
        cwd: String,
        model: String,
        tools: Vec<String>,
        mcp_servers: Vec<(String, String)>,
        permission_mode: String,
        slash_commands: Vec<String>,
        include_partial: bool,
    ) -> Self {
        Self {
            buf,
            session_id,
            cwd,
            model,
            tools,
            mcp_servers,
            permission_mode,
            slash_commands,
            initialized: AtomicBool::new(false),
            include_partial,
            prompt_input: Mutex::new(None),
            mcp_errors: Mutex::new(Vec::new()),
            folder_trusted: Mutex::new(None),
        }
    }



    pub fn set_prompt_input(&self, prompt: String) {
        *self.prompt_input.lock().unwrap() = Some(prompt);
    }



    pub fn set_mcp_errors(&self, errors: Vec<(String, String)>) {
        *self.mcp_errors.lock().unwrap() = errors;
    }


    pub fn set_folder_trusted(&self, trusted: bool) {
        *self.folder_trusted.lock().unwrap() = Some(trusted);
    }


    pub fn init_now(&self) {
        if !self.initialized.swap(true, Ordering::SeqCst) {
            self.emit_init();
        }
    }

    fn emit_init(&self) {
        let mut line = serde_json::json!({
            "type": "system",
            "subtype": "init",
            "session_id": self.session_id,
            "cwd": self.cwd,
            "model": self.model,
            "tools": self.tools,
            "mcp_servers": self.mcp_servers.iter().map(|(n, s)| json!({"name":n,"status":s})).collect::<Vec<_>>(),
            "permissionMode": self.permission_mode,
            "slash_commands": self.slash_commands,
        });
        if let Some(t) = *self.folder_trusted.lock().unwrap() {
            line["folder_trusted"] = t.into();
        }
        let errors = self.mcp_errors.lock().unwrap();
        if !errors.is_empty() {
            line["mcp_server_errors"] = errors
                .iter()
                .map(|(n, r)| json!({"name": n, "reason": r}))
                .collect::<Vec<_>>()
                .into();
        }
        self.emit_line(line);
    }


    fn emit_line(&self, mut line: serde_json::Value) {
        if let Some(obj) = line.as_object_mut() {
            obj.insert("v".into(), json!(STREAM_JSON_VERSION));
        }
        self.buf.write_line(&serde_json::to_string(&line).unwrap());
    }
}

impl zen_core::EventSink for StreamJsonSink {
    fn emit(&self, ev: AgentEvent) {
        if !self.initialized.swap(true, Ordering::SeqCst) {
            self.emit_init();
        }
        match ev {
            AgentEvent::Assistant { message, parent_tool_use_id } => {
                let uuid = uuid::Uuid::new_v4();
                self.emit_line(serde_json::json!({
                    "type": "assistant",
                    "uuid": uuid.to_string(),
                    "message": message,
                    "session_id": self.session_id,
                    "parent_tool_use_id": serde_json::to_value(&parent_tool_use_id).unwrap_or(serde_json::Value::Null),
                }));
            }
            AgentEvent::ToolResult { tool_use_id, content, is_error, parent_tool_use_id, parallel } => {
                let uuid = uuid::Uuid::new_v4();
                self.emit_line(serde_json::json!({
                    "type": "user",
                    "uuid": uuid.to_string(),
                    "message": {
                        "role": "user",
                        "content": [{
                            "type": "tool_result",
                            "tool_use_id": tool_use_id,
                            "content": content,
                            "is_error": is_error,
                            "parallel": parallel,
                        }]
                    },
                    "session_id": self.session_id,
                    "parent_tool_use_id": serde_json::to_value(&parent_tool_use_id).unwrap_or(serde_json::Value::Null),
                }));
            }
            AgentEvent::TextDelta { text } => {
                if self.include_partial {
                    let uuid = uuid::Uuid::new_v4();
                    self.emit_line(serde_json::json!({
                        "type": "stream_event",
                        "uuid": uuid.to_string(),
                        "session_id": self.session_id,
                        "event": {
                            "type": "content_block_delta",
                            "delta": {"type": "text_delta", "text": text}
                        }
                    }));
                }
            }
            AgentEvent::ToolProgress { tool_use_id, tool_name, elapsed_ms } => {
                let uuid = uuid::Uuid::new_v4();
                self.emit_line(serde_json::json!({
                    "type": "system",
                    "subtype": "tool_progress",
                    "uuid": uuid.to_string(),
                    "session_id": self.session_id,
                    "tool_use_id": tool_use_id,
                    "tool_name": tool_name,
                    "elapsed_ms": elapsed_ms,
                }));
            }
            AgentEvent::ApiRetry { attempt, max_retries, retry_delay_ms, error_status, error } => {
                let uuid = uuid::Uuid::new_v4();
                self.emit_line(serde_json::json!({
                    "type": "system",
                    "subtype": "api_retry",
                    "uuid": uuid.to_string(),
                    "session_id": self.session_id,
                    "attempt": attempt,
                    "max_retries": max_retries,
                    "retry_delay_ms": retry_delay_ms,
                    "error_status": error_status,
                    "error": error,
                }));
            }
            AgentEvent::CompactBoundary { trigger, pre_tokens } => {
                let uuid = uuid::Uuid::new_v4();
                self.emit_line(serde_json::json!({
                    "type": "system",
                    "subtype": "compact_boundary",
                    "uuid": uuid.to_string(),
                    "session_id": self.session_id,
                    "trigger": trigger,
                    "pre_tokens": pre_tokens,
                }));
            }
            AgentEvent::Pruned { pruned, tokens_saved } => {
                let uuid = uuid::Uuid::new_v4();
                self.emit_line(serde_json::json!({
                    "type": "system",
                    "subtype": "prune",
                    "uuid": uuid.to_string(),
                    "session_id": self.session_id,
                    "pruned": pruned,
                    "tokens_saved": tokens_saved,
                }));
            }
            AgentEvent::PermissionDenied(d) => {
                let uuid = uuid::Uuid::new_v4();
                self.emit_line(serde_json::json!({
                    "type": "system",
                    "subtype": "permission_denied",
                    "uuid": uuid.to_string(),
                    "session_id": self.session_id,
                    "tool_name": d.tool_name,
                    "tool_use_id": d.tool_use_id,
                    "decision_reason": "permission required in non-interactive mode or denied by rule",
                }));
            }
            AgentEvent::SystemMessage { event, text } => {
                let uuid = uuid::Uuid::new_v4();
                self.emit_line(serde_json::json!({
                    "type": "system",
                    "subtype": "hook_message",
                    "uuid": uuid.to_string(),
                    "session_id": self.session_id,
                    "hook_event_name": event,
                    "message": text,
                }));
            }


            _ => {}
        }
    }
}


pub fn print_result(result: &RunResult, fmt: &crate::cli::OutputFormat, buf: &Buffered, is_repl: bool) {
    match fmt {
        crate::cli::OutputFormat::Text => {
            if is_repl {
                buf.write_line(&result.result);
            } else {
                buf.write_str(&result.result);
            }
        }
        crate::cli::OutputFormat::Json | crate::cli::OutputFormat::StreamJson => {
            buf.write_line(&result_json(result).to_string());
        }
    }
}





pub fn validate_json_schema(text: &str, schema_path: &std::path::Path) -> Result<(), String> {
    let schema_text =
        std::fs::read_to_string(schema_path).map_err(|e| format!("cannot read schema: {}", e))?;
    let schema: serde_json::Value =
        serde_json::from_str(&schema_text).map_err(|e| format!("invalid schema JSON: {}", e))?;
    let value: serde_json::Value =
        serde_json::from_str(text.trim()).map_err(|e| format!("output is not valid JSON: {}", e))?;


    if let Some(t) = schema.get("type").and_then(|v| v.as_str()) {
        let ok = match t {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            _ => true,
        };
        if !ok {
            return Err(format!("expected type '{}', got {}", t, json_type(&value)));
        }
    }

    if let (Some(obj), Some(req)) = (value.as_object(), schema.get("required").and_then(|v| v.as_array())) {
        for r in req {
            if let Some(name) = r.as_str() {
                if !obj.contains_key(name) {
                    return Err(format!("missing required field '{}'", name));
                }
            }
        }
    }
    Ok(())
}

fn json_type(v: &serde_json::Value) -> &'static str {
    if v.is_object() { "object" }
    else if v.is_array() { "array" }
    else if v.is_string() { "string" }
    else if v.is_number() { "number" }
    else if v.is_boolean() { "boolean" }
    else { "null" }
}





pub fn result_json(result: &RunResult) -> serde_json::Value {
    let mut v = serde_json::to_value(result).unwrap_or_else(|_| json!({}));
    let stop_reason = match result.subtype.as_str() {
        "success" => json!("end_turn"),
        "error_max_turns" => json!("max_turns"),
        _ => serde_json::Value::Null,
    };
    if let Some(obj) = v.as_object_mut() {
        obj.insert("uuid".into(), json!(uuid::Uuid::new_v4().to_string()));
        obj.insert("stop_reason".into(), stop_reason);
        obj.insert("v".into(), json!(STREAM_JSON_VERSION));
    }
    v
}


pub use serde_json::json;



#[cfg(test)]
mod tests {
    use super::*;
    use zen_core::{ChatMessage, EventSink, PermissionDenial, ToolCall, FunctionCall};

    #[test]
    fn stream_json_init_line_shape() {
        let buf = Arc::new(Buffered::new());
        let sink = StreamJsonSink::new(
            buf.clone(),
            "sess-id".into(),
            "/tmp/wd".into(),
            "mock-model".into(),
            vec!["Read".into(), "Bash".into()],
            vec![("srv1".to_string(), "connected".to_string())],
            "default".into(),
            vec!["/help".into(), "/compact".into()],
        );

        let msg = ChatMessage::assistant(Some("hi".into()), None);
        let _ = msg;
        sink.emit(AgentEvent::Assistant {
            message: ChatMessage::assistant(Some("hi".into()), None),
            parent_tool_use_id: None,
        });
        let out = buf.text();
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines.len() >= 2, "expected init + assistant lines, got: {}", out);


        let init: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(init["type"], "system");
        assert_eq!(init["subtype"], "init");
        assert_eq!(init["session_id"], "sess-id");
        assert_eq!(init["cwd"], "/tmp/wd");
        assert_eq!(init["model"], "mock-model");
        assert_eq!(init["permissionMode"], "default");
        assert_eq!(init["tools"], json!(["Read", "Bash"]));
        assert_eq!(init["mcp_servers"][0]["name"], "srv1");
        assert_eq!(init["mcp_servers"][0]["status"], "connected");
        assert_eq!(init["slash_commands"], json!(["/help", "/compact"]));


        let asst: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(asst["type"], "assistant");
        assert_eq!(asst["message"]["role"], "assistant");
        assert_eq!(asst["message"]["content"], "hi");
        assert_eq!(asst["parent_tool_use_id"], serde_json::Value::Null);
    }

    #[test]
    fn stream_json_user_tool_result_shape() {
        let buf = Arc::new(Buffered::new());
        let sink = StreamJsonSink::new(
            buf.clone(),
            "s".into(),
            "/w".into(),
            "m".into(),
            vec![],
            vec![],
            "default".into(),
            vec![],
        );
        sink.emit(AgentEvent::Assistant {
            message: ChatMessage::assistant(
                None,
                Some(vec![ToolCall {
                    id: "call_1".into(),
                    call_type: "function".into(),
                    function: FunctionCall { name: "Read".into(), arguments: "{\"file\":}".into() },
                }]),
            ),
            parent_tool_use_id: None,
        });
        sink.emit(AgentEvent::ToolResult {
            tool_use_id: "call_1".into(),
            content: "file contents here".into(),
            is_error: false,
            parent_tool_use_id: None,
            parallel: false,
        });
        let out = buf.text();
        let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();

        assert_eq!(lines.len(), 3);
        let u: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(u["type"], "user");
        assert_eq!(u["message"]["content"][0]["type"], "tool_result");
        assert_eq!(u["message"]["content"][0]["tool_use_id"], "call_1");
        assert_eq!(u["message"]["content"][0]["content"], "file contents here");
        assert_eq!(u["message"]["content"][0]["is_error"], false);
        assert_eq!(u["message"]["content"][0]["parallel"], false);
    }

    #[test]
    fn stream_json_retries_and_compact() {
        let buf = Arc::new(Buffered::new());
        let sink = StreamJsonSink::new(
            buf.clone(),
            "s".into(),
            "/w".into(),
            "m".into(),
            vec![],
            vec![],
            "default".into(),
            vec![],
        );
        sink.emit(AgentEvent::ApiRetry {
            attempt: 1,
            max_retries: 4,
            retry_delay_ms: 1000,
            error_status: Some(429),
            error: "rate limited".into(),
        });
        sink.emit(AgentEvent::CompactBoundary { trigger: "auto".into(), pre_tokens: 90_000 });
        let out = buf.text();
        let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
        let init: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(init["subtype"], "init");

        let retry: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(retry["type"], "system");
        assert_eq!(retry["subtype"], "api_retry");
        assert_eq!(retry["attempt"], 1);
        assert_eq!(retry["max_retries"], 4);
        assert_eq!(retry["error_status"], 429);
        assert_eq!(retry["error"], "rate limited");

        let compact: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(compact["subtype"], "compact_boundary");
        assert_eq!(compact["trigger"], "auto");
        assert_eq!(compact["pre_tokens"], 90_000);
    }

    #[test]
    fn result_shape_json() {
        let buf = Arc::new(Buffered::new());
        let result = RunResult {
            record_type: "result".into(),
            subtype: "success".into(),
            is_error: false,
            duration_ms: 10,
            duration_api_ms: 8,
            num_turns: 1,
            result: "ok".into(),
            session_id: "sid".into(),
            total_cost_usd: 0.0,
            usage: zen_core::UsageStats { input_tokens: 5, output_tokens: 2, cache_read_input_tokens: 0 },
            model: "m".into(),
            permission_denials: vec![],
        };
        print_result(&result, &crate::cli::OutputFormat::Json, &buf, false);
        let text_out = buf.text();
        let line = text_out.lines().next().unwrap();
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["type"], "result");
        assert_eq!(v["subtype"], "success");
        assert!(!v["is_error"].as_bool().unwrap());
        assert_eq!(v["result"], "ok");
        assert_eq!(v["model"], "m");
        assert_eq!(v["usage"]["input_tokens"], 5);
        assert_eq!(v["usage"]["output_tokens"], 2);
        assert_eq!(v["permission_denials"], json!([]));

        assert_eq!(v["stop_reason"], "end_turn");
        assert!(uuid::Uuid::parse_str(v["uuid"].as_str().unwrap()).is_ok(), "{}", v);
        for key in ["duration_ms", "duration_api_ms", "num_turns", "session_id", "total_cost_usd"] {
            assert!(v.get(key).is_some(), "missing {}", key);
        }
    }

    #[test]
    fn c8_2_stop_reason_follows_subtype() {
        let mut r = crate::agent_run::denied_result("s", "m", vec![]);
        assert!(result_json(&r)["stop_reason"].is_null());
        r.subtype = "error_max_turns".into();
        assert_eq!(result_json(&r)["stop_reason"], "max_turns");
    }

    #[test]
    fn c7_3_init_lists_mcp_server_errors_only_when_present() {
        let mk = || {
            let buf = Arc::new(Buffered::new());
            let sink = StreamJsonSink::new(
                buf.clone(), "s".into(), "/w".into(), "m".into(), vec![], vec![],
                "default".into(), vec![],
            );
            (buf, sink)
        };
        let (buf, sink) = mk();
        sink.init_now();
        let init: serde_json::Value = serde_json::from_str(buf.text().lines().next().unwrap()).unwrap();
        assert!(init.get("mcp_server_errors").is_none(), "{}", init);

        let (buf, sink) = mk();
        sink.set_mcp_errors(vec![("x".into(), "url_missing_type".into())]);
        sink.init_now();
        let init: serde_json::Value = serde_json::from_str(buf.text().lines().next().unwrap()).unwrap();
        assert_eq!(init["mcp_server_errors"], json!([{"name": "x", "reason": "url_missing_type"}]));
    }

    #[test]
    fn text_sink_collects_assistant_and_result() {
        let buf = Arc::new(Buffered::new());
        let sink = TextSink::new_headless(buf.clone());
        sink.emit(AgentEvent::TextDelta { text: "hello".into() });
        sink.emit(AgentEvent::Assistant {
            message: ChatMessage::assistant(None, None),
            parent_tool_use_id: None,
        });
        assert_eq!(buf.text(), "hello");
    }

    #[test]
    fn permission_denial_serializes() {
        let d = PermissionDenial {
            tool_name: "Bash".into(),
            tool_use_id: "call_x".into(),
            tool_input: json!({"command":"rm -rf /"}),
        };
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(v["tool_name"], "Bash");
        assert_eq!(v["tool_use_id"], "call_x");
    }


    #[test]
    fn stream_json_every_line_has_version() {
        let buf = Arc::new(Buffered::new());
        let sink = StreamJsonSink::with_partial(
            buf.clone(),
            "s".into(),
            "/w".into(),
            "m".into(),
            vec![],
            vec![],
            "default".into(),
            vec![],
            true,
        );
        sink.emit(AgentEvent::Assistant {
            message: ChatMessage::assistant(Some("hi".into()), None),
            parent_tool_use_id: None,
        });
        sink.emit(AgentEvent::TextDelta { text: "hel".into() });
        sink.emit(AgentEvent::ToolResult {
            tool_use_id: "call_1".into(),
            content: "out".into(),
            is_error: false,
            parent_tool_use_id: None,
            parallel: false,
        });
        sink.emit(AgentEvent::ToolProgress {
            tool_use_id: "call_1".into(),
            tool_name: "Bash".into(),
            elapsed_ms: 5000,
        });
        sink.emit(AgentEvent::ApiRetry {
            attempt: 1,
            max_retries: 3,
            retry_delay_ms: 100,
            error_status: None,
            error: "boom".into(),
        });
        let out = buf.text();
        let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
        assert!(lines.len() >= 5, "expected init + 5 events, got: {}", out);
        for line in &lines {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["v"], json!(STREAM_JSON_VERSION), "missing v in: {}", line);
        }

        let result = RunResult {
            record_type: "result".into(),
            subtype: "success".into(),
            is_error: false,
            duration_ms: 1,
            duration_api_ms: 1,
            num_turns: 1,
            result: "done".into(),
            session_id: "s".into(),
            total_cost_usd: 0.0,
            usage: zen_core::UsageStats::default(),
            model: "m".into(),
            permission_denials: vec![],
        };
        let r = result_json(&result);
        assert_eq!(r["v"], json!(STREAM_JSON_VERSION));
    }


    #[test]
    fn stream_json_tool_progress_shape() {
        let buf = Arc::new(Buffered::new());
        let sink = StreamJsonSink::new(
            buf.clone(), "s".into(), "/w".into(), "m".into(), vec![], vec![],
            "default".into(), vec![],
        );
        sink.emit(AgentEvent::ToolProgress {
            tool_use_id: "call_9".into(),
            tool_name: "Bash".into(),
            elapsed_ms: 12_345,
        });
        let out = buf.text();
        let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 2);
        let p: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(p["type"], "system");
        assert_eq!(p["subtype"], "tool_progress");
        assert_eq!(p["tool_use_id"], "call_9");
        assert_eq!(p["tool_name"], "Bash");
        assert_eq!(p["elapsed_ms"], 12_345);
        assert_eq!(p["session_id"], "s");
        assert_eq!(p["v"], json!(STREAM_JSON_VERSION));
    }
}
