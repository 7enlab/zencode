

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use zen_core::*;

use crate::testutil::*;


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
        let tag = args
            .get("tag")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
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
        usage: Usage {
            prompt_tokens: 20,
            completion_tokens: 8,
            total_tokens: 28,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        },
        finish_reason: Some("tool_calls".into()),
    }
}

fn tool_results(agent: &crate::Agent) -> Vec<(String, String)> {
    agent
        .messages()
        .iter()
        .filter(|m| m.role == "tool")
        .map(|m| {
            (
                m.tool_call_id.clone().unwrap_or_default(),
                m.content.clone().unwrap_or_default(),
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p0_1_read_only_calls_run_concurrently_and_keep_order() {
    let s = Shared::new();
    let tools = registry_with(vec![
        s.tool("Read", true, 300),
        s.tool("Grep", true, 300),
        s.tool("Glob", true, 300),
    ]);
    let llm = MockLlm::new(vec![
        multi_call(&[("Read", "a"), ("Grep", "b"), ("Glob", "c")]),
        resp("done"),
    ]);
    let (mut agent, _sink, _h) = build_agent(
        llm,
        tools,
        FakePerms::new(PermissionDecision::Allow),
        base_cfg(),
    );
    let t0 = Instant::now();
    let r = agent.run_turn("go").await;
    let wall = t0.elapsed();
    assert!(!r.is_error, "{:?}", r.result);
    assert!(
        wall < Duration::from_millis(800),
        "3x300ms read-only calls took {:?}",
        wall
    );
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

    let tools = registry_with(vec![
        s.tool("Read", true, 100),
        s.tool("Bash", false, 100),
        s.tool("TodoWrite", true, 100),
    ]);
    let llm = MockLlm::new(vec![
        multi_call(&[
            ("Read", "r1"),
            ("Read", "r2"),
            ("Bash", "b1"),
            ("Bash", "b2"),
            ("TodoWrite", "t1"),
            ("Read", "r3"),
        ]),
        resp("done"),
    ]);
    let (mut agent, _sink, _h) = build_agent(
        llm,
        tools,
        FakePerms::new(PermissionDecision::Allow),
        base_cfg(),
    );
    let r = agent.run_turn("go").await;
    assert!(!r.is_error, "{:?}", r.result);
    let log = s.log.lock().unwrap().clone();

    let pos = |e: &str| log.iter().position(|x| x == e).unwrap();
    assert!(
        pos("start r2") < pos("end r1"),
        "reads should overlap: {:?}",
        log
    );
    assert!(
        pos("start b1") > pos("end r1") && pos("start b1") > pos("end r2"),
        "{:?}",
        log
    );
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
    let llm = MockLlm::new(vec![
        multi_call(&[("Read", "a"), ("Nope", "x"), ("Read", "b")]),
        resp("done"),
    ]);
    let (mut agent, _sink, _h) = build_agent(
        llm,
        tools,
        FakePerms::new(PermissionDecision::Allow),
        base_cfg(),
    );
    let r = agent.run_turn("go").await;
    assert!(!r.is_error);
    let res = tool_results(&agent);
    assert_eq!(res.len(), 3);
    assert_eq!(res[0].1, "Read a");
    assert!(res[1].1.starts_with("Unknown tool"), "{:?}", res[1]);
    assert_eq!(res[2].1, "Read b");
}





async fn status_server(replies: Vec<(u16, &'static str)>) -> (String, Arc<AtomicUsize>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = Arc::new(AtomicUsize::new(0));
    let counter = served.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let i = counter.fetch_add(1, Ordering::SeqCst);
            let (code, body) = replies[i.min(replies.len() - 1)];
            let mut buf = vec![0u8; 65536];
            let mut got = Vec::new();
            loop {
                let n = sock.read(&mut buf).await.unwrap_or(0);
                got.extend_from_slice(&buf[..n]);
                let txt = String::from_utf8_lossy(&got);
                if let Some(h) = txt.find("\r\n\r\n") {
                    let len = txt[..h]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if got.len() >= h + 4 + len {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let resp = format!(
                "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                code,
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
        }
    });
    (format!("http://{addr}/v1"), served)
}

const OK_BODY: &str = r#"{"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;

fn req() -> ChatRequest {
    ChatRequest {
        model: "m".into(),
        messages: vec![ChatMessage::user("hi")],
        tools: vec![],
        max_tokens: None,
    }
}

fn retries(sink: &CollectSink) -> usize {
    sink.events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| matches!(e, AgentEvent::ApiRetry { .. }))
        .count()
}

#[tokio::test]
async fn p0_4_gateway_errors_retry_past_four_attempts_within_the_budget() {
    let (url, served) = status_server(vec![
        (503, "busy"),
        (502, "bad"),
        (504, "slow"),
        (503, "busy"),
        (429, "rl"),
        (200, OK_BODY),
    ])
    .await;
    let sink = Arc::new(CollectSink::default());
    let mut c = crate::client::OpenAiClient::new(url, None, sink.clone());
    c.base_retry_delay_ms = 1;
    let r = c
        .chat(req())
        .await
        .expect("should succeed on the 6th attempt");
    assert_eq!(r.message.content.as_deref(), Some("ok"));
    assert_eq!(served.load(Ordering::SeqCst), 6);
    assert_eq!(retries(&sink), 5);
}

#[tokio::test]
async fn p0_4_total_wait_is_capped() {
    let (url, served) = status_server(vec![(429, r#"{"error":{"message":"rate limited"}}"#)]).await;
    let sink = Arc::new(CollectSink::default());
    let mut c = crate::client::OpenAiClient::new(url, None, sink.clone());
    c.base_retry_delay_ms = 20;
    c.max_total_wait_ms = 100;
    let t0 = Instant::now();
    let err = c.chat(req()).await.unwrap_err().to_string();
    let took = t0.elapsed();
    assert!(
        err.contains("HTTP 429") && err.contains("retry.maxWaitSecs"),
        "{}",
        err
    );
    assert!(took < Duration::from_millis(1500), "took {:?}", took);


    assert_eq!(served.load(Ordering::SeqCst), 4);
    let total: u64 = sink
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ApiRetry { retry_delay_ms, .. } => Some(*retry_delay_ms),
            _ => None,
        })
        .sum();
    assert!(
        (75..=100).contains(&total),
        "total wait {total} outside [75, 100]"
    );
}

#[tokio::test]
async fn p0_4_other_5xx_stop_after_five_retries() {
    let (url, served) = status_server(vec![(500, "boom")]).await;
    let sink = Arc::new(CollectSink::default());
    let mut c = crate::client::OpenAiClient::new(url, None, sink.clone());
    c.base_retry_delay_ms = 1;
    let err = c.chat(req()).await.unwrap_err().to_string();
    assert!(err.contains("HTTP 500"), "{}", err);

    assert_eq!(served.load(Ordering::SeqCst), 6);
}



#[cfg(unix)]
fn verify_cfg(cmd: &str, timeout_secs: u64) -> crate::AgentConfig {
    crate::AgentConfig {
        verify_after_edit: Some(cmd.to_string()),
        verify_after_edit_timeout_secs: timeout_secs,
        ..base_cfg()
    }
}

#[cfg(unix)]
async fn run_one(
    tool: &'static str,
    is_error: bool,
    cfg: crate::AgentConfig,
) -> (String, std::path::PathBuf) {
    let out = if is_error {
        ToolOutput::err("edit failed")
    } else {
        ToolOutput::ok("edited")
    };
    let tools = registry_with(vec![Arc::new(FakeTool {
        name: tool,
        read_only: false,
        output: out,
    })]);
    let llm = MockLlm::new(vec![resp_tool(tool, "{}", "e1"), resp("done")]);
    let (mut agent, _s, _h) =
        build_agent(llm, tools, FakePerms::new(PermissionDecision::Allow), cfg);
    let cwd = agent.ctx.cwd();
    agent.run_turn("go").await;
    (tool_results(&agent)[0].1.clone(), cwd)
}

#[cfg(unix)]
#[tokio::test]
async fn p0_6_failing_verify_after_edit_is_appended_with_a_40_line_tail() {
    let (content, _) = run_one("Edit", false, verify_cfg("seq 1 100; exit 3", 60)).await;
    assert!(content.starts_with("edited"), "{}", content);
    assert!(
        content.contains("verify `seq 1 100; exit 3` failed (exit 3)"),
        "{}",
        content
    );
    let tail: Vec<&str> = content
        .lines()
        .map(|l| l.trim_end_matches(']'))
        .filter(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_digit()))
        .collect();
    assert_eq!(tail.len(), 40, "{}", content);
    assert_eq!(tail.first(), Some(&"61"));
    assert_eq!(tail.last(), Some(&"100"));
}

#[cfg(unix)]
#[tokio::test]
async fn p0_6_verify_runs_only_after_successful_file_changes() {

    let (content, _) = run_one("Write", false, verify_cfg("true", 60)).await;
    assert_eq!(content, "edited");

    for (tool, is_error) in [("Edit", true), ("Bash", false)] {
        let (content, cwd) = run_one(tool, is_error, verify_cfg("touch verify-ran", 60)).await;
        assert!(
            !cwd.join("verify-ran").exists(),
            "{} ran verify: {}",
            tool,
            content
        );
    }

    let (content, cwd) = run_one("Edit", false, base_cfg()).await;
    assert_eq!(content, "edited");
    assert!(!cwd.join("verify-ran").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn p0_6_verify_after_edit_times_out() {
    let t0 = Instant::now();
    let (content, _) = run_one("Edit", false, verify_cfg("sleep 5", 1)).await;
    assert!(content.contains("timed out after 1s"), "{}", content);
    assert!(t0.elapsed() < Duration::from_secs(4));
}
