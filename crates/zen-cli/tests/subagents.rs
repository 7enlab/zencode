mod common;

use common::*;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sub_agent_task_tool_works() {
    let home = TempDir::new("subagent_home");
    let cwd = TempDir::new("subagent_cwd");

    let agents_json = r#"{
        "researcher": {
            "description": "Research specialist",
            "prompt": "You are a researcher. Answer concisely with facts only.",
            "tools": ["Read", "Glob", "Grep"]
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
                    "arguments": "{\"description\":\"Research topic\",\"prompt\":\"Find info about X\",\"subagent_type\":\"researcher\"}"
                }
            }])),
        ),

        Reply::text("research result: X is a thing"),

        Reply::text("parent got sub-agent result"),
    ]).await;

    let (stdout, stderr, code) = tokio::time::timeout(Duration::from_secs(90), async {
        ZenRun::new("subagent", &home, &cwd)
            .args([
                "-p",
                "--output-format",
                "json",
                "--bare",
                "--setting-sources",
                "",
                "--permission-mode",
                "bypassPermissions",
                "--agents",
                agents_json,
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

    let requests = server.requests().await;
    assert_eq!(
        requests.len(),
        3,
        "expected 3 requests, got {}: {:?}",
        requests.len(),
        requests
    );

    let first_tools = request_tool_names(&requests[0]);
    assert!(
        first_tools.contains(&"Task".to_string()),
        "parent should have Task tool: {:?}",
        first_tools
    );

    let second_sys = system_message(&requests[1]);
    assert!(
        second_sys.contains("You are a researcher"),
        "sub-agent should have custom prompt: {}",
        second_sys
    );
    assert!(
        second_sys.contains("Answer concisely"),
        "sub-agent prompt detail missing: {}",
        second_sys
    );

    let third_msgs = request_messages_text(&requests[2]);
    assert!(
        third_msgs.contains("research result"),
        "parent should see sub-agent result: {}",
        third_msgs
    );
}

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
                "-p",
                "--output-format",
                "json",
                "--bare",
                "--setting-sources",
                "",
                "--permission-mode",
                "bypassPermissions",
                "--agents",
                agents_json,
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

    let second_tools = request_tool_names(&requests[1]);
    assert!(
        second_tools.contains(&"Read".to_string()),
        "sub-agent should inherit Read: {:?}",
        second_tools
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn builtin_general_purpose_agent_available() {
    let home = TempDir::new("subagent_builtin_home");
    let cwd = TempDir::new("subagent_builtin_cwd");

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
                "-p",
                "--output-format",
                "json",
                "--setting-sources",
                "",
                "--permission-mode",
                "bypassPermissions",
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

    let second_sys = system_message(&requests[1]);
    assert!(
        second_sys.contains("sub-agent") || second_sys.contains("Sub-agent"),
        "built-in prompt: {}",
        second_sys
    );
}

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

        Reply::text("depth limit reached"),
        Reply::text("parent handled error"),
    ]).await;

    let (stdout, stderr, code) = tokio::time::timeout(Duration::from_secs(90), async {
        ZenRun::new("subagent_depth", &home, &cwd)
            .args([
                "-p",
                "--output-format",
                "json",
                "--bare",
                "--setting-sources",
                "",
                "--permission-mode",
                "bypassPermissions",
                "--agents",
                agents_json,
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

    let requests = server.requests().await;
    assert_eq!(requests.len(), 4);
    let sub_tools = request_tool_names(&requests[1]);
    assert!(
        !sub_tools.contains(&"Task".to_string()),
        "sub-agent must not get Task: {:?}",
        sub_tools
    );
    let third = request_messages_text(&requests[2]);
    assert!(
        third.contains("Unknown tool 'Task'"),
        "nested Task should be refused: {}",
        third
    );
}

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

        Reply::text("specialist done"),
        Reply::text("parent done"),
    ]).await;

    let (_stdout, stderr, code) = tokio::time::timeout(Duration::from_secs(90), async {
        ZenRun::new("subagent_model", &home, &cwd)
            .args([
                "-p",
                "--output-format",
                "json",
                "--bare",
                "--setting-sources",
                "",
                "--permission-mode",
                "bypassPermissions",
                "--agents",
                agents_json,
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

    let second_req = request_json(&requests[1]);
    let second_model = second_req["model"].as_str().unwrap();
    assert_eq!(
        second_model, "specialist-model",
        "sub-agent should use its own model"
    );
}
