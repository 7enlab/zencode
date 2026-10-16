//! End-to-end tests for headless `-p` output formats, exit codes and stdin handling.
//!
//! These run the compiled `zen` binary against the shared loopback mock server in
//! `common/mod.rs` and assert on the *wire shape* the CLI promises (SPEC §14): the exact
//! field sets of `init`/`assistant`/`user`/`result` records, the fact that the last line is
//! always a `result`, and that `session_id` is stable across a run.

mod common;

use common::*;
use std::time::Duration;

/// `-p --output-format json` emits exactly one `result` line with the documented field set
/// and a consistent session id (also equal to `--session-id` when supplied).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn json_output_result_shape_and_session_id() {
    let home = TempDir::new("json_home");
    let cwd = TempDir::new("json_cwd");
    let server = MockServer::single_text("hello from mock").await;

    let out = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("json", &home, &cwd)
            .args(["-p", "--output-format", "json", "--bare", "--setting-sources", ""])
            .arg("--session-id").arg("00000000-0000-4000-8000-000000000003")
            .mock("mock-model", &server)
            .arg("say hi")
            .run_output()
    })
    .await
    .expect("timed out");

    let (stdout, stderr, code) = out;
    assert_eq!(code, 0, "stderr: {}\nstdout: {}", stderr, stdout);

    let lines = ndjson(&stdout);
    assert_eq!(lines.len(), 1, "json output must be a single result line:\n{}", stdout);
    let v = &lines[0];
    assert_eq!(v["type"], "result");
    assert_eq!(v["subtype"], "success");
    assert_eq!(v["is_error"], false);
    assert_eq!(v["result"], "hello from mock");
    assert_eq!(v["session_id"], "00000000-0000-4000-8000-000000000003");
    assert_eq!(v["model"], "mock-model");
    assert_eq!(v["total_cost_usd"], 0.0);
    assert!(v["permission_denials"].as_array().is_some());
    assert_eq!(v["usage"]["input_tokens"], 7);
    assert_eq!(v["usage"]["output_tokens"], 3);
    // Deterministic field set: catches accidental schema drift.
    assert_fields(
        v,
        &[
            "type",
            "subtype",
            "is_error",
            "duration_ms",
            "duration_api_ms",
            "num_turns",
            "result",
            "session_id",
            "total_cost_usd",
            "usage",
            "model",
            "permission_denials",
            // SPEC C8.2: zen's extra fields.
            "uuid",
            "stop_reason",
        ],
        "result",
    );
    assert_eq!(v["stop_reason"], "end_turn");
}

/// `text` output prints only the assistant's final text (no JSON, no tool noise).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_output_is_just_the_answer() {
    let home = TempDir::new("text_home");
    let cwd = TempDir::new("text_cwd");
    let server = MockServer::single_text("plain text answer").await;

    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("text", &home, &cwd)
            .args(["-p", "--bare", "--setting-sources", ""])
            .mock("mock-model", &server)
            .arg("hi")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    assert_eq!(stdout.trim(), "plain text answer", "unexpected stdout: {:?}", stdout);
}

/// `stream-json` starts with a `system`/`init` line, includes an `assistant` line, and always
/// ends with the `result` line; every line is valid JSON with the same `session_id`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_json_line_shapes_and_order() {
    let home = TempDir::new("stream_home");
    let cwd = TempDir::new("stream_cwd");
    let server = MockServer::single_text("streamed answer").await;

    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("stream", &home, &cwd)
            .args([
                "-p",
                "--output-format",
                "stream-json",
                "--bare",
                "--setting-sources",
                "",
            ])
            .arg("--session-id").arg("00000000-0000-4000-8000-000000000001")
            .mock("mock-model", &server)
            .arg("say hi")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let lines = ndjson(&stdout);
    assert!(lines.len() >= 3, "expected init + assistant + result:\n{}", stdout);

    // Init line is first and carries the documented fields.
    let init = &lines[0];
    assert_eq!(init["type"], "system");
    assert_eq!(init["subtype"], "init");
    assert_eq!(init["session_id"], "00000000-0000-4000-8000-000000000001");
    assert_eq!(init["model"], "mock-model");
    assert_eq!(init["permissionMode"], "default");
    assert!(init["tools"].as_array().is_some());
    assert!(init["slash_commands"].as_array().is_some());
    assert_fields(
        init,
        &[
            "type",
            "subtype",
            "session_id",
            "cwd",
            "model",
            "tools",
            "mcp_servers",
            "permissionMode",
            "slash_commands",
        ],
        "init",
    );

    // Assistant line shape.
    let asst = find_line(&lines, "assistant", None).expect("assistant line");
    assert_eq!(asst["session_id"], "00000000-0000-4000-8000-000000000001");
    assert_eq!(asst["message"]["role"], "assistant");
    assert_eq!(asst["message"]["content"], "streamed answer");
    assert_fields(
        asst,
        &["type", "message", "session_id", "parent_tool_use_id", "uuid"],
        "assistant",
    );
    // SPEC C8.3: assistant lines carry a uuid.
    assert!(!asst["uuid"].as_str().unwrap_or("").is_empty(), "assistant uuid: {}", asst);

    // Last line is the result and matches the assistant text.
    let last = lines.last().unwrap();
    assert_eq!(last["type"], "result");
    assert_eq!(last["result"], "streamed answer");
    assert_eq!(last["session_id"], "00000000-0000-4000-8000-000000000001");

    // Every line carries the same session id.
    for l in &lines {
        assert_eq!(l["session_id"], "00000000-0000-4000-8000-000000000001", "line lost session id: {}", l);
    }
}

