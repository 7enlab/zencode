

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use zen_core::*;

use crate::agent::{TRUNCATED_CALL_NUDGE, TRUNCATION_CONTINUE_NUDGE};
use crate::testutil::*;


struct Rec {
    calls: Arc<Mutex<Vec<Value>>>,
}

#[async_trait]
impl Tool for Rec {
    fn name(&self) -> &str {
        "Write"
    }
    fn description(&self) -> &str {
        "write"
    }
    fn parameters_schema(&self) -> Value {
        json!({"type": "object", "properties": {"file_path": {"type": "string"}, "content": {"type": "string"}}})
    }
    fn is_read_only(&self) -> bool {
        false
    }
    async fn call(&self, args: Value, _ctx: &ToolContext) -> ToolOutput {
        self.calls.lock().unwrap().push(args);
        ToolOutput::ok("written")
    }
}

fn cut(mut r: ChatResponse, reason: &str) -> ChatResponse {
    r.finish_reason = Some(reason.into());
    r
}

type Calls = Arc<Mutex<Vec<Value>>>;

fn setup(replies: Vec<ChatResponse>, cfg: crate::AgentConfig) -> (crate::Agent, Arc<MockLlm>, Arc<CollectSink>, Calls) {
    let calls = Arc::new(Mutex::new(vec![]));
    let tools = registry_with(vec![Arc::new(Rec { calls: calls.clone() })]);
    let llm = MockLlm::new(replies);
    let (agent, sink, _h) = build_agent(llm.clone(), tools, FakePerms::new(PermissionDecision::Allow), cfg);
    (agent, llm, sink, calls)
}

fn last_user(req: &ChatRequest) -> String {
    req.messages.iter().rev().find(|m| m.role == "user").and_then(|m| m.content.clone()).unwrap_or_default()
}

#[test]
fn trunc_reasons_recognized() {
    for r in ["length", "max_tokens", "MAX_TOKENS", "max_output_tokens"] {
        assert!(crate::agent::is_truncated(Some(r)), "{r}");
    }
    for r in ["stop", "tool_calls", "content_filter"] {
        assert!(!crate::agent::is_truncated(Some(r)), "{r}");
    }
    assert!(!crate::agent::is_truncated(None));
}

#[tokio::test]
async fn trunc_text_reply_is_continued_and_stitched() {
    let (mut agent, llm, sink, _) = setup(
        vec![cut(resp("The answer is forty"), "length"), cut(resp("-two, because "), "max_tokens"), resp("six times seven.")],
        base_cfg(),
    );
    let res = agent.run_turn("question").await;
    assert!(!res.is_error, "{}", res.result);
    assert_eq!(res.subtype, "success");
    assert_eq!(res.result, "The answer is forty-two, because six times seven.");
    assert_eq!(res.num_turns, 3);
    let reqs = llm.requests.lock().unwrap();
    assert_eq!(reqs.len(), 3);

    assert_eq!(last_user(&reqs[1]), TRUNCATION_CONTINUE_NUDGE);
    let prev = &reqs[1].messages[reqs[1].messages.len() - 2];
    assert_eq!(prev.role, "assistant");
    assert_eq!(prev.content.as_deref(), Some("The answer is forty"));
    assert_eq!(last_user(&reqs[2]), TRUNCATION_CONTINUE_NUDGE);

    let n = sink.events.lock().unwrap().iter().filter(|e| matches!(e, AgentEvent::Assistant { .. })).count();
    assert_eq!(n, 3);
}

#[tokio::test]
async fn trunc_half_tool_call_is_not_executed_and_is_reemitted() {
    let half = r#"{"file_path":"big.txt","content":"line 1\nline 2\nli"#;
    let (mut agent, llm, sink, calls) = setup(
        vec![
            cut(resp_tool("Write", half, "w1"), "length"),
            resp_tool("Write", r#"{"file_path":"big.txt","content":"line 1\nline 2\nline 3\n"}"#, "w2"),
            resp("done"),
        ],
        base_cfg(),
    );
    let res = agent.run_turn("write it").await;
    assert!(!res.is_error, "{}", res.result);
    assert_eq!(res.result, "done");

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["content"], "line 1\nline 2\nline 3\n");
    let reqs = llm.requests.lock().unwrap();
    assert_eq!(last_user(&reqs[1]), TRUNCATED_CALL_NUDGE);

    let has_w1 = |m: &ChatMessage| {
        m.tool_calls.iter().flatten().any(|c| c.id == "w1") || m.tool_call_id.as_deref() == Some("w1")
    };
    assert!(!reqs.iter().flat_map(|r| r.messages.iter()).any(has_w1));
    assert!(!agent.messages().iter().any(has_w1));
    let evs = sink.events.lock().unwrap();
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::Assistant { message, .. } if has_w1(message))));
}

