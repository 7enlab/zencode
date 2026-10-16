//! End-to-end tests for permission modes and tool access control.
//!
//! These tests verify the permission system (SPEC §5) by running headless against a mock
//! server that asks for specific tools. The mock server script returns tool calls, and we
//! assert whether the tool was allowed or denied based on the CLI flags and settings.

mod common;

use common::*;

/// Default headless mode (no prompter) denies mutating tools: Write should be denied and
/// show up in `permission_denials` in the JSON result.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_headless_denies_write_tool() {
    let home = TempDir::new("perm_write_home");
    let cwd = TempDir::new("perm_write_cwd");
    // Mock returns a tool call for Write
    let server = MockServer::start(vec![
        Reply::with_tool_calls(
            "",
            Some(serde_json::json!([{
                "id": "call_1",
                "type": "function",
                "function": {"name": "Write", "arguments": "{\"file_path\":\"test.txt\",\"content\":\"hello\"}"}
            }])),
        ),
        // Second response after denial
        Reply::text("write was denied"),
    ]).await;

    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("perm_write", &home, &cwd)
            .args(["-p", "--output-format", "json", "--bare", "--setting-sources", ""])
            .mock("mock-model", &server)
            .arg("write a file")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr); // process succeeds, but tool denied
    let v = last_result(&stdout);
    assert_eq!(v["is_error"], false); // tool denial is not a process error
    let denials = v["permission_denials"].as_array().expect("permission_denials array");
    assert!(!denials.is_empty(), "expected Write denial, got: {}", v);
    let denied_tool = denials[0]["tool_name"].as_str().expect("tool_name string");
    assert_eq!(denied_tool, "Write", "expected Write tool denied, got: {}", denied_tool);
}

/// `--permission-mode bypassPermissions` allows all tools (except those in deny rules).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bypass_permissions_allows_write() {
    let home = TempDir::new("bypass_home");
    let cwd = TempDir::new("bypass_cwd");
    cwd.write("test.txt", "existing");

    let server = MockServer::start(vec![
        Reply::with_tool_calls(
            "",
            Some(serde_json::json!([{
                "id": "call_1",
                "type": "function",
                "function": {"name": "Write", "arguments": "{\"file_path\":\"test.txt\",\"content\":\"hello\"}"}
            }])),
        ),
        Reply::text("write succeeded"),
    ]).await;

    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("bypass", &home, &cwd)
            .args([
                "-p", "--output-format", "json", "--bare", "--setting-sources", "",
                "--permission-mode", "bypassPermissions"
            ])
            .mock("mock-model", &server)
            .arg("write a file")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let v = last_result(&stdout);
    assert_eq!(v["result"], "write succeeded");
    let denials = v["permission_denials"].as_array().expect("permission_denials array");
    assert!(denials.is_empty(), "expected no denials with bypassPermissions, got: {:?}", denials);
}

/// `--permission-mode acceptEdits` allows edit-like tools (Write, Edit) but not Bash.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accept_edits_allows_write_but_not_bash() {
    let home = TempDir::new("accept_home");
    let cwd = TempDir::new("accept_cwd");
    cwd.write("test.txt", "existing");

    // Test Write is allowed
    let server = MockServer::start(vec![
        Reply::with_tool_calls(
            "",
            Some(serde_json::json!([{
                "id": "call_1",
                "type": "function",
                "function": {"name": "Write", "arguments": "{\"file_path\":\"test.txt\",\"content\":\"hello\"}"}
            }])),
        ),
        Reply::text("write ok"),
    ]).await;

    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("accept_write", &home, &cwd)
            .args([
                "-p", "--output-format", "json", "--bare", "--setting-sources", "",
                "--permission-mode", "acceptEdits"
            ])
            .mock("mock-model", &server)
            .arg("write a file")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let v = last_result(&stdout);
    assert_eq!(v["result"], "write ok");
    let denials = v["permission_denials"].as_array().expect("permission_denials array");
    assert!(denials.is_empty(), "Write should be allowed in acceptEdits, got denials: {:?}", denials);
}

/// `--permission-mode plan` denies all mutating tools.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_mode_denies_write() {
    let home = TempDir::new("plan_home");
    let cwd = TempDir::new("plan_cwd");

    let server = MockServer::start(vec![
        Reply::with_tool_calls(
            "",
            Some(serde_json::json!([{
                "id": "call_1",
                "type": "function",
                "function": {"name": "Write", "arguments": "{\"file_path\":\"test.txt\",\"content\":\"hello\"}"}
            }])),
        ),
        Reply::text("plan mode denied"),
    ]).await;

    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("plan", &home, &cwd)
            .args([
                "-p", "--output-format", "json", "--bare", "--setting-sources", "",
                "--permission-mode", "plan"
            ])
            .mock("mock-model", &server)
            .arg("write a file")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let v = last_result(&stdout);
    assert_eq!(v["result"], "plan mode denied");
    // Plan mode hides mutating tools from the request; a forced Write call is refused with a
    // tool error and never touches the disk.
    let requests = server.requests().await;
    assert_eq!(requests.len(), 2);
    let tools = request_tool_names(&requests[0]);
    assert!(tools.contains(&"Read".to_string()), "read-only tools stay: {:?}", tools);
    assert!(!tools.contains(&"Write".to_string()), "Write must be hidden in plan mode: {:?}", tools);
    let second = request_messages_text(&requests[1]);
    assert!(second.contains("not available in plan mode"), "tool error missing: {}", second);
    assert!(!cwd.path().join("test.txt").exists(), "plan mode must not write files");
}

