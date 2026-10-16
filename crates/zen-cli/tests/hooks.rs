//! End-to-end tests for hooks (SPEC §13).
//!
//! Hooks are configured in settings.json under the "hooks" key and run as shell commands
//! with JSON input on stdin. Key behaviors: exit 0 = success, exit 2 = block with feedback,
//! other exits = logged error.

mod common;

use common::*;

/// PreToolUse hook with exit code 2 blocks the tool and feeds hook stderr back to the model
/// in the next request (via tool result).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pre_tool_use_exit_2_blocks_tool() {
    let home = TempDir::new("hook_pre2_home");
    let cwd = TempDir::new("hook_pre2_cwd");
    cwd.write("test.txt", "existing");

    // Hook that blocks Write with a custom message on stderr
    let hook_config = r#"{
        "hooks": {
            "PreToolUse": [
                {
                    "matcher": "Write",
                    "hooks": [{"type": "command", "command": "echo 'BLOCKED by hook' >&2; exit 2", "timeout": 5}]
                }
            ]
        }
    }"#;
    cwd.write(".zen/settings.json", hook_config);

    let server = MockServer::start(vec![
        Reply::with_tool_calls(
            "",
            Some(serde_json::json!([{
                "id": "call_1",
                "type": "function",
                "function": {"name": "Write", "arguments": "{\"file_path\":\"test.txt\",\"content\":\"hello\"}"}
            }])),
        ),
        Reply::text("hook blocked it"),
    ]).await;

    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("hook_pre2", &home, &cwd)
            .args(["-p", "--output-format", "json", "--setting-sources", "project"])
            .mock("mock-model", &server)
            .arg("write a file")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let v = last_result(&stdout);
    assert_eq!(v["result"], "hook blocked it");

    // The tool result sent back to the model should contain the hook's stderr
    let requests = server.requests().await;
    assert_eq!(requests.len(), 2);
    let second_req = request_messages_text(&requests[1]);
    assert!(second_req.contains("BLOCKED by hook"), "hook stderr should be in tool result: {}", second_req);
    // The blocked tool never ran: the file keeps its original content.
    assert_eq!(std::fs::read_to_string(cwd.path().join("test.txt")).unwrap(), "existing");
}

/// PreToolUse hook with exit code 0 allows the tool to proceed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pre_tool_use_exit_0_allows_tool() {
    let home = TempDir::new("hook_pre0_home");
    let cwd = TempDir::new("hook_pre0_cwd");
    cwd.write("test.txt", "existing");

    let hook_config = r#"{
        "hooks": {
            "PreToolUse": [
                {
                    "matcher": "Write",
                    "hooks": [{"type": "command", "command": "echo 'allowing'", "timeout": 5}]
                }
            ]
        }
    }"#;
    cwd.write(".zen/settings.json", hook_config);

    let server = MockServer::start(vec![
        Reply::with_tool_calls(
            "",
            Some(serde_json::json!([{
                "id": "call_1",
                "type": "function",
                "function": {"name": "Write", "arguments": "{\"file_path\":\"out.txt\",\"content\":\"hello\"}"}
            }])),
        ),
        Reply::text("tool allowed"),
    ]).await;

    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("hook_pre0", &home, &cwd)
            // acceptEdits so Write is allowed by permissions; only the hook could block it.
            .args(["-p", "--output-format", "json", "--setting-sources", "project", "--permission-mode", "acceptEdits"])
            .mock("mock-model", &server)
            .arg("write a file")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let v = last_result(&stdout);
    assert_eq!(v["result"], "tool allowed");
    let denials = v["permission_denials"].as_array().expect("permission_denials array");
    assert!(denials.is_empty(), "exit 0 hook should not cause denial: {:?}", denials);
    assert_eq!(std::fs::read_to_string(cwd.path().join("out.txt")).unwrap(), "hello");
}

