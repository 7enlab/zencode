//! Behavior tests for the agent loop, driven entirely by scripted doubles from
//! [`crate::testutil`]: no network, no real tools, no real hooks.
//!
//! Each test states one observable contract of [`crate::Agent::run_turn`] / compaction / the
//! sub-agent executor: what the model is sent, what ends up in the history, what the
//! [`zen_core::RunResult`] says and which events/hooks fire.

use std::sync::Arc;

use serde_json::{json, Value};
use zen_core::*;

use crate::agent::combine_permission;
use crate::subagent::subagent_mode;
use crate::testutil::*;
use crate::{AgentConfig, SubAgentExecutor};

fn fake(name: &'static str, read_only: bool, out: &str) -> Arc<dyn Tool> {
    Arc::new(FakeTool { name, read_only, output: ToolOutput::ok(out) })
}

fn allow() -> Arc<FakePerms> {
    FakePerms::new(PermissionDecision::Allow)
}

/// Every assistant tool_call id in `msgs` has a matching `tool` message after it.
fn assert_paired(msgs: &[ChatMessage]) {
    for (i, m) in msgs.iter().enumerate() {
        if let Some(calls) = &m.tool_calls {
            for c in calls {
                assert!(
                    msgs[i + 1..]
                        .iter()
                        .any(|t| t.role == "tool" && t.tool_call_id.as_deref() == Some(&c.id)),
                    "tool_call {} has no tool result in {:#?}",
                    c.id,
                    msgs
                );
            }
        }
    }
}

#[tokio::test]
async fn text_reply_is_the_result_and_system_prompt_is_sent_not_stored() {
    let llm = MockLlm::new(vec![resp("hello")]);
    let (mut agent, _sink, _h) = build_agent(llm.clone(), ToolRegistry::new(), allow(), base_cfg());
    let r = agent.run_turn("hi").await;
    assert_eq!(r.subtype, "success");
    assert!(!r.is_error);
    assert_eq!(r.result, "hello");
    assert_eq!(r.num_turns, 1);
    assert_eq!(r.usage.input_tokens, 10);
    assert_eq!(r.usage.output_tokens, 5);
    let req = &llm.requests()[0];
    assert_eq!(req.messages[0].role, "system");
    assert_eq!(req.messages[0].content.as_deref(), Some("you are a test"));
    assert_eq!(agent.messages()[0].role, "user", "system prompt must not be stored in history");
}

#[tokio::test]
async fn prune_old_tool_results_end_to_end() {
    // 8 model calls: 7 tool calls in a row (each returning a large,
    // never-referenced result), then a final text reply that ends the turn.
    // prune_after=2 → results from turns 1..=5 are old enough to prune by the
    // last call.
    let big = "x".repeat(12_000); // ~3k tokens
    let mut scripted = vec![];
    for i in 1..=7 {
        scripted.push(resp_tool(
            "Read",
            &format!(r#"{{"path": "src/f{}.rs"}}"#, i),
            &format!("c{}", i),
        ));
    }
    scripted.push(resp("done"));
    let llm = MockLlm::new(scripted);
    let reg = registry_with(vec![fake("Read", true, &big)]);
    let mut cfg = base_cfg();
    cfg.prune.prune_after = 2;
    let (mut agent, sink, _h) = build_agent(llm.clone(), reg, allow(), cfg);

    let r = agent.run_turn("go").await;
    assert_eq!(r.subtype, "success");

    // Old results were pruned and stats accumulated.
    let stats = agent.prune_stats();
    assert!(stats.pruned >= 4, "expected pruning, got {:?}", stats);
    assert!(stats.pruned_tokens > 0);

    // The last request the model saw contains pruned summaries, not raw output.
    let requests = llm.requests();
    let last_req = requests.last().unwrap();
    let pruned_summaries: Vec<_> = last_req
        .messages
        .iter()
        .filter(|m| {
            m.role == "tool"
                && m.content.as_deref().unwrap_or("").starts_with("[zen: pruned")
        })
        .collect();
    assert!(
        !pruned_summaries.is_empty(),
        "expected [zen: pruned markers in the final request"
    );
    // Summaries keep the identifier and the tool_call_id (structure preserved).
    let first = pruned_summaries[0].content.as_deref().unwrap();
    assert!(first.contains("src/f1.rs"), "identifier preserved: {}", first);
    assert!(pruned_summaries[0].tool_call_id.is_some());

    // A Pruned event was emitted for observability.
    let events = sink.events.lock().unwrap();
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Pruned { pruned, tokens_saved }
                if *pruned > 0 && *tokens_saved > 0
        )),
        "expected AgentEvent::Pruned"
    );

    // History stays paired: every tool_call still has its tool result.
    assert_paired(agent.messages());
}