/// `--allowedTools Write` adds Write to the allow list, permitting it in default mode.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn allowed_tools_write_permits_write_in_default_mode() {
    let home = TempDir::new("allowed_home");
    let cwd = TempDir::new("allowed_cwd");
    cwd.write("test.txt", "existing");

    let server = MockServer::start(vec![
        Reply::with_tool_calls(
            "",
            Some(serde_json::json!([{
                "id": "call_1",
                "type": "function",
                "function": {"name": "Write", "arguments": "{\"file_path\":\"test.txt\",\"content\":\"hello\"}"}
            }])),
        ),
        Reply::text("write ok"),
    ]).await;

    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("allowed", &home, &cwd)
            .args([
                "-p", "--output-format", "json", "--bare", "--setting-sources", "",
                "--allowedTools", "Write"
            ])
            .mock("mock-model", &server)
            .arg("write a file")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let v = last_result(&stdout);
    assert_eq!(v["result"], "write ok");
    let denials = v["permission_denials"].as_array().expect("permission_denials array");
    assert!(denials.is_empty(), "Write should be allowed via --allowedTools, got denials: {:?}", denials);
}

/// `--disallowedTools Read` removes Read from the tools list sent to the model.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disallowed_tools_removes_tool_from_request() {
    let home = TempDir::new("disallowed_home");
    let cwd = TempDir::new("disallowed_cwd");
    cwd.write("test.txt", "existing");

    let server = MockServer::single_text("ok").await;

    let (stdout, _, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("disallowed", &home, &cwd)
            .args([
                "-p", "--output-format", "stream-json", "--bare", "--setting-sources", "",
                "--disallowedTools", "Read"
            ])
            .mock("mock-model", &server)
            .arg("read a file")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0);
    let lines = ndjson(&stdout);
    let init = &lines[0];
    let tools: Vec<String> = init["tools"].as_array().unwrap()
        .iter().filter_map(|t| t["function"]["name"].as_str().map(|s| s.to_string()))
        .collect();
    assert!(!tools.contains(&"Read".to_string()), "Read should be removed from tools list: {:?}", tools);
}

/// A deny rule in project `.zen/settings.json` beats `--allowedTools`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_deny_beats_cli_allow() {
    let home = TempDir::new("settings_deny_home");
    let cwd = TempDir::new("settings_deny_cwd");
    cwd.write("test.txt", "existing");

    // Project settings with deny rule for Write
    // Scoped deny: a bare `Write` deny would hide the tool entirely (SPEC C2.2).
    write_settings(cwd.path(), "settings.json", r#"{"permissions":{"deny":["Write(test.txt)"]}}"#);

    let server = MockServer::start(vec![
        Reply::with_tool_calls(
            "",
            Some(serde_json::json!([{
                "id": "call_1",
                "type": "function",
                "function": {"name": "Write", "arguments": "{\"file_path\":\"test.txt\",\"content\":\"hello\"}"}
            }])),
        ),
        Reply::text("denied by settings"),
    ]).await;

    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("settings_deny", &home, &cwd)
            .args([
                "-p", "--output-format", "json", "--bare", "--setting-sources", "project",
                "--allowedTools", "Write"
            ])
            .mock("mock-model", &server)
            .arg("write a file")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let v = last_result(&stdout);
    let denials = v["permission_denials"].as_array().expect("permission_denials array");
    assert!(!denials.is_empty(), "settings deny should beat CLI allow");
    let denied_tool = denials[0]["tool_name"].as_str().expect("tool_name string");
    assert_eq!(denied_tool, "Write");
}