#[tokio::test]
async fn trunc_complete_tool_call_still_runs() {
    let (mut agent, llm, _s, calls) = setup(
        vec![cut(resp_tool("Write", r#"{"file_path":"a","content":"x"}"#, "w1"), "length"), resp("ok")],
        base_cfg(),
    );
    let res = agent.run_turn("go").await;
    assert_eq!(res.result, "ok");
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(llm.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn trunc_cap_reached_on_text_ends_with_note() {
    let cfg = crate::AgentConfig { max_continuations: 2, ..base_cfg() };
    let (mut agent, llm, sink, _) =
        setup(vec![cut(resp("a"), "length"), cut(resp("b"), "length"), cut(resp("c"), "length"), resp("never")], cfg);
    let res = agent.run_turn("go").await;
    assert!(!res.is_error);
    assert_eq!(res.result, "abc");
    assert_eq!(llm.requests.lock().unwrap().len(), 3);
    let evs = sink.events.lock().unwrap();
    let note = evs.iter().find_map(|e| match e {
        AgentEvent::SystemMessage { event, text } if event == "Truncation" => Some(text.clone()),
        _ => None,
    });
    let note = note.expect("truncation note");
    assert!(note.contains("cut off by the output token limit 3 time(s)"), "{note}");
}

#[tokio::test]
async fn trunc_cap_reached_on_tool_call_is_a_clear_error_and_nothing_runs() {
    let cfg = crate::AgentConfig { max_continuations: 1, ..base_cfg() };
    let (mut agent, llm, _s, calls) = setup(
        vec![
            cut(resp_tool("Write", r#"{"file_path":"a","con"#, "w1"), "length"),
            cut(resp_tool("Write", r#"{"file_path":"a","content":"xy"#, "w2"), "length"),
            resp("never"),
        ],
        cfg,
    );
    let res = agent.run_turn("go").await;
    assert!(res.is_error);
    assert_eq!(res.subtype, "error_during_execution");
    assert!(res.result.contains("tool call was cut off by the output token limit 2 time(s)"), "{}", res.result);
    assert!(res.result.contains("no tool was run"), "{}", res.result);
    assert!(calls.lock().unwrap().is_empty());
    assert_eq!(llm.requests.lock().unwrap().len(), 2);

    assert!(agent.messages().iter().all(|m| m.tool_calls.is_none()));
}

#[tokio::test]
async fn trunc_zero_cap_disables_continuation() {
    let cfg = crate::AgentConfig { max_continuations: 0, ..base_cfg() };
    let (mut agent, llm, _s, _) = setup(vec![cut(resp("partial"), "length"), resp("never")], cfg);
    let res = agent.run_turn("go").await;
    assert_eq!(res.result, "partial");
    assert_eq!(llm.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn trunc_counter_resets_after_a_normal_reply() {

    let cfg = crate::AgentConfig { max_continuations: 1, ..base_cfg() };
    let (mut agent, _llm, _s, calls) = setup(
        vec![
            cut(resp_tool("Write", r#"{"file_path""#, "w1"), "length"),
            resp_tool("Write", r#"{"file_path":"a","content":"x"}"#, "w2"),
            cut(resp("fin"), "length"),
            resp("ished"),
        ],
        cfg,
    );
    let res = agent.run_turn("go").await;
    assert!(!res.is_error, "{}", res.result);
    assert_eq!(res.result, "finished");
    assert_eq!(calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn trunc_plan_mode_plan_text_is_glued() {
    let cfg = crate::AgentConfig { permission_mode: PermissionMode::Plan, ..base_cfg() };
    let (mut agent, _llm, _s, _) = setup(vec![cut(resp("Step 1: re"), "length"), resp("ad the file.")], cfg);
    let res = agent.run_turn("plan").await;
    assert_eq!(res.result, "Step 1: read the file.");
}