/// A model-side failure still produces a `result` with `is_error: true` and a non-zero exit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_error_yields_error_result_and_nonzero_exit() {
    let home = TempDir::new("err_home");
    let cwd = TempDir::new("err_cwd");
    // 400 is non-retryable, so the client fails fast without backoff delays.
    let server = MockServer::start(vec![Reply::status(400, "bad request from mock")]).await;

    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("err", &home, &cwd)
            .args(["-p", "--output-format", "json", "--bare", "--setting-sources", ""])
            .mock("mock-model", &server)
            .arg("hi")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_ne!(code, 0, "model error must be a non-zero exit\nstdout: {}", stdout);
    let v = last_result(&stdout);
    assert_eq!(v["is_error"], true);
    assert_eq!(v["subtype"], "error_during_execution");
    assert!(
        v["result"].as_str().unwrap_or("").contains("400"),
        "error text should mention the status: {}",
        v
    );
    let _ = stderr;
}

/// An unknown flag is rejected by clap with a non-zero exit and a message on stderr.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_flag_is_rejected_with_stderr() {
    let home = TempDir::new("badflag_home");
    let cwd = TempDir::new("badflag_cwd");

    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("badflag", &home, &cwd)
            .args(["-p", "--definitely-not-a-flag", "hi"])
            .run_output()
    })
    .await
    .expect("timed out");

    assert_ne!(code, 0, "invalid flag must fail");
    assert!(
        !stderr.trim().is_empty(),
        "clap should explain the error on stderr; stdout: {}",
        stdout
    );
    assert!(stdout.trim().is_empty(), "nothing should go to stdout: {}", stdout);
}

/// `--output-format` with an unknown value is also rejected by clap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_output_format_is_rejected() {
    let home = TempDir::new("badfmt_home");
    let cwd = TempDir::new("badfmt_cwd");

    let (_, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("badfmt", &home, &cwd)
            .args(["-p", "--output-format", "yaml", "hi"])
            .run_output()
    })
    .await
    .expect("timed out");

    assert_ne!(code, 0);
    assert!(stderr.contains("yaml") || stderr.contains("invalid"), "stderr: {}", stderr);
}

/// Missing prompt (no positional arg and stdin closed) is a clean error, not a panic.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_prompt_reports_error() {
    let home = TempDir::new("noprompt_home");
    let cwd = TempDir::new("noprompt_cwd");

    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("noprompt", &home, &cwd)
            .args(["-p", "--bare", "--setting-sources", ""])
            .run_output()
    })
    .await
    .expect("timed out");

    assert_ne!(code, 0);
    assert!(
        stderr.to_lowercase().contains("no prompt") || stdout.to_lowercase().contains("no prompt"),
        "expected a 'no prompt' error; stdout={} stderr={}",
        stdout,
        stderr
    );
}

/// Piped stdin is prepended to the positional prompt, separated by a blank line.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stdin_is_prepended_to_prompt() {
    let home = TempDir::new("stdin_home");
    let cwd = TempDir::new("stdin_cwd");
    let server = MockServer::single_text("got it").await;

    let (_, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("stdin", &home, &cwd)
            .args(["-p", "--bare", "--setting-sources", ""])
            .mock("mock-model", &server)
            .stdin("stdin-payload-line")
            .arg("arg-payload-line")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let requests = server.requests().await;
    assert_eq!(requests.len(), 1, "expected exactly one model request");
    let text = request_messages_text(&requests[0]);
    assert!(text.contains("stdin-payload-line"), "stdin missing: {}", text);
    assert!(text.contains("arg-payload-line"), "prompt missing: {}", text);
    // stdin comes first
    let si = text.find("stdin-payload-line").unwrap();
    let ai = text.find("arg-payload-line").unwrap();
    assert!(si < ai, "stdin must be prepended before the arg prompt");
}

/// The subprocess must not hang: even with a slow-ish mock, a generous but finite timeout
/// applies (sanity check on the harness itself).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn harness_timeout_guard_exists() {
    let home = TempDir::new("timeout_home");
    let cwd = TempDir::new("timeout_cwd");
    let server = MockServer::single_text("quick").await;
    let (_, _, code) = tokio::time::timeout(Duration::from_secs(30), async {
        ZenRun::new("timeout", &home, &cwd)
            .args(["-p", "--bare", "--setting-sources", ""])
            .mock("m", &server)
            .arg("hi")
            .run_output()
    })
    .await
    .expect("harness should complete well within 30s");
    assert_eq!(code, 0);
}
