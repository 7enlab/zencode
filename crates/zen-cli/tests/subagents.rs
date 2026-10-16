//! End-to-end tests for sub-agents / Task tool (SPEC §9).
//!
//! Tests verify that the `--agents` CLI flag defines sub-agents, the model can call
//! the `Task` tool to delegate work, and the sub-agent runs with its own context and
//! returns its final text to the parent.

mod common;

use common::*;
use std::time::Duration;

/// Inline --agents JSON defines a sub-agent; model calls Task tool; sub-agent runs with
/// its own system prompt and returns result to parent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sub_agent_task_tool_works() {
    let home = TempDir::new("subagent_home");
    let cwd = TempDir::new("subagent_cwd");

    // Define a custom sub-agent via --agents
    let agents_json = r#"{
        "researcher": {
            "description": "Research specialist",
            "prompt": "You are a researcher. Answer concisely with facts only.",
            "tools": ["Read", "Glob", "Grep"]
        }
    }"#;

    let server = MockServer::start(vec![
        // Parent model calls Task tool
        Reply::with_tool_calls(
            "",
            Some(serde_json::json!([{
                "id": "call_task_1",
                "type": "function",
                "function": {
                    "name": "Task",
                    "arguments": "{\"description\":\"Research topic\",\"prompt\":\"Find info about X\",\"subagent_type\":\"researcher\"}"
                }
            }])),
        ),
        // Sub-agent response (simulated by mock - the sub-agent makes its own request)
        // The mock will receive a second request from the sub-agent
        Reply::text("research result: X is a thing"),
        // Parent continues after receiving sub-agent result
        Reply::text("parent got sub-agent result"),
    ]).await;

    let (stdout, stderr, code) = tokio::time::timeout(Duration::from_secs(90), async {
        ZenRun::new("subagent", &home, &cwd)
            .args([
                "-p", "--output-format", "json", "--bare", "--setting-sources", "",
                "--permission-mode", "bypassPermissions",
                "--agents", agents_json
            ])
            .mock("mock-model", &server)
            .arg("delegate to researcher")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let v = last_result(&stdout);
    assert_eq!(v["result"], "parent got sub-agent result");

    // Should have 3 requests: parent calls Task, sub-agent runs, parent receives result
    let requests = server.requests().await;
    assert_eq!(requests.len(), 3, "expected 3 requests, got {}: {:?}", requests.len(), requests);

    // First request: parent calls Task
    let first_tools = request_tool_names(&requests[0]);
    assert!(first_tools.contains(&"Task".to_string()), "parent should have Task tool: {:?}", first_tools);

    // Second request: sub-agent (should have the researcher's system prompt)
    let second_sys = system_message(&requests[1]);
    assert!(second_sys.contains("You are a researcher"), "sub-agent should have custom prompt: {}", second_sys);
    assert!(second_sys.contains("Answer concisely"), "sub-agent prompt detail missing: {}", second_sys);

    // Third request: parent receives sub-agent result
    let third_msgs = request_messages_text(&requests[2]);
    assert!(third_msgs.contains("research result"), "parent should see sub-agent result: {}", third_msgs);
}

/// Sub-agent inherits parent's tools when not explicitly specified.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sub_agent_inherits_tools_when_not_specified() {
    let home = TempDir::new("subagent_inherit_home");
    let cwd = TempDir::new("subagent_inherit_cwd");

    let agents_json = r#"{
        "general": {
            "description": "General purpose",
            "prompt": "You are a helper.",
            "tools": null
        }
    }"#;

    let server = MockServer::start(vec![
        Reply::with_tool_calls(
            "",
            Some(serde_json::json!([{
                "id": "call_task_1",
                "type": "function",
                "function": {
                    "name": "Task",
                    "arguments": "{\"description\":\"Task\",\"prompt\":\"Do something\",\"subagent_type\":\"general\"}"
                }
            }])),
        ),
        // Sub-agent request - should have all parent tools (Read, Write, etc.)
        Reply::with_tool_calls(
            "",
            Some(serde_json::json!([{
                "id": "call_1",
                "type": "function",
                "function": {"name": "Read", "arguments": "{\"file_path\":\"test.txt\"}"}
            }])),
        ),
        Reply::text("sub-agent done"),
        Reply::text("parent done"),
    ]).await;

    cwd.write("test.txt", "content");

    let (_stdout, stderr, code) = tokio::time::timeout(Duration::from_secs(90), async {
        ZenRun::new("subagent_inherit", &home, &cwd)
            .args([
                "-p", "--output-format", "json", "--bare", "--setting-sources", "",
                "--permission-mode", "bypassPermissions",
                "--agents", agents_json
            ])
            .mock("mock-model", &server)
            .arg("delegate")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let requests = server.requests().await;
    assert_eq!(requests.len(), 4);

    // Sub-agent request (2nd) should have Read tool available
    let second_tools = request_tool_names(&requests[1]);
    assert!(second_tools.contains(&"Read".to_string()), "sub-agent should inherit Read: {:?}", second_tools);
}

/// Built-in general-purpose agent is always available unless overridden.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn builtin_general_purpose_agent_available() {
    let home = TempDir::new("subagent_builtin_home");
    let cwd = TempDir::new("subagent_builtin_cwd");

    // No --agents flag; built-in general-purpose should exist
    let server = MockServer::start(vec![
        Reply::with_tool_calls(
            "",
            Some(serde_json::json!([{
                "id": "call_task_1",
                "type": "function",
                "function": {
                    "name": "Task",
                    "arguments": "{\"description\":\"Task\",\"prompt\":\"Do something\",\"subagent_type\":\"general-purpose\"}"
                }
            }])),
        ),
        Reply::text("general-purpose done"),
        Reply::text("parent done"),
    ]).await;

    let (stdout, stderr, code) = tokio::time::timeout(Duration::from_secs(90), async {
        ZenRun::new("subagent_builtin", &home, &cwd)
            .args([
                // No --bare: built-in agents are part of normal discovery (--bare without
                // --agents skips sub-agents entirely). Temp home/cwd hold no agent files.
                "-p", "--output-format", "json", "--setting-sources", "",
                "--permission-mode", "bypassPermissions"
            ])
            .mock("mock-model", &server)
            .arg("delegate to general-purpose")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let v = last_result(&stdout);
    assert_eq!(v["result"], "parent done");

    let requests = server.requests().await;
    assert_eq!(requests.len(), 3);

    // Sub-agent should have the built-in system prompt
    let second_sys = system_message(&requests[1]);
    assert!(second_sys.contains("sub-agent") || second_sys.contains("Sub-agent"), "built-in prompt: {}", second_sys);
}

/// Sub-agent depth limit prevents infinite recursion.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sub_agent_depth_limit() {
    let home = TempDir::new("subagent_depth_home");
    let cwd = TempDir::new("subagent_depth_cwd");

    let agents_json = r#"{
        "recursive": {
            "description": "Recursive agent",
            "prompt": "You are a recursive agent.",
            "tools": ["Task"]
        }
    }"#;

    let server = MockServer::start(vec![
        // Parent calls Task -> recursive agent
        Reply::with_tool_calls(
            "",
            Some(serde_json::json!([{
                "id": "call_task_1",
                "type": "function",
                "function": {
                    "name": "Task",
                    "arguments": "{\"description\":\"Recurse\",\"prompt\":\"Recurse\",\"subagent_type\":\"recursive\"}"
                }
            }])),
        ),
        // Recursive agent calls Task again (depth 2) - should fail due to depth limit
        Reply::with_tool_calls(
            "",
            Some(serde_json::json!([{
                "id": "call_task_2",
                "type": "function",
                "function": {
                    "name": "Task",
                    "arguments": "{\"description\":\"Recurse again\",\"prompt\":\"Recurse again\",\"subagent_type\":\"recursive\"}"
                }
            }])),
        ),
        // Sub-agent at depth 2 gets error, returns to parent
        Reply::text("depth limit reached"),
        Reply::text("parent handled error"),
    ]).await;

    let (stdout, stderr, code) = tokio::time::timeout(Duration::from_secs(90), async {
        ZenRun::new("subagent_depth", &home, &cwd)
            .args([
                "-p", "--output-format", "json", "--bare", "--setting-sources", "",
                "--permission-mode", "bypassPermissions",
                "--agents", agents_json
            ])
            .mock("mock-model", &server)
            .arg("recurse")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let v = last_result(&stdout);
    assert_eq!(v["result"], "parent handled error");
    // parent → sub-agent (depth 1) → nested Task refused → sub-agent answers → parent.
    // Sub-agents are not given the Task tool (max depth 1), so even an agent whose definition
    // lists Task cannot recurse: the nested call fails as an unknown tool.
    let requests = server.requests().await;
    assert_eq!(requests.len(), 4);
    let sub_tools = request_tool_names(&requests[1]);
    assert!(!sub_tools.contains(&"Task".to_string()), "sub-agent must not get Task: {:?}", sub_tools);
    let third = request_messages_text(&requests[2]);
    assert!(third.contains("Unknown tool 'Task'"), "nested Task should be refused: {}", third);
}

/// Sub-agent can use a different model if specified.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sub_agent_custom_model() {
    let home = TempDir::new("subagent_model_home");
    let cwd = TempDir::new("subagent_model_cwd");

    let agents_json = r#"{
        "specialist": {
            "description": "Specialist",
            "prompt": "You are a specialist.",
            "model": "specialist-model"
        }
    }"#;

    let server = MockServer::start(vec![
        Reply::with_tool_calls(
            "",
            Some(serde_json::json!([{
                "id": "call_task_1",
                "type": "function",
                "function": {
                    "name": "Task",
                    "arguments": "{\"description\":\"Specialist task\",\"prompt\":\"Do it\",\"subagent_type\":\"specialist\"}"
                }
            }])),
        ),
        // Sub-agent request should use specialist-model
        Reply::text("specialist done"),
        Reply::text("parent done"),
    ]).await;

    let (_stdout, stderr, code) = tokio::time::timeout(Duration::from_secs(90), async {
        ZenRun::new("subagent_model", &home, &cwd)
            .args([
                "-p", "--output-format", "json", "--bare", "--setting-sources", "",
                "--permission-mode", "bypassPermissions",
                "--agents", agents_json
            ])
            .mock("mock-model", &server)
            .arg("use specialist")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let requests = server.requests().await;
    assert_eq!(requests.len(), 3);

    // Second request is from sub-agent - should have model = specialist-model
    let second_req = request_json(&requests[1]);
    let second_model = second_req["model"].as_str().unwrap();
    assert_eq!(second_model, "specialist-model", "sub-agent should use its own model");
}