/// PreToolUse hook with other exit code (e.g., 3) logs error but doesn't block.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pre_tool_use_other_exit_logs_but_allows() {
    let home = TempDir::new("hook_pre3_home");
    let cwd = TempDir::new("hook_pre3_cwd");
    cwd.write("test.txt", "existing");

    let hook_config = r#"{
        "hooks": {
            "PreToolUse": [
                {
                    "matcher": "Write",
                    "hooks": [{"type": "command", "command": "echo 'oops' >&2; exit 3", "timeout": 5}]
                }
            ]
        }
    }"#;
    cwd.write(".zen/settings.json", hook_config);

    let server = MockServer::start(vec![
        Reply::with_tool_calls(
            "",
            Some(serde_json::json!([{
                "id": "call_1",
                "type": "function",
                "function": {"name": "Write", "arguments": "{\"file_path\":\"test.txt\",\"content\":\"hello\"}"}
            }])),
        ),
        Reply::text("tool proceeded despite hook error"),
    ]).await;

    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("hook_pre3", &home, &cwd)
            .args(["-p", "--output-format", "json", "--setting-sources", "project"])
            .mock("mock-model", &server)
            .arg("write a file")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let v = last_result(&stdout);
    assert_eq!(v["result"], "tool proceeded despite hook error");
    // Hook error should be logged to stderr (not block)
    assert!(stderr.contains("hook error") || stderr.contains("exit code 3"), "hook error should be logged: {}", stderr);
}

/// UserPromptSubmit hook adds context (stdout) to the user prompt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_prompt_submit_hook_adds_context() {
    let home = TempDir::new("hook_ups_home");
    let cwd = TempDir::new("hook_ups_cwd");

    let hook_config = r#"{
        "hooks": {
            "UserPromptSubmit": [
                {"hooks": [{"type": "command", "command": "echo 'EXTRA CONTEXT FROM HOOK'", "timeout": 5}]}
            ]
        }
    }"#;
    cwd.write(".zen/settings.json", hook_config);

    let server = MockServer::single_text("ok").await;

    let (_, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("hook_ups", &home, &cwd)
            .args(["-p", "--output-format", "json", "--setting-sources", "project"])
            .mock("mock-model", &server)
            .arg("user prompt")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let requests = server.requests().await;
    assert_eq!(requests.len(), 1);
    let msgs_text = request_messages_text(&requests[0]);
    assert!(msgs_text.contains("EXTRA CONTEXT FROM HOOK"), "hook stdout should be added to prompt: {}", msgs_text);
}

/// UserPromptSubmit hook with exit 2 blocks the prompt entirely (stop_run).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_prompt_submit_exit_2_blocks_prompt() {
    let home = TempDir::new("hook_ups2_home");
    let cwd = TempDir::new("hook_ups2_cwd");

    let hook_config = r#"{
        "hooks": {
            "UserPromptSubmit": [
                {"hooks": [{"type": "command", "command": "echo 'REJECTED' >&2; exit 2", "timeout": 5}]}
            ]
        }
    }"#;
    cwd.write(".zen/settings.json", hook_config);

    let server = MockServer::single_text("should not reach").await;

    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("hook_ups2", &home, &cwd)
            .args(["-p", "--output-format", "json", "--setting-sources", "project"])
            .mock("mock-model", &server)
            .arg("user prompt")
            .run_output()
    })
    .await
    .expect("timed out");

    // The run stops before the model with an error result (SPEC §14: errors exit non-zero)
    // carrying the hook's stderr.
    assert_eq!(code, 1, "stderr: {}", stderr);
    let v = last_result(&stdout);
    assert_eq!(v["is_error"], true, "{}", v);
    assert!(v["result"].as_str().unwrap_or("").contains("REJECTED") || stderr.contains("REJECTED"),
        "hook stderr should be surfaced: {} / {}", v, stderr);
    // No request should reach the model
    let requests = server.requests().await;
    assert_eq!(requests.len(), 0, "model should not be called when prompt blocked");
}