#[tokio::test]
async fn no_prune_disables_pruning_and_capping() {
    let big = "x".repeat(12_000);
    let llm = MockLlm::new(vec![
        resp_tool("Read", r#"{"path": "src/f1.rs"}"#, "c1"),
        resp("done"),
    ]);
    let reg = registry_with(vec![fake("Read", true, &big)]);
    let mut cfg = base_cfg();
    cfg.prune.enabled = false;
    let (mut agent, _sink, _h) = build_agent(llm.clone(), reg, allow(), cfg);

    agent.run_turn("go").await;
    assert_eq!(agent.prune_stats().pruned, 0);
    assert_eq!(agent.prune_stats().capped, 0);
    // Raw large result is untouched in history.
    let tool_msg = agent
        .messages()
        .iter()
        .find(|m| m.role == "tool")
        .unwrap();
    assert_eq!(tool_msg.content.as_deref().unwrap().len(), 12_000);
}

#[tokio::test]
async fn oversized_tool_result_is_capped_with_reread_hint() {
    // 40k chars ≈ 10k tokens > 8k cap.
    let huge = "y".repeat(40_000);
    let llm = MockLlm::new(vec![
        resp_tool("Read", r#"{"path": "src/big.rs"}"#, "c1"),
        resp("done"),
    ]);
    let reg = registry_with(vec![fake("Read", true, &huge)]);
    let (mut agent, _sink, _h) = build_agent(llm.clone(), reg, allow(), base_cfg());

    agent.run_turn("go").await;
    let stats = agent.prune_stats();
    assert_eq!(stats.capped, 1);
    assert!(stats.capped_tokens > 0);

    let tool_msg = agent
        .messages()
        .iter()
        .find(|m| m.role == "tool")
        .unwrap();
    let content = tool_msg.content.as_deref().unwrap();
    assert!(
        content.starts_with("[zen: truncated"),
        "capped result carries the marker"
    );
    assert!(content.contains("re-read with a narrower query"));
    assert!(content.contains("src/big.rs"), "label preserved: {}", &content[..120]);
}

#[tokio::test]
async fn max_output_tokens_is_threaded_into_the_request() {
    let llm = MockLlm::new(vec![resp("hello")]);
    let mut cfg = base_cfg();
    cfg.max_output_tokens = Some(512);
    let (mut agent, _sink, _h) = build_agent(llm.clone(), ToolRegistry::new(), allow(), cfg);
    let r = agent.run_turn("hi").await;
    assert_eq!(r.subtype, "success");
    assert_eq!(llm.requests()[0].max_tokens, Some(512));
}

#[tokio::test]
async fn max_output_tokens_defaults_to_none() {
    let llm = MockLlm::new(vec![resp("hello")]);
    let (mut agent, _sink, _h) = build_agent(llm.clone(), ToolRegistry::new(), allow(), base_cfg());
    let r = agent.run_turn("hi").await;
    assert_eq!(r.subtype, "success");
    assert_eq!(llm.requests()[0].max_tokens, None);
}

#[tokio::test]
async fn tool_call_roundtrip_feeds_result_back_and_accumulates_usage() {
    let llm = MockLlm::new(vec![resp_tool("Echo", r#"{"x":1}"#, "c1"), resp("done")]);
    let reg = registry_with(vec![fake("Echo", true, "ECHOED")]);
    let (mut agent, sink, _h) = build_agent(llm.clone(), reg, allow(), base_cfg());
    let r = agent.run_turn("go").await;
    assert_eq!(r.result, "done");
    assert_eq!(r.num_turns, 2);
    assert_eq!(r.usage.input_tokens, 30);
    assert_eq!(r.usage.output_tokens, 13);
    let second = &llm.requests()[1];
    let tool_msg = second.messages.iter().find(|m| m.role == "tool").unwrap();
    assert_eq!(tool_msg.tool_call_id.as_deref(), Some("c1"));
    assert_eq!(tool_msg.content.as_deref(), Some("ECHOED"));
    let events = sink.events.lock().unwrap();
    assert!(events.iter().any(|e| matches!(e,
        AgentEvent::ToolResult { tool_use_id, content, is_error: false, .. }
            if tool_use_id == "c1" && content == "ECHOED")));
}

#[tokio::test]
async fn unknown_tool_and_bad_json_become_tool_errors_not_crashes() {
    let llm = MockLlm::new(vec![
        resp_tool("Nope", "{}", "u1"),
        resp_tool("Echo", "{not json", "b1"),
        resp("recovered"),
    ]);
    let reg = registry_with(vec![fake("Echo", true, "x")]);
    let (mut agent, _s, _h) = build_agent(llm.clone(), reg, allow(), base_cfg());
    let r = agent.run_turn("go").await;
    assert_eq!(r.result, "recovered");
    let msgs = agent.messages();
    let contents: Vec<&str> =
        msgs.iter().filter(|m| m.role == "tool").filter_map(|m| m.content.as_deref()).collect();
    assert!(contents[0].contains("Unknown tool 'Nope'"));
    assert!(contents[1].contains("Invalid JSON arguments"));
    assert_paired(msgs);
}

#[tokio::test]
async fn deny_is_recorded_in_result_and_event() {
    let llm = MockLlm::new(vec![resp_tool("Write", r#"{"p":"a"}"#, "w1"), resp("ok")]);
    let reg = registry_with(vec![fake("Write", false, "wrote")]);
    let perms = FakePerms::new(PermissionDecision::Deny("rule says no".into()));
    let (mut agent, sink, _h) = build_agent(llm, reg, perms, base_cfg());
    let r = agent.run_turn("go").await;
    assert_eq!(r.permission_denials.len(), 1);
    assert_eq!(r.permission_denials[0].tool_name, "Write");
    assert_eq!(r.permission_denials[0].tool_use_id, "w1");
    assert_eq!(r.permission_denials[0].tool_input, json!({"p":"a"}));
    let tool_msg = agent.messages().iter().find(|m| m.role == "tool").unwrap();
    assert_eq!(tool_msg.content.as_deref(), Some("rule says no"));
    assert!(sink.events.lock().unwrap().iter().any(|e| matches!(e, AgentEvent::PermissionDenied(_))));
}

#[tokio::test]
async fn ask_without_prompter_denies_in_headless_mode() {
    let llm = MockLlm::new(vec![resp_tool("Write", "{}", "w1"), resp("ok")]);
    let reg = registry_with(vec![fake("Write", false, "wrote")]);
    let (mut agent, _s, _h) =
        build_agent(llm, reg, FakePerms::new(PermissionDecision::Ask), base_cfg());
    let r = agent.run_turn("go").await;
    assert_eq!(r.permission_denials.len(), 1);
    let tool_msg = agent.messages().iter().find(|m| m.role == "tool").unwrap();
    assert!(tool_msg.content.as_deref().unwrap().contains("non-interactive"));
}

#[tokio::test]
async fn plan_mode_hides_mutating_tools_and_returns_plan_text() {
    let llm = MockLlm::new(vec![resp("1. do X\n2. do Y")]);
    let reg = registry_with(vec![fake("Read", true, "r"), fake("Write", false, "w")]);
    let cfg = AgentConfig { permission_mode: PermissionMode::Plan, ..base_cfg() };
    let (mut agent, _s, _h) = build_agent(llm.clone(), reg, allow(), cfg);
    let r = agent.run_turn("plan it").await;
    assert_eq!(r.result, "1. do X\n2. do Y");
    let names: Vec<String> =
        llm.requests()[0].tools.iter().map(|t| t.function.name.clone()).collect();
    assert!(names.contains(&"Read".to_string()));
    assert!(!names.contains(&"Write".to_string()));
}

#[tokio::test]
async fn max_turns_stops_with_error_subtype() {
    let llm = MockLlm::new(vec![
        resp_tool("Echo", "{}", "a"),
        resp_tool("Echo", "{}", "b"),
        resp("never reached"),
    ]);
    let reg = registry_with(vec![fake("Echo", true, "x")]);
    let cfg = AgentConfig { max_turns: 2, ..base_cfg() };
    let (mut agent, _s, _h) = build_agent(llm, reg, allow(), cfg);
    let r = agent.run_turn("go").await;
    assert_eq!(r.subtype, "error_max_turns");
    assert!(r.is_error);
    assert_eq!(r.num_turns, 2);
}

#[tokio::test]
async fn model_failure_surfaces_as_error_result() {
    let llm = MockLlm::with_errors(vec![Err(anyhow::anyhow!("boom 500"))]);
    let (mut agent, _s, _h) = build_agent(llm, ToolRegistry::new(), allow(), base_cfg());
    let r = agent.run_turn("go").await;
    assert_eq!(r.subtype, "error_during_execution");
    assert!(r.is_error);
    assert!(r.result.contains("boom 500"));
}

#[tokio::test]
async fn fallback_model_is_used_after_primary_fails() {
    let llm = MockLlm::with_errors(vec![Err(anyhow::anyhow!("primary down")), Ok(resp("via fb"))]);
    let cfg = AgentConfig { fallback_models: vec!["fb-model".into()], ..base_cfg() };
    let (mut agent, _s, _h) = build_agent(llm.clone(), ToolRegistry::new(), allow(), cfg);
    let r = agent.run_turn("go").await;
    assert_eq!(r.result, "via fb");
    let models: Vec<String> = llm.requests().iter().map(|r| r.model.clone()).collect();
    assert_eq!(models, vec!["mock-model".to_string(), "fb-model".to_string()]);
}

// ── hooks (SPEC C3) ──────────────────────────────────────────────────────────

#[tokio::test]
async fn c3_10_user_prompt_submit_block_skips_model_and_context_is_appended() {
    // Block.
    let llm = MockLlm::new(vec![]);
    let (mut agent, _s, hooks) = build_agent(llm.clone(), ToolRegistry::new(), allow(), base_cfg());
    hooks.push(
        HookEvent::UserPromptSubmit,
        HookOutcome { blocked: true, feedback: Some("no secrets".into()), ..Default::default() },
    );
    let r = agent.run_turn("my password is x").await;
    assert!(r.is_error);
    assert_eq!(r.result, "no secrets");
    assert!(llm.requests().is_empty(), "blocked prompt must not reach the model");

    // Additional context.
    let llm = MockLlm::new(vec![resp("ok")]);
    let (mut agent, _s, hooks) = build_agent(llm.clone(), ToolRegistry::new(), allow(), base_cfg());
    hooks.push(
        HookEvent::UserPromptSubmit,
        HookOutcome { feedback: Some("CTX: branch main".into()), ..Default::default() },
    );
    agent.run_turn("hello").await;
    let user = llm.requests()[0].messages[1].content.clone().unwrap();
    assert!(user.starts_with("hello") && user.contains("CTX: branch main"));
    let calls = hooks.calls.lock().unwrap();
    assert_eq!(calls[0].2["prompt"], "hello");
}

#[tokio::test]
async fn c3_7_pre_tool_use_block_and_updated_input() {
    let llm = MockLlm::new(vec![
        resp_tool("Echo", r#"{"v":"orig"}"#, "c1"),
        resp_tool("Echo", r#"{"v":"orig"}"#, "c2"),
        resp("end"),
    ]);
    let reg = registry_with(vec![fake("Echo", true, "ran")]);
    let (mut agent, _s, hooks) = build_agent(llm, reg, allow(), base_cfg());
    hooks.push(
        HookEvent::PreToolUse,
        HookOutcome { blocked: true, feedback: Some("blocked by policy".into()), ..Default::default() },
    );
    hooks.push(
        HookEvent::PreToolUse,
        HookOutcome { updated_input: Some(json!({"v":"rewritten"})), ..Default::default() },
    );
    agent.run_turn("go").await;
    let tools: Vec<String> = agent
        .messages()
        .iter()
        .filter(|m| m.role == "tool")
        .map(|m| m.content.clone().unwrap())
        .collect();
    assert_eq!(tools, vec!["blocked by policy".to_string(), "ran".to_string()]);
    let calls = hooks.calls.lock().unwrap();
    let pre: Vec<&(HookEvent, String, Value)> =
        calls.iter().filter(|c| c.0 == HookEvent::PreToolUse).collect();
    assert_eq!(pre[0].1, "Echo", "matcher target is the tool name");
    assert_eq!(pre[0].2["tool_use_id"], "c1");
    // PostToolUse sees the rewritten input.
    let post = calls.iter().find(|c| c.0 == HookEvent::PostToolUse).unwrap();
    assert_eq!(post.2["tool_input"], json!({"v":"rewritten"}));
    assert_eq!(post.2["tool_response"], "ran");
}

#[tokio::test]
async fn c2_13_hook_allow_cannot_override_rule_deny() {
    assert_eq!(
        combine_permission(PermissionDecision::Deny("r".into()), Some(PermissionDecision::Allow), false),
        PermissionDecision::Deny("r".into())
    );
    assert_eq!(
        combine_permission(PermissionDecision::Ask, Some(PermissionDecision::Allow), false),
        PermissionDecision::Allow
    );
    assert_eq!(
        combine_permission(PermissionDecision::Allow, Some(PermissionDecision::Ask), false),
        PermissionDecision::Ask
    );
    assert_eq!(
        combine_permission(PermissionDecision::Allow, Some(PermissionDecision::Deny("h".into())), false),
        PermissionDecision::Deny("h".into())
    );
    assert_eq!(combine_permission(PermissionDecision::Allow, None, false), PermissionDecision::Allow);

    // An explicit ask rule is not answered by a hook allow.
    assert_eq!(
        combine_permission(PermissionDecision::Ask, Some(PermissionDecision::Allow), true),
        PermissionDecision::Ask
    );

    // End to end: rule deny + hook allow → still denied.
    let llm = MockLlm::new(vec![resp_tool("Write", "{}", "w"), resp("ok")]);
    let reg = registry_with(vec![fake("Write", false, "wrote")]);
    let perms = FakePerms::new(PermissionDecision::Deny("deny rule".into()));
    let (mut agent, _s, hooks) = build_agent(llm, reg, perms, base_cfg());
    hooks.push(
        HookEvent::PreToolUse,
        HookOutcome { permission_override: Some(PermissionDecision::Allow), ..Default::default() },
    );
    let r = agent.run_turn("go").await;
    assert_eq!(r.permission_denials.len(), 1);
}

/// Rule/mode checker that answers `Ask`, optionally because an explicit ask rule matched.
struct AskPerms {
    ask_rule: bool,
}

impl PermissionChecker for AskPerms {
    fn check(&self, _n: &str, _i: &Value, _ro: bool, _c: &ToolContext) -> PermissionDecision {
        PermissionDecision::Ask
    }
    fn ask_rule_matches(&self, _n: &str, _i: &Value, _c: &ToolContext) -> bool {
        self.ask_rule
    }
}

/// C2.13: a PreToolUse `allow` answers a mode-default prompt but not an explicit ask rule.
#[tokio::test]
async fn c2_13_hook_allow_cannot_override_ask_rule_end_to_end() {
    for (ask_rule, expect_denied) in [(true, 1usize), (false, 0usize)] {
        let llm = MockLlm::new(vec![resp_tool("Write", "{}", "w"), resp("ok")]);
        let reg = registry_with(vec![fake("Write", false, "wrote")]);
        let (mut agent, _s, hooks) =
            build_agent(llm, reg, Arc::new(AskPerms { ask_rule }), base_cfg());
        hooks.push(
            HookEvent::PreToolUse,
            HookOutcome { permission_override: Some(PermissionDecision::Allow), ..Default::default() },
        );
        let r = agent.run_turn("go").await;
        assert_eq!(r.permission_denials.len(), expect_denied, "ask_rule={}", ask_rule);
    }
}

#[tokio::test]
async fn c3_6_pre_tool_use_continue_false_stops_and_keeps_history_valid() {
    let llm = MockLlm::new(vec![ChatResponse {
        message: ChatMessage::assistant(
            None,
            Some(vec![
                ToolCall {
                    id: "a".into(),
                    call_type: "function".into(),
                    function: FunctionCall { name: "Echo".into(), arguments: "{}".into() },
                },
                ToolCall {
                    id: "b".into(),
                    call_type: "function".into(),
                    function: FunctionCall { name: "Echo".into(), arguments: "{}".into() },
                },
            ]),
        ),
        usage: Usage::default(),
        finish_reason: None,
    }]);
    let reg = registry_with(vec![fake("Echo", true, "x")]);
    let (mut agent, _s, hooks) = build_agent(llm, reg, allow(), base_cfg());
    hooks.push(
        HookEvent::PreToolUse,
        HookOutcome {
            stop_run: true,
            stop_reason: Some("Build is red, stopping".into()),
            system_message: Some("warn".into()),
            ..Default::default()
        },
    );
    let r = agent.run_turn("go").await;
    assert_eq!(r.result, "Build is red, stopping");
    assert_paired(agent.messages());
}

#[tokio::test]
async fn c3_8_post_tool_use_updated_output_and_feedback() {
    let llm = MockLlm::new(vec![resp_tool("Echo", "{}", "c1"), resp("fin")]);
    let reg = registry_with(vec![fake("Echo", true, "raw output")]);
    let (mut agent, _s, hooks) = build_agent(llm.clone(), reg, allow(), base_cfg());
    hooks.push(
        HookEvent::PostToolUse,
        HookOutcome {
            updated_output: Some("redacted".into()),
            feedback: Some("lint: 2 warnings".into()),
            ..Default::default()
        },
    );
    agent.run_turn("go").await;
    let tool = llm.requests()[1].messages.iter().find(|m| m.role == "tool").cloned().unwrap();
    assert_eq!(tool.content.as_deref(), Some("redacted\nlint: 2 warnings"));
}

#[tokio::test]
async fn c3_2_failed_tool_fires_post_tool_use_failure() {
    let llm = MockLlm::new(vec![resp_tool("Bad", "{}", "c1"), resp("fin")]);
    let bad: Arc<dyn Tool> =
        Arc::new(FakeTool { name: "Bad", read_only: true, output: ToolOutput::err("exploded") });
    let (mut agent, _s, hooks) = build_agent(llm, registry_with(vec![bad]), allow(), base_cfg());
    agent.run_turn("go").await;
    let calls = hooks.calls.lock().unwrap();
    let f = calls.iter().find(|c| c.0 == HookEvent::PostToolUseFailure).expect("failure hook");
    assert_eq!(f.2["error"], "exploded");
    assert!(!calls.iter().any(|c| c.0 == HookEvent::PostToolUse));
}

#[tokio::test]
async fn c3_9_stop_hook_block_continues_with_reason_and_sets_stop_hook_active() {
    let llm = MockLlm::new(vec![resp("first"), resp("second")]);
    let (mut agent, _s, hooks) = build_agent(llm.clone(), ToolRegistry::new(), allow(), base_cfg());
    hooks.push(
        HookEvent::Stop,
        HookOutcome { blocked: true, feedback: Some("run the tests".into()), ..Default::default() },
    );
    let r = agent.run_turn("go").await;
    assert_eq!(r.result, "second");
    assert_eq!(r.num_turns, 2);
    let second = &llm.requests()[1];
    assert_eq!(second.messages.last().unwrap().content.as_deref(), Some("run the tests"));
    let calls = hooks.calls.lock().unwrap();
    let stops: Vec<&Value> =
        calls.iter().filter(|c| c.0 == HookEvent::Stop).map(|c| &c.2).collect();
    assert_eq!(stops[0]["stop_hook_active"], false);
    assert_eq!(stops[0]["last_assistant_message"], "first");
    assert_eq!(stops[1]["stop_hook_active"], true);
}

#[tokio::test]
async fn c3_9_stop_hook_loop_guard_caps_continuations() {
    let n = 3;
    let llm = MockLlm::new((0..=n).map(|i| resp(&format!("r{}", i))).collect());
    let (mut agent, _s, hooks) = build_agent(llm.clone(), ToolRegistry::new(), allow(), base_cfg());
    agent.set_stop_hook_max_continues(n);
    for _ in 0..10 {
        hooks.push(HookEvent::Stop, HookOutcome { blocked: true, ..Default::default() });
    }
    let r = agent.run_turn("go").await;
    assert_eq!(r.subtype, "success");
    assert_eq!(llm.requests().len(), n + 1, "initial answer + n continuations");
    assert_eq!(crate::DEFAULT_STOP_HOOK_CONTINUES, 8);
}

#[tokio::test]
async fn system_message_from_hook_is_emitted_to_sink_only() {
    let llm = MockLlm::new(vec![resp("ok")]);
    let (mut agent, sink, hooks) = build_agent(llm.clone(), ToolRegistry::new(), allow(), base_cfg());
    hooks.push(
        HookEvent::UserPromptSubmit,
        HookOutcome { system_message: Some("heads up".into()), ..Default::default() },
    );
    agent.run_turn("hi").await;
    assert!(sink.events.lock().unwrap().iter().any(|e| matches!(e,
        AgentEvent::SystemMessage { event, text } if event == "UserPromptSubmit" && text == "heads up")));
    let sent = serde_json::to_string(&llm.requests()[0].messages).unwrap();
    assert!(!sent.contains("heads up"), "systemMessage must not reach the model");
}

// ── compaction (SPEC C10) ───────────────────────────────────────────────────

#[tokio::test]
async fn c10_manual_compact_replaces_history_keeps_pairs_and_fires_hooks() {
    let llm = MockLlm::new(vec![
        resp_tool("Echo", "{}", "c1"),
        resp("answer one"),
        resp("SUMMARY TEXT"),
    ]);
    let reg = registry_with(vec![fake("Echo", true, "x")]);
    let cfg = AgentConfig { keep_after_compact: 2, ..base_cfg() };
    let (mut agent, sink, hooks) = build_agent(llm.clone(), reg, allow(), cfg);
    agent.run_turn("first").await;
    agent.compact(Some("focus on parser"), "manual").await.unwrap();
    let msgs = agent.messages();
    assert!(msgs[0].content.as_deref().unwrap().contains("SUMMARY TEXT"));
    assert!(msgs[0].content.as_deref().unwrap().starts_with("[Compaction summary"));
    assert_paired(msgs);
    assert!(msgs.iter().all(|m| m.role != "tool" || msgs.iter().any(|a| a.tool_calls.is_some())));
    // Summarization request: no tools, focus included.
    let sreq = llm.requests().last().cloned().unwrap();
    assert!(sreq.tools.is_empty());
    assert!(sreq.messages.last().unwrap().content.as_deref().unwrap().contains("focus on parser"));
    // Hooks and boundary event.
    let calls = hooks.calls.lock().unwrap();
    let pre = calls.iter().find(|c| c.0 == HookEvent::PreCompact).unwrap();
    assert_eq!(pre.1, "manual");
    assert_eq!(pre.2["custom_instructions"], "focus on parser");
    assert!(calls.iter().any(|c| c.0 == HookEvent::PostCompact));
    assert!(sink.events.lock().unwrap().iter().any(|e| matches!(e,
        AgentEvent::CompactBoundary { trigger, .. } if trigger == "manual")));
}

#[tokio::test]
async fn compaction_request_is_one_transcript_message_and_summary_tracks_files() {
    // Pi-inspired: summarizer gets a flattened transcript (no system prompt, no raw chat) and the
    // stored summary carries deterministic <read-files>/<modified-files> lists.
    let llm = MockLlm::new(vec![
        resp_tool("Read", r#"{"file_path":"src/a.rs"}"#, "c1"),
        resp("answer one"),
        resp("SUMMARY TEXT"),
    ]);
    let reg = registry_with(vec![fake("Read", true, "file body")]);
    let cfg = AgentConfig { keep_after_compact: 1, system_prompt: "SYSTEM-PROMPT-XYZ".into(), ..base_cfg() };
    let (mut agent, _sink, _hooks) = build_agent(llm.clone(), reg, allow(), cfg);
    agent.run_turn("first").await;
    agent.compact(None, "manual").await.unwrap();
    let sreq = llm.requests().last().cloned().unwrap();
    assert_eq!(sreq.messages.len(), 1, "one user message: {:?}", sreq.messages);
    assert_eq!(sreq.messages[0].role, "user");
    let body = sreq.messages[0].content.as_deref().unwrap();
    assert!(body.contains("[User]: first"));
    assert!(body.contains(r#"[Assistant tool calls]: Read({"file_path":"src/a.rs"})"#));
    assert!(body.contains("[Tool result]: file body"));
    assert!(body.contains("Summarize this coding session"));
    assert!(!body.contains("SYSTEM-PROMPT-XYZ"));
    let summary = agent.messages()[0].content.as_deref().unwrap();
    assert!(summary.contains("SUMMARY TEXT\n\n<read-files>\nsrc/a.rs\n</read-files>"), "{summary}");
}

#[tokio::test]
async fn c10_pre_compact_block_leaves_history_untouched() {
    let llm = MockLlm::new(vec![resp("a")]);
    let (mut agent, _s, hooks) = build_agent(llm.clone(), ToolRegistry::new(), allow(), base_cfg());
    agent.run_turn("x").await;
    let before = agent.messages().to_vec();
    hooks.push(HookEvent::PreCompact, HookOutcome { blocked: true, ..Default::default() });
    agent.compact(None, "manual").await.unwrap();
    assert_eq!(agent.messages(), &before[..]);
    assert_eq!(llm.requests().len(), 1, "no summarization call when vetoed");
}

#[tokio::test]
async fn c10_auto_compact_triggers_over_threshold() {
    // First turn reports 10 prompt tokens; threshold 5 → next turn compacts first.
    let llm = MockLlm::new(vec![resp("one"), resp("SUM"), resp("two")]);
    let cfg = AgentConfig { auto_compact_threshold: Some(5), keep_after_compact: 1, ..base_cfg() };
    let (mut agent, sink, _h) = build_agent(llm.clone(), ToolRegistry::new(), allow(), cfg);
    agent.run_turn("first").await;
    let r = agent.run_turn("second").await;
    assert_eq!(r.result, "two");
    assert!(sink.events.lock().unwrap().iter().any(|e| matches!(e,
        AgentEvent::CompactBoundary { trigger, .. } if trigger == "auto")));
    let last = llm.requests().last().cloned().unwrap();
    let joined = serde_json::to_string(&last.messages).unwrap();
    assert!(joined.contains("SUM"));
    assert!(!joined.contains("\"one\""), "compacted history must not be resent verbatim");
}

#[tokio::test]
async fn set_messages_replaces_history_for_resume() {
    let llm = MockLlm::new(vec![resp("cont")]);
    let (mut agent, _s, _h) = build_agent(llm.clone(), ToolRegistry::new(), allow(), base_cfg());
    agent.set_messages(vec![ChatMessage::user("old q"), ChatMessage::assistant(Some("old a".into()), None)]);
    agent.run_turn("new q").await;
    let sent: Vec<String> =
        llm.requests()[0].messages.iter().filter_map(|m| m.content.clone()).collect();
    assert_eq!(sent, vec!["you are a test", "old q", "old a", "new q"]);
}

// ── sub-agents (SPEC C6) ────────────────────────────────────────────────────

fn executor(llm: Arc<MockLlm>, tools: ToolRegistry, hooks: Arc<FakeHooks>) -> (SubAgentExecutor, Arc<CollectSink>) {
    let sink = Arc::new(CollectSink::default());
    let ex = SubAgentExecutor::new(llm, tools, allow(), hooks, sink.clone(), temp_ctx(), base_cfg());
    (ex, sink)
}

#[tokio::test]
async fn c6_subagent_fresh_context_tool_filter_and_parent_id() {
    let llm = MockLlm::new(vec![resp_tool("Read", "{}", "s1"), resp("sub result")]);
    let reg = registry_with(vec![
        fake("Read", true, "r"),
        fake("Write", false, "w"),
        fake("Task", true, "t"),
        fake("Grep", true, "g"),
    ]);
    let hooks = FakeHooks::new();
    let (ex, sink) = executor(llm.clone(), reg, hooks.clone());
    let def = AgentDefinition {
        name: "reader".into(),
        description: "reads".into(),
        tools: Some(vec!["Read".into(), "Task".into(), "Grep".into()]),
        disallowed_tools: vec!["Grep".into()],
        max_turns: Some(5),
        ..Default::default()
    };
    let out = ex.run_subagent(&def, "find it", "task-1").await.unwrap();
    assert_eq!(out, "sub result");
    let req = &llm.requests()[0];
    let names: Vec<String> = req.tools.iter().map(|t| t.function.name.clone()).collect();
    assert_eq!(names, vec!["Read".to_string()], "allowlist minus disallowed minus Task at max depth");
    assert_eq!(req.messages.len(), 2, "fresh context: system + prompt only");
    assert!(sink.events.lock().unwrap().iter().all(|e| match e {
        AgentEvent::Assistant { parent_tool_use_id, .. }
        | AgentEvent::ToolResult { parent_tool_use_id, .. } => parent_tool_use_id.as_deref() == Some("task-1"),
        _ => true,
    }));
    let calls = hooks.calls.lock().unwrap();
    let start = calls.iter().position(|c| c.0 == HookEvent::SubagentStart).unwrap();
    let stop = calls.iter().position(|c| c.0 == HookEvent::SubagentStop).unwrap();
    assert!(start < stop);
    assert_eq!(calls[stop].1, "reader");
    assert_eq!(calls[stop].2["last_assistant_message"], "sub result");
    assert!(!calls.iter().any(|c| c.0 == HookEvent::Stop), "sub-agents fire SubagentStop, not Stop");
}

#[tokio::test]
async fn c6_subagent_stop_block_continues_subagent() {
    let llm = MockLlm::new(vec![resp("draft"), resp("final")]);
    let hooks = FakeHooks::new();
    hooks.push(
        HookEvent::SubagentStop,
        HookOutcome { blocked: true, feedback: Some("add tests".into()), ..Default::default() },
    );
    let (ex, _sink) = executor(llm.clone(), ToolRegistry::new(), hooks);
    let def = AgentDefinition { name: "w".into(), description: "d".into(), ..Default::default() };
    let out = ex.run_subagent(&def, "do", "t").await.unwrap();
    assert_eq!(out, "final");
    assert_eq!(llm.requests()[1].messages.last().unwrap().content.as_deref(), Some("add tests"));
}

#[tokio::test]
async fn c6_depth_limit_refuses_nested_spawn() {
    let llm = MockLlm::new(vec![]);
    let hooks = FakeHooks::new();
    let sink = Arc::new(CollectSink::default());
    let cfg = AgentConfig { depth: 1, ..base_cfg() };
    let ex = SubAgentExecutor::new(llm, ToolRegistry::new(), allow(), hooks, sink, temp_ctx(), cfg);
    let def = AgentDefinition { name: "deep".into(), description: "d".into(), ..Default::default() };
    let err = ex.run_subagent(&def, "x", "t").await.unwrap_err().to_string();
    assert!(err.contains("depth limit"));
}

#[tokio::test]
async fn c6_1_preloads_skills_into_subagent_system_prompt() {
    let llm = MockLlm::new(vec![resp("done")]);
    let (mut ex, sink) = executor(llm.clone(), ToolRegistry::new(), FakeHooks::new());
    let cwd = ex.ctx.cwd.lock().unwrap().clone();
    let skill_dir = cwd.join(".zen/skills/style");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("SKILL.md"), "---\nname: style\ndescription: S\n---\nUse tabs. See ${ZEN_SKILL_DIR}/x\n").unwrap();
    ex.skills = vec![SkillInfo {
        name: "known".into(),
        description: "k".into(),
        path: std::path::PathBuf::from("/k/SKILL.md"),
        body: "KNOWN BODY".into(),
        allowed_tools: vec![],
    }];
    let def = AgentDefinition {
        name: "w".into(),
        description: "d".into(),
        system_prompt: "Base.".into(),
        skills: vec!["style".into(), "known".into(), "ghost".into()],
        ..Default::default()
    };
    assert_eq!(ex.run_subagent(&def, "go", "t").await.unwrap(), "done");
    let sys = llm.requests()[0].messages[0].content.clone().unwrap();
    assert!(sys.starts_with("Base."));
    // Compare paths independent of the separator (Windows mixes `\` and `/`).
    let norm = |p: &str| p.replace('\\', "/");
    let want = format!("## Skill: style\nUse tabs. See {}/x", skill_dir.display());
    assert!(norm(&sys).contains(&norm(&want)), "{}", sys);
    assert!(!sys.contains("description: S"), "frontmatter stripped");
    assert!(sys.contains("## Skill: known\nKNOWN BODY"));
    assert!(sink.events.lock().unwrap().iter().any(|e| matches!(e,
        AgentEvent::SystemMessage { text, .. } if text.contains("'ghost' not found"))));
}

#[tokio::test]
async fn c6_3_builtin_read_only_agent_and_no_nested_task_by_default() {
    // A plan-mode, read-only definition (like the built-in Plan/Explore) only sees its tools,
    // never Task (default max depth 1), and returns only its final text.
    let llm = MockLlm::new(vec![resp_tool("Grep", "{}", "g1"), resp("only this")]);
    let reg = registry_with(vec![
        fake("Read", true, "r"),
        fake("Grep", true, "grep-out"),
        fake("Write", false, "w"),
        fake("Bash", false, "b"),
        fake("Task", true, "t"),
    ]);
    let (ex, _sink) = executor(llm.clone(), reg, FakeHooks::new());
    let def = AgentDefinition {
        name: "Plan".into(),
        description: "d".into(),
        tools: Some(vec!["Read".into(), "Grep".into(), "Task".into()]),
        permission_mode: Some("plan".into()),
        ..Default::default()
    };
    let out = ex.run_subagent(&def, "investigate", "task-9").await.unwrap();
    assert_eq!(out, "only this", "intermediate tool output is not returned to the parent");
    let names: Vec<String> = llm.requests()[0].tools.iter().map(|t| t.function.name.clone()).collect();
    // tools_for_request emits definitions in stable name-sorted order (prompt-cache stability).
    assert_eq!(names, vec!["Grep".to_string(), "Read".to_string()]);
    // A deeper limit keeps Task available to the first level.
    let llm2 = MockLlm::new(vec![resp("ok")]);
    let reg2 = registry_with(vec![fake("Read", true, "r"), fake("Task", true, "t")]);
    let (mut ex2, _s) = executor(llm2.clone(), reg2, FakeHooks::new());
    ex2.set_max_depth(2);
    let gp = AgentDefinition { name: "gp".into(), description: "d".into(), ..Default::default() };
    ex2.run_subagent(&gp, "x", "t").await.unwrap();
    let names2: Vec<String> = llm2.requests()[0].tools.iter().map(|t| t.function.name.clone()).collect();
    assert!(names2.contains(&"Task".to_string()));
}

#[test]
fn c6_subagent_mode_cannot_escalate() {
    use PermissionMode::*;
    assert_eq!(subagent_mode(Default, Some("bypassPermissions")), Default);
    assert_eq!(subagent_mode(Default, Some("acceptEdits")), Default);
    assert_eq!(subagent_mode(Default, Some("plan")), Plan);
    assert_eq!(subagent_mode(AcceptEdits, Some("acceptEdits")), AcceptEdits);
    assert_eq!(subagent_mode(BypassPermissions, Some("bypassPermissions")), BypassPermissions);
    assert_eq!(subagent_mode(Plan, Some("garbage")), Plan);
    assert_eq!(subagent_mode(Default, None), Default);
}

/// Prompter double that always answers yes.
struct YesPrompter;
impl Prompter for YesPrompter {
    fn ask_permission(&self, _: &str, _: &Value) -> PromptAnswer {
        PromptAnswer::Yes
    }
}

#[tokio::test]
async fn c3_2_notification_hook_fires_before_permission_prompt() {
    let llm = MockLlm::new(vec![resp_tool("Write", "{}", "w1"), resp("ok")]);
    let reg = registry_with(vec![fake("Write", false, "wrote")]);
    let (mut agent, _s, hooks) =
        build_agent(llm, reg, FakePerms::new(PermissionDecision::Ask), base_cfg());
    agent.prompter = Some(Arc::new(YesPrompter));
    let r = agent.run_turn("go").await;
    assert!(r.permission_denials.is_empty());
    let calls = hooks.calls.lock().unwrap();
    let n = calls.iter().find(|c| c.0 == HookEvent::Notification).expect("Notification fired");
    assert_eq!(n.1, "permission_prompt");
    assert_eq!(n.2["notification_type"], "permission_prompt");
    assert!(n.2["message"].as_str().unwrap().contains("Write"));
}

/// Prompter that always answers "always allow" with two rules (one per line).
struct AlwaysTwo;

impl Prompter for AlwaysTwo {
    fn ask_permission(&self, _tool: &str, _input: &Value) -> PromptAnswer {
        PromptAnswer::Always("Bash(git add *)\nBash(git commit *)".into())
    }
}

/// C2.12: an "always allow" answer carrying several rules persists each one separately.
#[tokio::test]
async fn c2_12_always_allow_persists_one_rule_per_line() {
    let llm = MockLlm::new(vec![resp_tool("Bash", "{}", "b"), resp("ok")]);
    let reg = registry_with(vec![fake("Bash", false, "ran")]);
    let perms = FakePerms::new(PermissionDecision::Ask);
    let mut agent = crate::Agent::new(
        llm,
        reg,
        perms.clone(),
        Some(Arc::new(AlwaysTwo)),
        FakeHooks::new(),
        Arc::new(CollectSink::default()),
        temp_ctx(),
        base_cfg(),
    );
    let r = agent.run_turn("go").await;
    assert!(r.permission_denials.is_empty());
    assert_eq!(
        *perms.always.lock().unwrap(),
        vec!["Bash(git add *)".to_string(), "Bash(git commit *)".to_string()]
    );
}

// ───────── r7 item 2: tool-call repair before rejection ─────────

/// A tool with a real schema that records the arguments it was called with.
struct RecordingTool {
    calls: std::sync::Mutex<Vec<Value>>,
}

#[async_trait::async_trait]
impl Tool for RecordingTool {
    fn name(&self) -> &str {
        "Edit"
    }
    fn description(&self) -> &str {
        "records args"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {"type": "string"},
                "old_string": {"type": "string"},
                "new_string": {"type": "string"},
                "replace_all": {"type": "boolean"}
            },
            "required": ["file_path", "old_string", "new_string"],
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        false
    }
    async fn call(&self, args: Value, _ctx: &ToolContext) -> ToolOutput {
        self.calls.lock().unwrap().push(args);
        ToolOutput::ok("edited")
    }
}

#[tokio::test]
async fn r7_fenced_args_and_lowercase_name_are_repaired_and_run() {
    let raw = "```json{\"file_path\":\"a.rs\",\"old_string\":\"x\",\"new_string\":\"y\",\"replace_all\":\"true\",}```";
    let llm = MockLlm::new(vec![resp_tool("str_replace", raw, "e1"), resp("done")]);
    let tool = Arc::new(RecordingTool { calls: Default::default() });
    let reg = registry_with(vec![tool.clone() as Arc<dyn Tool>]);
    let (mut agent, _s, hooks) = build_agent(llm.clone(), reg, allow(), base_cfg());
    let r = agent.run_turn("go").await;
    assert_eq!(r.result, "done");
    assert_eq!(
        tool.calls.lock().unwrap().clone(),
        vec![json!({"file_path":"a.rs","old_string":"x","new_string":"y","replace_all":true})]
    );
    // The model is told what was repaired, and the next request carries the repaired call.
    let second = &llm.requests()[1];
    let tool_msg = second.messages.iter().find(|m| m.role == "tool").unwrap();
    let content = tool_msg.content.as_deref().unwrap();
    assert!(content.starts_with("edited"), "{}", content);
    assert!(content.contains("mapped to 'Edit'") && content.contains("code fence"), "{}", content);
    let call = &second.messages.iter().find_map(|m| m.tool_calls.clone()).unwrap()[0];
    assert_eq!(call.function.name, "Edit");
    assert_eq!(serde_json::from_str::<Value>(&call.function.arguments).unwrap()["replace_all"], json!(true));
    // Hooks see the repaired name too.
    assert!(hooks.calls.lock().unwrap().iter().any(|(ev, target, _)| *ev == HookEvent::PreToolUse && target == "Edit"));
}

#[tokio::test]
async fn c4_leaked_template_tokens_in_tool_name_are_cut_and_not_echoed() {
    // gpt-oss via vLLM: `Edit<​|channel|>commentary` (zero-width space after `<`).
    let llm = MockLlm::new(vec![
        resp_tool("Edit<\u{200B}|channel|>commentary", r#"{"file_path":"a","old_string":"x","new_string":"y"}"#, "e1"),
        resp("done"),
    ]);
    let tool = Arc::new(RecordingTool { calls: Default::default() });
    let reg = registry_with(vec![tool.clone() as Arc<dyn Tool>]);
    let (mut agent, _s, _h) = build_agent(llm.clone(), reg, allow(), base_cfg());
    let r = agent.run_turn("go").await;
    assert_eq!(r.result, "done");
    assert_eq!(tool.calls.lock().unwrap().len(), 1, "the call runs as Edit");
    let second = &llm.requests()[1];
    let call = &second.messages.iter().find_map(|m| m.tool_calls.clone()).unwrap()[0];
    assert_eq!(call.function.name, "Edit", "history carries the clean name");
    let body = serde_json::to_string(&second.messages).unwrap();
    assert!(!body.contains("|channel|"), "no leaked tokens anywhere in the next request: {body}");
}

#[tokio::test]
async fn c5_empty_reply_is_requeried_then_accepted_after_two() {
    // Empty reply → dropped, nudge, real answer.
    let llm = MockLlm::new(vec![resp(""), resp("done")]);
    let (mut agent, _s, _h) = build_agent(llm.clone(), registry_with(vec![]), allow(), base_cfg());
    let r = agent.run_turn("go").await;
    assert_eq!(r.result, "done");
    let second = &llm.requests()[1];
    let last = second.messages.last().unwrap();
    assert_eq!(last.role, "user");
    assert!(last.content.as_deref().unwrap().contains("reply was empty"));
    assert!(
        !second.messages.iter().any(|m| m.role == "assistant" && m.content.as_deref().unwrap_or("").is_empty() && m.tool_calls.is_none()),
        "the empty reply is not kept in history"
    );
    // Three empty replies in a row: two requeries, then the run ends (no endless loop).
    let llm = MockLlm::new(vec![resp(""), resp(" "), resp(""), resp("never")]);
    let (mut agent, _s, _h) = build_agent(llm.clone(), registry_with(vec![]), allow(), base_cfg());
    let r = agent.run_turn("go").await;
    assert_eq!(r.result.trim(), "");
    assert_eq!(llm.requests().len(), 3);
}

#[tokio::test]
async fn c7_unparsed_sample_error_is_requeried_but_other_errors_end_the_run() {
    let llm = MockLlm::with_errors(vec![
        Err(anyhow::anyhow!("model returned no choices (body: {{\"error\":{{\"message\":\"unexpected tokens remaining in message header\"}}}})")),
        Ok(resp("done")),
    ]);
    let (mut agent, _s, _h) = build_agent(llm.clone(), registry_with(vec![]), allow(), base_cfg());
    let r = agent.run_turn("go").await;
    assert_eq!(r.result, "done");
    let last = llm.requests()[1].messages.last().unwrap().clone();
    assert!(last.content.unwrap().contains("could not parse your last reply"));
    // A real failure (quota, auth) still ends the run at once.
    let llm = MockLlm::with_errors(vec![Err(anyhow::anyhow!("HTTP 401: bad key")), Ok(resp("never"))]);
    let (mut agent, _s, _h) = build_agent(llm.clone(), registry_with(vec![]), allow(), base_cfg());
    let r = agent.run_turn("go").await;
    assert!(r.is_error && r.result.contains("401"), "{}", r.result);
    assert_eq!(llm.requests().len(), 1);
}

#[tokio::test]
async fn r7_well_formed_call_gets_no_repair_note_and_bad_call_lists_fields() {
    let llm = MockLlm::new(vec![
        resp_tool("Edit", r#"{"file_path":"a","old_string":"x","new_string":"y"}"#, "ok1"),
        resp_tool("Edit", "{file_path: a", "bad1"),
        resp("done"),
    ]);
    let tool = Arc::new(RecordingTool { calls: Default::default() });
    let reg = registry_with(vec![tool.clone() as Arc<dyn Tool>]);
    let (mut agent, _s, _h) = build_agent(llm, reg, allow(), base_cfg());
    agent.run_turn("go").await;
    let tools: Vec<String> =
        agent.messages().iter().filter(|m| m.role == "tool").filter_map(|m| m.content.clone()).collect();
    assert_eq!(tools[0], "edited");
    assert!(tools[1].contains("Invalid JSON arguments") && tools[1].contains("file_path (string, required)"), "{}", tools[1]);
    assert_eq!(tool.calls.lock().unwrap().len(), 1, "the unrepairable call must not run");
}

// ───────── r7 item 5: repeat-edit guard + todo re-injection ─────────

fn tool_contents(msgs: &[ChatMessage]) -> Vec<String> {
    msgs.iter().filter(|m| m.role == "tool").filter_map(|m| m.content.clone()).collect()
}

#[tokio::test]
async fn r7_fifth_edit_turn_gets_exactly_one_guard_note() {
    let mut script: Vec<ChatResponse> =
        (0..5).map(|i| resp_tool("Edit", r#"{"file_path":"a.rs"}"#, &format!("e{}", i))).collect();
    script.push(resp("done"));
    let llm = MockLlm::new(script);
    let reg = registry_with(vec![fake("Edit", false, "edited")]);
    let (mut agent, _s, _h) = build_agent(llm, reg, allow(), base_cfg());
    agent.run_turn("go").await;
    let tools = tool_contents(agent.messages());
    let noted: Vec<usize> =
        tools.iter().enumerate().filter(|(_, c)| c.contains("changed a.rs 4 times")).map(|(i, _)| i).collect();
    assert_eq!(noted, vec![3], "{:#?}", tools);

    // A new turn starts with fresh counters: three more edits stay quiet.
    let mut script: Vec<ChatResponse> =
        (0..3).map(|i| resp_tool("Edit", r#"{"file_path":"a.rs"}"#, &format!("f{}", i))).collect();
    script.push(resp("done"));
    agent.llm = MockLlm::new(script);
    agent.run_turn("again").await;
    assert_eq!(tool_contents(agent.messages()).iter().filter(|c| c.contains("[zen:")).count(), 1);
}

#[tokio::test]
async fn r7_three_identical_failures_nudge() {
    let failing: Arc<dyn Tool> =
        Arc::new(FakeTool { name: "Bash", read_only: false, output: ToolOutput::err("exit 1") });
    let mut script: Vec<ChatResponse> =
        (0..3).map(|i| resp_tool("Bash", r#"{"command":"make"}"#, &format!("b{}", i))).collect();
    script.push(resp("done"));
    let (mut agent, _s, _h) = build_agent(MockLlm::new(script), registry_with(vec![failing]), allow(), base_cfg());
    agent.run_turn("go").await;
    let tools = tool_contents(agent.messages());
    assert!(!tools[1].contains("[zen:") && tools[2].contains("failed 3 times"), "{:#?}", tools);
}

#[tokio::test]
async fn r7_todos_reinjected_every_sixth_call_and_after_compaction() {
    // 13 model calls: 12 tool calls then a final answer.
    let mut script: Vec<ChatResponse> =
        (0..12).map(|i| resp_tool("Echo", "{}", &format!("c{}", i))).collect();
    script.push(resp("done"));
    script.push(resp("SUMMARY TEXT")); // for the manual compaction below
    let llm = MockLlm::new(script);
    let reg = registry_with(vec![fake("Echo", true, "ok")]);
    let (mut agent, _s, _h) = build_agent(llm.clone(), reg, allow(), base_cfg());
    *agent.ctx.todos.lock().unwrap() =
        vec![TodoItem { content: "fix the parser".into(), status: "in_progress".into(), active_form: None }];
    agent.run_turn("go").await;

    // Which requests (1-based model calls) showed the todo list in their newest message?
    let reqs = llm.requests();
    let with_todo: Vec<usize> = reqs
        .iter()
        .enumerate()
        .filter(|(_, r)| r.messages.last().and_then(|m| m.content.as_deref()).is_some_and(|c| c.contains("[~] fix the parser")))
        .map(|(i, _)| i + 1)
        .collect();
    assert_eq!(with_todo, vec![6, 12]);

    agent.compact(None, "manual").await.unwrap();
    let summary = agent.messages()[0].content.clone().unwrap();
    assert!(summary.contains("SUMMARY TEXT") && summary.contains("[~] fix the parser"), "{}", summary);
}

#[tokio::test]
async fn r7_short_turn_and_empty_todo_list_get_no_notes() {
    let mut script: Vec<ChatResponse> =
        (0..7).map(|i| resp_tool("Edit", &format!(r#"{{"file_path":"f{}.rs"}}"#, i), &format!("c{}", i))).collect();
    script.push(resp("done"));
    let reg = registry_with(vec![fake("Edit", false, "edited")]);
    let (mut agent, _s, _h) = build_agent(MockLlm::new(script), reg, allow(), base_cfg());
    agent.run_turn("go").await;
    assert!(agent.messages().iter().all(|m| !m.content.as_deref().unwrap_or("").contains("[zen:")));
}

// ───────── r7 item 3: check-before-finish ─────────

#[tokio::test]
async fn r7_edit_then_answer_gets_one_reminder_then_bash_then_success() {
    let llm = MockLlm::new(vec![
        resp_tool("Edit", r#"{"file_path":"src/a.rs"}"#, "e1"),
        resp("looks fine"),
        resp_tool("Bash", r#"{"command":"cargo test"}"#, "b1"),
        resp("tests pass"),
    ]);
    let reg = registry_with(vec![fake("Edit", false, "edited"), fake("Bash", false, "ok")]);
    let cfg = AgentConfig { verify_reminder: true, ..base_cfg() };
    let (mut agent, _s, _h) = build_agent(llm.clone(), reg, allow(), cfg);
    let r = agent.run_turn("fix it").await;
    assert_eq!(r.result, "tests pass");
    assert_eq!(r.num_turns, 4);
    let reminders: Vec<&ChatMessage> = agent
        .messages()
        .iter()
        .filter(|m| m.role == "user" && m.content.as_deref().unwrap_or("").contains("have not run any command"))
        .collect();
    assert_eq!(reminders.len(), 1);
    assert!(reminders[0].content.as_deref().unwrap().contains("src/a.rs"));
}

#[tokio::test]
async fn r7_no_reminder_without_edits_or_when_disabled() {
    for (cfg, script) in [
        (AgentConfig { verify_reminder: true, ..base_cfg() }, vec![resp_tool("Read", "{}", "r1"), resp("answer")]),
        (base_cfg(), vec![resp_tool("Edit", r#"{"file_path":"a"}"#, "e1"), resp("answer")]),
    ] {
        let reg = registry_with(vec![fake("Read", true, "text"), fake("Edit", false, "edited")]);
        let (mut agent, _s, _h) = build_agent(MockLlm::new(script), reg, allow(), cfg);
        let r = agent.run_turn("q").await;
        assert_eq!((r.result.as_str(), r.num_turns), ("answer", 2));
    }
}

#[tokio::test]
async fn r7_failing_verify_command_stops_after_two_rounds() {
    let mut script = Vec::new();
    for i in 0..3 {
        script.push(resp_tool("Edit", r#"{"file_path":"a.rs"}"#, &format!("e{}", i)));
        script.push(resp(&format!("attempt {}", i)));
    }
    let llm = MockLlm::new(script);
    let reg = registry_with(vec![fake("Edit", false, "edited")]);
    let cfg = AgentConfig {
        verify_reminder: true,
        verify_command: Some("echo FAILING-CHECK; exit 1".into()),
        ..base_cfg()
    };
    let (mut agent, sink, _h) = build_agent(llm.clone(), reg, allow(), cfg);
    let r = agent.run_turn("fix").await;
    assert!(!r.is_error);
    assert_eq!(r.result, "attempt 2", "the turn ends after two automatic rounds");
    assert_eq!(llm.requests().len(), 6);
    let feedback = agent
        .messages()
        .iter()
        .filter(|m| m.role == "user" && m.content.as_deref().unwrap_or("").contains("FAILING-CHECK"))
        .count();
    assert_eq!(feedback, 2);
    assert!(sink.events.lock().unwrap().iter().any(|e| matches!(e,
        AgentEvent::SystemMessage { event, .. } if event == "Verify")));
}

#[tokio::test]
async fn r7_tool_call_written_as_text_runs_as_a_call() {
    let llm = MockLlm::new(vec![
        resp(r#"{"name": "Edit", "parameters": {"file_path": "a.rs", "old_string": "x", "new_string": "y"}}"#),
        resp("done"),
    ]);
    let tool = Arc::new(RecordingTool { calls: Default::default() });
    let reg = registry_with(vec![tool.clone() as Arc<dyn Tool>]);
    let (mut agent, _s, _h) = build_agent(llm.clone(), reg, allow(), base_cfg());
    let r = agent.run_turn("go").await;
    assert_eq!(r.result, "done");
    assert_eq!(tool.calls.lock().unwrap().len(), 1);
    let second = &llm.requests()[1];
    let tool_msg = second.messages.iter().find(|m| m.role == "tool").unwrap();
    assert!(tool_msg.content.as_deref().unwrap().contains("written as message text"));
    assert_paired(agent.messages());
}

/// A tool that sleeps before returning, for heartbeat tests.
struct SlowTool {
    delay_ms: u64,
}

#[async_trait::async_trait]
impl Tool for SlowTool {
    fn name(&self) -> &str {
        "Slow"
    }
    fn description(&self) -> &str {
        "slow fake tool"
    }
    fn parameters_schema(&self) -> Value {
        json!({ "type": "object" })
    }
    fn is_read_only(&self) -> bool {
        true
    }
    async fn call(&self, _args: Value, _ctx: &ToolContext) -> ToolOutput {
        tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
        ToolOutput::ok("slow done")
    }
}

/// Issue #21: a tool running longer than the heartbeat interval emits
/// `ToolProgress` events with increasing elapsed times.
#[tokio::test]
async fn tool_progress_heartbeat_emitted_for_slow_tool() {
    use crate::agent::run_tool_with_heartbeat;
    let sink = Arc::new(CollectSink::default());
    let tool: Arc<dyn Tool> = Arc::new(SlowTool { delay_ms: 250 });
    let ctx = temp_ctx();
    let out = run_tool_with_heartbeat(
        tool,
        json!({}),
        ctx,
        (None, crate::agent::TimeoutSource::None),
        sink.clone(),
        "call_slow_1".into(),
        "Slow".into(),
        std::time::Duration::from_millis(50),
    )
    .await;
    assert_eq!(out.content, "slow done");
    let events = sink.events.lock().unwrap();
    let progress: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolProgress { tool_use_id, tool_name, elapsed_ms } => {
                Some((tool_use_id.clone(), tool_name.clone(), *elapsed_ms))
            }
            _ => None,
        })
        .collect();
    assert!(!progress.is_empty(), "expected at least one heartbeat");
    for (id, name, _) in &progress {
        assert_eq!(id, "call_slow_1");
        assert_eq!(name, "Slow");
    }
    // elapsed times increase across heartbeats
    let elapsed: Vec<u64> = progress.iter().map(|(_, _, ms)| *ms).collect();
    assert!(elapsed.windows(2).all(|w| w[1] > w[0]), "elapsed not increasing: {:?}", elapsed);
}

/// Issue #21: a fast tool emits no heartbeat.
#[tokio::test]
async fn tool_progress_no_heartbeat_for_fast_tool() {
    use crate::agent::run_tool_with_heartbeat;
    let sink = Arc::new(CollectSink::default());
    let tool: Arc<dyn Tool> = Arc::new(SlowTool { delay_ms: 10 });
    let ctx = temp_ctx();
    let out = run_tool_with_heartbeat(
        tool,
        json!({}),
        ctx,
        (None, crate::agent::TimeoutSource::None),
        sink.clone(),
        "call_fast_1".into(),
        "Slow".into(),
        std::time::Duration::from_secs(60),
    )
    .await;
    assert_eq!(out.content, "slow done");
    let events = sink.events.lock().unwrap();
    assert!(
        !events.iter().any(|e| matches!(e, AgentEvent::ToolProgress { .. })),
        "fast tool should emit no heartbeat"
    );
}

#[tokio::test]
async fn budget_cap_blocks_before_first_call_when_tiny() {
    use crate::budget::BudgetPolicy;
    let llm = MockLlm::new(vec![resp("hello")]);
    let mut cfg = base_cfg();
    cfg.model = "gpt-4o".into();
    cfg.budget = Some(BudgetPolicy { max_budget_usd: 0.0001, warn_at: 0.8 });
    let (mut agent, _sink, _h) = build_agent(llm.clone(), ToolRegistry::new(), allow(), cfg);
    let r = agent.run_turn("hi").await;
    assert_eq!(r.subtype, "error_budget_exceeded");
    assert!(r.is_error);
    assert!(r.result.contains("Budget exceeded"), "got: {}", r.result);
    assert!(r.result.contains("$0.00"), "got: {}", r.result);
    assert!(
        llm.requests().is_empty(),
        "no model call should have been made"
    );
}

#[tokio::test]
async fn budget_warns_once_at_threshold_then_stays_quiet() {
    use crate::budget::BudgetPolicy;
    let llm = MockLlm::new(vec![resp("hello"), resp("world")]);
    let mut cfg = base_cfg();
    cfg.model = "gpt-4o".into();
    cfg.budget = Some(BudgetPolicy { max_budget_usd: 1.0, warn_at: 0.5 });
    let (mut agent, sink, _h) = build_agent(llm.clone(), ToolRegistry::new(), allow(), cfg);
    // 200k input tokens of gpt-4o @ $2.50/1M = $0.50 = warn threshold.
    agent.usage = UsageStats { input_tokens: 200_000, output_tokens: 0, ..Default::default() };
    let r = agent.run_turn("hi").await;
    assert_eq!(r.subtype, "success");
    assert!(agent.budget_warned, "warn-once flag must be set");
    let warn_count = sink
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| matches!(e, AgentEvent::SystemMessage { event, .. } if event == "budget"))
        .count();
    assert_eq!(warn_count, 1, "exactly one budget warning");
    // Second turn: already warned → no repeat warning, call proceeds.
    let r2 = agent.run_turn("again").await;
    assert_eq!(r2.subtype, "success");
    let warn_count2 = sink
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| matches!(e, AgentEvent::SystemMessage { event, .. } if event == "budget"))
        .count();
    assert_eq!(warn_count2, 1, "warning must not repeat");
}

#[tokio::test]
async fn budget_is_noop_for_free_models() {
    use crate::budget::BudgetPolicy;
    let llm = MockLlm::new(vec![resp("hello")]);
    let mut cfg = base_cfg();
    cfg.model = "qwen/qwen3-coder:free".into();
    cfg.budget = Some(BudgetPolicy { max_budget_usd: 0.000_001, warn_at: 0.8 });
    let (mut agent, _sink, _h) = build_agent(llm.clone(), ToolRegistry::new(), allow(), cfg);
    let r = agent.run_turn("hi").await;
    assert_eq!(r.subtype, "success", "free models never trip the cap");
    assert_eq!(llm.requests().len(), 1);
    assert!(!agent.budget_warned);
}

#[tokio::test]
async fn budget_allows_unknown_pricing() {
    use crate::budget::BudgetPolicy;
    let llm = MockLlm::new(vec![resp("hello")]);
    let mut cfg = base_cfg();
    cfg.model = "mystery-model-9999".into(); // not in the price table
    cfg.budget = Some(BudgetPolicy { max_budget_usd: 0.000_001, warn_at: 0.8 });
    let (mut agent, _sink, _h) = build_agent(llm.clone(), ToolRegistry::new(), allow(), cfg);
    let r = agent.run_turn("hi").await;
    // Unknown pricing cannot be enforced: the call proceeds (costs never guessed).
    assert_eq!(r.subtype, "success");
    assert_eq!(llm.requests().len(), 1);
}

#[tokio::test]
async fn no_budget_config_means_no_checks() {
    let llm = MockLlm::new(vec![resp("hello")]);
    let mut cfg = base_cfg();
    cfg.model = "gpt-4o".into();
    cfg.budget = None;
    let (mut agent, _sink, _h) = build_agent(llm.clone(), ToolRegistry::new(), allow(), cfg);
    let r = agent.run_turn("hi").await;
    assert_eq!(r.subtype, "success");
    assert!(!agent.budget_warned);
}

#[tokio::test]
async fn plan_mode_rejects_hallucinated_mutating_call_by_the_harness() {
    // The model never sees Write in plan mode (tools are withheld), but if it hallucinates
    // the call anyway the *harness* must reject it: the tool never executes, no permission
    // denial is recorded, and the turn stays a clean plan.
    let llm = MockLlm::new(vec![resp_tool("Write", r#"{"path":"x"}"#, "w1"), resp("plan done")]);
    let reg = registry_with(vec![fake("Read", true, "r"), fake("Write", false, "MUTATED")]);
    let cfg = AgentConfig { permission_mode: PermissionMode::Plan, ..base_cfg() };
    let (mut agent, _s, _h) = build_agent(llm, reg, allow(), cfg);
    let r = agent.run_turn("go").await;
    let tool_contents: Vec<&str> = agent
        .messages()
        .iter()
        .filter(|m| m.role == "tool")
        .filter_map(|m| m.content.as_deref())
        .collect();
    assert_eq!(tool_contents.len(), 1, "one tool result expected");
    assert!(
        tool_contents[0].contains("not available in plan mode"),
        "harness must reject with a clear error, got: {}",
        tool_contents[0]
    );
    assert!(
        !agent
            .messages()
            .iter()
            .any(|m| m.content.as_deref().unwrap_or("").contains("MUTATED")),
        "the mutating tool must never execute in plan mode"
    );
    assert!(
        r.permission_denials.is_empty(),
        "plan-mode rejection is not a permission denial"
    );
    assert_eq!(r.result, "plan done");
    assert_paired(agent.messages());
}
