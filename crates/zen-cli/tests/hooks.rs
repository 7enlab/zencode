





mod common;

use common::*;



#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pre_tool_use_exit_2_blocks_tool() {
    let home = TempDir::new("hook_pre2_home");
    let cwd = TempDir::new("hook_pre2_cwd");
    cwd.write("test.txt", "existing");


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


    let requests = server.requests().await;
    assert_eq!(requests.len(), 2);
    let second_req = request_messages_text(&requests[1]);
    assert!(second_req.contains("BLOCKED by hook"), "hook stderr should be in tool result: {}", second_req);

    assert_eq!(std::fs::read_to_string(cwd.path().join("test.txt")).unwrap(), "existing");
}


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

    assert!(stderr.contains("hook error") || stderr.contains("exit code 3"), "hook error should be logged: {}", stderr);
}


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



    assert_eq!(code, 1, "stderr: {}", stderr);
    let v = last_result(&stdout);
    assert_eq!(v["is_error"], true, "{}", v);
    assert!(v["result"].as_str().unwrap_or("").contains("REJECTED") || stderr.contains("REJECTED"),
        "hook stderr should be surfaced: {} / {}", v, stderr);

    let requests = server.requests().await;
    assert_eq!(requests.len(), 0, "model should not be called when prompt blocked");
}



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

    let requests = server.requests().await;
    assert_eq!(requests.len(), 2, "expected 2 model requests with Stop hook feedback");
    let second = request_messages_text(&requests[1]);
    assert!(second.contains("CONTINUE: do one more thing"), "stop feedback missing: {}", second);
}


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

    let requests = server.requests().await;
    assert_eq!(requests.len(), 1);
    let text = request_messages_text(&requests[0]);
    assert!(text.contains("SESSION STARTED"), "SessionStart context missing: {}", text);
}


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
