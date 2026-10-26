






#![cfg(test)]

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::{Agent, AgentConfig};
use tokio::io::AsyncReadExt;
use zen_core::*;





pub struct MockLlm {
    pub queue: Mutex<VecDeque<anyhow::Result<ChatResponse>>>,
    pub requests: Mutex<Vec<ChatRequest>>,
}

impl MockLlm {

    pub fn new(responses: Vec<ChatResponse>) -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(responses.into_iter().map(Ok).collect()),
            requests: Mutex::new(vec![]),
        })
    }


    pub fn with_errors(items: Vec<anyhow::Result<ChatResponse>>) -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(items.into()),
            requests: Mutex::new(vec![]),
        })
    }


    pub fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl LlmBackend for MockLlm {
    async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse> {
        self.requests.lock().unwrap().push(req);
        let mut q = self.queue.lock().unwrap();
        q.pop_front()
            .unwrap_or_else(|| Err(anyhow::anyhow!("mock: no scripted responses left")))
    }
}


pub fn resp(text: &str) -> ChatResponse {
    ChatResponse {
        message: ChatMessage::assistant(Some(text.into()), None),
        usage: Usage { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15, cache_read_input_tokens: 0, cache_creation_input_tokens: 0 },
        finish_reason: Some("stop".into()),
    }
}



pub fn resp_tool(name: &str, args: &str, id: &str) -> ChatResponse {
    ChatResponse {
        message: ChatMessage::assistant(
            None,
            Some(vec![ToolCall {
                id: id.into(),
                call_type: "function".into(),
                function: FunctionCall { name: name.into(), arguments: args.into() },
            }]),
        ),
        usage: Usage { prompt_tokens: 20, completion_tokens: 8, total_tokens: 28, cache_read_input_tokens: 0, cache_creation_input_tokens: 0 },
        finish_reason: Some("tool_calls".into()),
    }
}



pub fn resp_multi_tool(calls: Vec<(&str, &str, &str)>) -> ChatResponse {
    let tool_calls = calls
        .into_iter()
        .map(|(name, args, id)| ToolCall {
            id: id.into(),
            call_type: "function".into(),
            function: FunctionCall { name: name.into(), arguments: args.into() },
        })
        .collect();
    ChatResponse {
        message: ChatMessage::assistant(None, Some(tool_calls)),
        usage: Usage { prompt_tokens: 20, completion_tokens: 8, total_tokens: 28, ..Default::default() },
        finish_reason: Some("tool_calls".into()),
    }
}




pub struct ConcurrencyTool {
    pub name: &'static str,
    pub delay: std::time::Duration,
    pub current: Arc<std::sync::atomic::AtomicUsize>,
    pub max: Arc<std::sync::atomic::AtomicUsize>,
}

impl ConcurrencyTool {
    pub fn read(delay_ms: u64) -> (Arc<Self>, Arc<std::sync::atomic::AtomicUsize>) {
        let max = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let tool = Arc::new(Self {
            name: "Read",
            delay: std::time::Duration::from_millis(delay_ms),
            current: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            max: max.clone(),
        });
        (tool, max)
    }
}

#[async_trait::async_trait]
impl Tool for ConcurrencyTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "concurrency-tracking fake tool for tests"
    }
    fn parameters_schema(&self) -> Value {
        json!({ "type": "object" })
    }
    fn is_read_only(&self) -> bool {
        true
    }
    async fn call(&self, _args: Value, _ctx: &ToolContext) -> ToolOutput {
        use std::sync::atomic::Ordering;
        let c = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        self.max.fetch_max(c, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        self.current.fetch_sub(1, Ordering::SeqCst);
        ToolOutput::ok("read-ok")
    }
}


pub struct FakeTool {
    pub name: &'static str,
    pub read_only: bool,
    pub output: ToolOutput,
}

#[async_trait::async_trait]
impl Tool for FakeTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "fake tool for tests"
    }
    fn parameters_schema(&self) -> Value {
        json!({ "type": "object" })
    }
    fn is_read_only(&self) -> bool {
        self.read_only
    }
    async fn call(&self, _args: Value, _ctx: &ToolContext) -> ToolOutput {
        self.output.clone()
    }
}


pub struct FakePerms {
    pub decision: PermissionDecision,
    pub always: Mutex<Vec<String>>,
}

impl FakePerms {
    pub fn new(d: PermissionDecision) -> Arc<Self> {
        Arc::new(Self { decision: d, always: Mutex::new(vec![]) })
    }
}

impl PermissionChecker for FakePerms {
    fn check(&self, _name: &str, _input: &Value, _ro: bool, _ctx: &ToolContext) -> PermissionDecision {
        self.decision.clone()
    }
    fn add_allow_rule(&self, rule: &str) {
        self.always.lock().unwrap().push(rule.to_string());
    }
}


#[derive(Default)]
pub struct FakeHooks {
    pub queues: Mutex<HashMap<HookEvent, VecDeque<HookOutcome>>>,

    pub calls: Mutex<Vec<(HookEvent, String, Value)>>,
}

impl FakeHooks {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    pub fn push(&self, ev: HookEvent, out: HookOutcome) {
        self.queues.lock().unwrap().entry(ev).or_default().push_back(out);
    }
}

#[async_trait::async_trait]
impl HookRunner for FakeHooks {
    async fn run(&self, ev: HookEvent, target: &str, payload: Value) -> HookOutcome {
        self.calls.lock().unwrap().push((ev, target.to_string(), payload));
        let mut q = self.queues.lock().unwrap();
        q.get_mut(&ev).and_then(|v| v.pop_front()).unwrap_or_default()
    }
}


#[derive(Default)]
pub struct CollectSink {
    pub events: Mutex<Vec<AgentEvent>>,
}

impl EventSink for CollectSink {
    fn emit(&self, ev: AgentEvent) {
        self.events.lock().unwrap().push(ev);
    }
}


pub fn temp_ctx() -> ToolContext {
    let dir = std::env::temp_dir().join(format!("zen-agent-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    ToolContext::new(dir, "session-test")
}


pub fn registry_with(tools: Vec<Arc<dyn Tool>>) -> ToolRegistry {
    let mut r = ToolRegistry::new();
    for t in tools {
        r.register(t);
    }
    r
}


pub fn base_cfg() -> AgentConfig {
    AgentConfig {
        model: "mock-model".into(),
        system_prompt: "you are a test".into(),
        auto_compact_threshold: None,
        ..Default::default()
    }
}


pub fn build_agent(
    llm: Arc<MockLlm>,
    tools: ToolRegistry,
    perms: Arc<dyn PermissionChecker>,
    cfg: AgentConfig,
) -> (Agent, Arc<CollectSink>, Arc<FakeHooks>) {
    let sink = Arc::new(CollectSink::default());
    let hooks = FakeHooks::new();
    let agent = Agent::new(
        llm,
        tools,
        perms,
        None,
        hooks.clone(),
        sink.clone(),
        temp_ctx(),
        cfg,
    );
    (agent, sink, hooks)
}


#[allow(dead_code)]
pub async fn read_full_request(sock: &mut tokio::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = sock.read(&mut tmp).await.unwrap();
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        let s = String::from_utf8_lossy(&buf);
        if let Some(idx) = s.find("\r\n\r\n") {
            let len = s[..idx]
                .lines()
                .find_map(|l| {
                    let low = l.to_ascii_lowercase();
                    low.strip_prefix("content-length:")
                        .and_then(|v| v.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            if buf.len() >= idx + 4 + len {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).to_string()
}


#[allow(dead_code)]
pub fn http_response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        body.len(),
        body
    )
}