/// Stop hook exiting 2 prevents stopping: its stderr becomes the model's next instruction
/// (SPEC §13). The hook blocks once (marker file) so the loop then ends normally.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_hook_feedback_continues_loop() {
    let home = TempDir::new("hook_stop_home");
    let cwd = TempDir::new("hook_stop_cwd");

    let hook_config = r#"{
        "hooks": {
            "Stop": [
                {"hooks": [{"type": "command", "command": "if [ -f .stopped ]; then exit 0; fi; touch .stopped; echo 'CONTINUE: do one more thing' >&2; exit 2", "timeout": 5}]}
            ]
        }
    }"#;
    cwd.write(".zen/settings.json", hook_config);

    let server = MockServer::start(vec![
        Reply::text("first answer"),
        Reply::text("done after stop hook"),
    ]).await;

    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("hook_stop", &home, &cwd)
            .args(["-p", "--output-format", "json", "--setting-sources", "project", "--permission-mode", "bypassPermissions"])
            .mock("mock-model", &server)
            .arg("start")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let v = last_result(&stdout);
    assert_eq!(v["result"], "done after stop hook");
    // 2 requests: the first answer was not allowed to stop; the hook's feedback drove a second.
    let requests = server.requests().await;
    assert_eq!(requests.len(), 2, "expected 2 model requests with Stop hook feedback");
    let second = request_messages_text(&requests[1]);
    assert!(second.contains("CONTINUE: do one more thing"), "stop feedback missing: {}", second);
}

/// SessionStart hook runs at session start.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_start_hook_runs() {
    let home = TempDir::new("hook_sess_home");
    let cwd = TempDir::new("hook_sess_cwd");

    let hook_config = r#"{
        "hooks": {
            "SessionStart": [
                {"matcher": "startup", "hooks": [{"type": "command", "command": "echo 'SESSION STARTED'", "timeout": 5}]}
            ]
        }
    }"#;
    cwd.write(".zen/settings.json", hook_config);

    let server = MockServer::single_text("ok").await;

    let (_, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("hook_sess", &home, &cwd)
            .args(["-p", "--output-format", "json", "--setting-sources", "project"])
            .mock("mock-model", &server)
            .arg("hi")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    // SessionStart exit-0 stdout is added to the context of the first prompt (SPEC §13).
    let requests = server.requests().await;
    assert_eq!(requests.len(), 1);
    let text = request_messages_text(&requests[0]);
    assert!(text.contains("SESSION STARTED"), "SessionStart context missing: {}", text);
}

/// Multiple PreToolUse hooks for same matcher run concurrently; one blocking wins.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multiple_pre_tool_use_hooks_one_blocks() {
    let home = TempDir::new("hook_multi_home");
    let cwd = TempDir::new("hook_multi_cwd");
    cwd.write("test.txt", "existing");

    let hook_config = r#"{
        "hooks": {
            "PreToolUse": [
                {
                    "matcher": "Write",
                    "hooks": [
                        {"type": "command", "command": "echo 'first allows'", "timeout": 5},
                        {"type": "command", "command": "echo 'SECOND BLOCKS' >&2; exit 2", "timeout": 5}
                    ]
                }
            ]
        }
    }"#;
    cwd.write(".zen/settings.json", hook_config);

    let server = MockServer::start(vec![
        Reply::with_tool_calls(
            "",
            Some(serde_json::json!([{
                "id": "call_1",
                "type": "function",
                "function": {"name": "Write", "arguments": "{\"file_path\":\"test.txt\",\"content\":\"hello\"}"}
            }])),
        ),
        Reply::text("blocked by second hook"),
    ]).await;

    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("hook_multi", &home, &cwd)
            .args(["-p", "--output-format", "json", "--setting-sources", "project"])
            .mock("mock-model", &server)
            .arg("write a file")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let v = last_result(&stdout);
    assert_eq!(v["result"], "blocked by second hook");
    let requests = server.requests().await;
    assert_eq!(requests.len(), 2);
    let second_req = request_messages_text(&requests[1]);
    assert!(second_req.contains("SECOND BLOCKS"), "blocking hook stderr should be in result: {}", second_req);
}