/// Read-only tools (Read, Glob, Grep) are always allowed in default mode without --allowedTools.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_only_tools_allowed_by_default() {
    let home = TempDir::new("readonly_home");
    let cwd = TempDir::new("readonly_cwd");
    cwd.write("test.txt", "content");

    let server = MockServer::start(vec![
        Reply::with_tool_calls(
            "",
            Some(serde_json::json!([{
                "id": "call_1",
                "type": "function",
                "function": {"name": "Read", "arguments": "{\"file_path\":\"test.txt\"}"}
            }])),
        ),
        Reply::text("read ok"),
    ]).await;

    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("readonly", &home, &cwd)
            .args(["-p", "--output-format", "json", "--bare", "--setting-sources", ""])
            .mock("mock-model", &server)
            .arg("read a file")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let v = last_result(&stdout);
    assert_eq!(v["result"], "read ok");
    let denials = v["permission_denials"].as_array().expect("permission_denials array");
    assert!(denials.is_empty(), "Read should be allowed by default, got denials: {:?}", denials);
}
fn write_call(id: &str) -> Reply {
    Reply::with_tool_calls(
        "",
        Some(serde_json::json!([{
            "id": id,
            "type": "function",
            "function": {"name": "Write", "arguments": "{\"file_path\":\"out.txt\",\"content\":\"hello\"}"}
        }])),
    )
}

/// SPEC C2.2: a tool-level deny in settings (`Bash(*)`) removes Bash from the request; a scoped
/// deny (`WebFetch(domain:x.test)`) keeps the tool.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c2_2_settings_bare_deny_removes_tool_scoped_deny_keeps_it() {
    let home = TempDir::new("c22_home");
    let cwd = TempDir::new("c22_cwd");
    write_settings(
        cwd.path(),
        "settings.json",
        r#"{"permissions":{"deny":["Bash(*)","WebFetch(domain:x.test)"]}}"#,
    );
    let server = MockServer::single_text("ok").await;
    let (_, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("c22", &home, &cwd)
            .args(["-p", "--output-format", "json", "--bare", "--setting-sources", "project"])
            .mock("mock-model", &server)
            .arg("hi")
            .run_output()
    })
    .await
    .expect("timed out");
    assert_eq!(code, 0, "stderr: {}", stderr);
    let requests = server.requests().await;
    let tools = request_tool_names(&requests[0]);
    assert!(!tools.contains(&"Bash".to_string()), "Bash should be hidden: {:?}", tools);
    assert!(tools.contains(&"WebFetch".to_string()), "WebFetch should stay: {:?}", tools);
    assert!(tools.contains(&"Read".to_string()), "{:?}", tools);
}

/// SPEC C2.13: a PreToolUse hook `allow` answers a mode-default prompt (Write runs headless)
/// but cannot override an explicit `ask` rule (Write stays denied with no prompter).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c2_13_hook_allow_does_not_override_ask_rule() {
    let hook = r#""hooks":{"PreToolUse":[{"matcher":"Write","hooks":[{"type":"command","command":"echo '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"allow\"}}'"}]}]}"#;
    for (ask_rule, expect_written) in [(false, true), (true, false)] {
        let tag = if ask_rule { "c213_ask" } else { "c213_noask" };
        let home = TempDir::new(&format!("{}_home", tag));
        let cwd = TempDir::new(&format!("{}_cwd", tag));
        let perms = if ask_rule { r#""permissions":{"ask":["Write"]},"# } else { "" };
        write_settings(cwd.path(), "settings.json", &format!("{{{}{}}}", perms, hook));
        let server = MockServer::start(vec![write_call("call_1"), Reply::text("done")]).await;
        let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
            ZenRun::new(tag, &home, &cwd)
                .args(["-p", "--output-format", "json", "--setting-sources", "project"])
                .mock("mock-model", &server)
                .arg("write")
                .run_output()
        })
        .await
        .expect("timed out");
        assert_eq!(code, 0, "stderr: {}", stderr);
        let v = last_result(&stdout);
        let denials = v["permission_denials"].as_array().expect("permission_denials");
        assert_eq!(cwd.path().join("out.txt").exists(), expect_written, "ask_rule={} {}", ask_rule, v);
        assert_eq!(denials.is_empty(), expect_written, "ask_rule={} {}", ask_rule, v);
    }
}

/// SPEC C2.4 end to end: `--allowedTools "Bash(echo *)"` does not allow `echo hi && touch pwned`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c2_4_compound_bash_needs_every_part_allowed() {
    let home = TempDir::new("c24_home");
    let cwd = TempDir::new("c24_cwd");
    let server = MockServer::start(vec![
        Reply::with_tool_calls(
            "",
            Some(serde_json::json!([{
                "id": "call_1",
                "type": "function",
                "function": {"name": "Bash", "arguments": "{\"command\":\"echo hi && touch pwned\"}"}
            }])),
        ),
        Reply::text("done"),
    ])
    .await;
    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("c24", &home, &cwd)
            .args([
                "-p", "--output-format", "json", "--bare", "--setting-sources", "",
                "--allowedTools", "Bash(echo *)",
            ])
            .mock("mock-model", &server)
            .arg("go")
            .run_output()
    })
    .await
    .expect("timed out");
    assert_eq!(code, 0, "stderr: {}", stderr);
    let v = last_result(&stdout);
    assert_eq!(v["permission_denials"].as_array().map(|a| a.len()), Some(1), "{}", v);
    assert!(!cwd.path().join("pwned").exists());
}
