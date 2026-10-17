//! Tests for the perf/p0 changes to the agent loop (kept apart from `tests.rs`).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use zen_core::*;

use crate::testutil::*;

/// A tool that sleeps, records start/end order and the max number of concurrent calls.
struct SlowTool {
    name: &'static str,
    read_only: bool,
    delay_ms: u64,
    running: Arc<AtomicUsize>,
    max_running: Arc<AtomicUsize>,
    log: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl Tool for SlowTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "slow test tool"
    }
    fn parameters_schema(&self) -> Value {
        json!({ "type": "object" })
    }
    fn is_read_only(&self) -> bool {
        self.read_only
    }
    async fn call(&self, args: Value, _ctx: &ToolContext) -> ToolOutput {
        let tag = args.get("tag").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_running.fetch_max(now, Ordering::SeqCst);
        self.log.lock().unwrap().push(format!("start {}", tag));
        tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
        self.log.lock().unwrap().push(format!("end {}", tag));
        self.running.fetch_sub(1, Ordering::SeqCst);
        ToolOutput::ok(format!("{} {}", self.name, tag))
    }
}

struct Shared {
    running: Arc<AtomicUsize>,
    max_running: Arc<AtomicUsize>,
    log: Arc<Mutex<Vec<String>>>,
}

impl Shared {
    fn new() -> Self {
        Self {
            running: Arc::new(AtomicUsize::new(0)),
            max_running: Arc::new(AtomicUsize::new(0)),
            log: Arc::new(Mutex::new(vec![])),
        }
    }
    fn tool(&self, name: &'static str, read_only: bool, delay_ms: u64) -> Arc<dyn Tool> {
        Arc::new(SlowTool {
            name,
            read_only,
            delay_ms,
            running: self.running.clone(),
            max_running: self.max_running.clone(),
            log: self.log.clone(),
        })
    }
}

fn multi_call(calls: &[(&str, &str)]) -> ChatResponse {
    ChatResponse {
        message: ChatMessage::assistant(
            None,
            Some(
                calls
                    .iter()
                    .enumerate()
                    .map(|(i, (name, tag))| ToolCall {
                        id: format!("c{}", i),
                        call_type: "function".into(),
                        function: FunctionCall {
                            name: name.to_string(),
                            arguments: json!({ "tag": tag }).to_string(),
                        },
                    })
                    .collect(),
            ),
        ),
        usage: Usage { prompt_tokens: 20, completion_tokens: 8, total_tokens: 28 },
        finish_reason: Some("tool_calls".into()),
    }
}

fn tool_results(agent: &crate::Agent) -> Vec<(String, String)> {
    agent
        .messages()
        .iter()
        .filter(|m| m.role == "tool")
        .map(|m| (m.tool_call_id.clone().unwrap_or_default(), m.content.clone().unwrap_or_default()))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p0_1_read_only_calls_run_concurrently_and_keep_order() {
    let s = Shared::new();
    let tools = registry_with(vec![s.tool("Read", true, 300), s.tool("Grep", true, 300), s.tool("Glob", true, 300)]);
    let llm = MockLlm::new(vec![multi_call(&[("Read", "a"), ("Grep", "b"), ("Glob", "c")]), resp("done")]);
    let (mut agent, _sink, _h) = build_agent(llm, tools, FakePerms::new(PermissionDecision::Allow), base_cfg());
    let t0 = Instant::now();
    let r = agent.run_turn("go").await;
    let wall = t0.elapsed();
    assert!(!r.is_error, "{:?}", r.result);
    assert!(wall < Duration::from_millis(800), "3x300ms read-only calls took {:?}", wall);
    assert_eq!(s.max_running.load(Ordering::SeqCst), 3);
    assert_eq!(
        tool_results(&agent),
        vec![
            ("c0".to_string(), "Read a".to_string()),
            ("c1".to_string(), "Grep b".to_string()),
            ("c2".to_string(), "Glob c".to_string()),
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p0_1_mutating_and_unlisted_tools_stay_sequential() {
    let s = Shared::new();
    // Read is batched; Bash (mutating) and TodoWrite (read-only but not parallel-safe) run alone.
    let tools = registry_with(vec![
        s.tool("Read", true, 100),
        s.tool("Bash", false, 100),
        s.tool("TodoWrite", true, 100),
    ]);
    let llm = MockLlm::new(vec![
        multi_call(&[("Read", "r1"), ("Read", "r2"), ("Bash", "b1"), ("Bash", "b2"), ("TodoWrite", "t1"), ("Read", "r3")]),
        resp("done"),
    ]);
    let (mut agent, _sink, _h) = build_agent(llm, tools, FakePerms::new(PermissionDecision::Allow), base_cfg());
    let r = agent.run_turn("go").await;
    assert!(!r.is_error, "{:?}", r.result);
    let log = s.log.lock().unwrap().clone();
    // r1/r2 overlap; everything after starts only once the previous call ended.
    let pos = |e: &str| log.iter().position(|x| x == e).unwrap();
    assert!(pos("start r2") < pos("end r1"), "reads should overlap: {:?}", log);
    assert!(pos("start b1") > pos("end r1") && pos("start b1") > pos("end r2"), "{:?}", log);
    assert!(pos("start b2") > pos("end b1"), "{:?}", log);
    assert!(pos("start t1") > pos("end b2"), "{:?}", log);
    assert!(pos("start r3") > pos("end t1"), "{:?}", log);
    let ids: Vec<String> = tool_results(&agent).into_iter().map(|(id, _)| id).collect();
    assert_eq!(ids, vec!["c0", "c1", "c2", "c3", "c4", "c5"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p0_1_denied_call_inside_a_batch_keeps_its_slot() {
    let s = Shared::new();
    let tools = registry_with(vec![s.tool("Read", true, 50)]);
    let llm = MockLlm::new(vec![multi_call(&[("Read", "a"), ("Nope", "x"), ("Read", "b")]), resp("done")]);
    let (mut agent, _sink, _h) = build_agent(llm, tools, FakePerms::new(PermissionDecision::Allow), base_cfg());
    let r = agent.run_turn("go").await;
    assert!(!r.is_error);
    let res = tool_results(&agent);
    assert_eq!(res.len(), 3);
    assert_eq!(res[0].1, "Read a");
    assert!(res[1].1.starts_with("Unknown tool"), "{:?}", res[1]);
    assert_eq!(res[2].1, "Read b");
}
