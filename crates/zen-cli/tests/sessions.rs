//! End-to-end tests for session persistence and resumption (SPEC §10).
//!
//! Tests cover:
//! - `--resume <id>` loads prior transcript
//! - `--continue` picks latest session for cwd
//! - `--fork-session` creates new session id from existing transcript
//! - `--no-session-persistence` writes no transcript file

mod common;

use common::*;
use std::fs;

/// Run 1 with --output-format json, capture session_id; run 2 with --resume <id> sends prior messages.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_session_replays_transcript() {
    let home = TempDir::new("resume_home");
    let cwd = TempDir::new("resume_cwd");

    // Turn 1
    let server1 = MockServer::single_text("first reply").await;
    let (stdout1, _, code1) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("resume1", &home, &cwd)
            .args([
                "-p", "--output-format", "json", "--bare", "--setting-sources", "",
                "--session-id", "00000000-0000-4000-8000-000000000004"
            ])
            .mock("mock-model", &server1)
            .arg("remember alpha")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code1, 0);
    let v1 = last_result(&stdout1);
    let session_id = v1["session_id"].as_str().unwrap().to_string();
    assert_eq!(session_id, "00000000-0000-4000-8000-000000000004");

    // Turn 2: resume the session
    let server2 = MockServer::single_text("second reply").await;
    let (_stdout2, _, code2) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("resume2", &home, &cwd)
            .args([
                "-p", "--output-format", "json", "--bare", "--setting-sources", "",
                "--resume", &session_id
            ])
            .mock("mock-model", &server2)
            .arg("what did I say?")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code2, 0);
    let requests = server2.requests().await;
    assert_eq!(requests.len(), 1);
    let msgs_text = request_messages_text(&requests[0]);
    assert!(msgs_text.contains("remember alpha"), "prior user msg missing: {}", msgs_text);
    assert!(msgs_text.contains("first reply"), "prior assistant msg missing: {}", msgs_text);
    assert!(msgs_text.contains("what did I say?"), "new prompt missing: {}", msgs_text);
    assert_eq!(msgs_text.matches("what did I say?").count(), 1, "duplicate prompt");
}

/// --continue picks the latest session for the current directory.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn continue_picks_latest_session() {
    let home = TempDir::new("continue_home");
    let cwd = TempDir::new("continue_cwd");

    // Create two session files with different timestamps
    let proj_dir = home.path().join(".zen").join("projects").join(session::encode_cwd(cwd.path()));
    fs::create_dir_all(&proj_dir).unwrap();

    // Session 1 (older)
    let p1 = proj_dir.join("first.jsonl");
    let _ = fs::write(&p1, r#"{"type":"user","uuid":"u1","parent_uuid":null,"session_id":"first","timestamp":"2024-01-01T00:00:00Z","cwd":"/tmp","message":{"role":"user","content":"first"}}"#);

    // Session 2 (newer) - sleep to ensure different mtime
    std::thread::sleep(std::time::Duration::from_millis(50));
    let p2 = proj_dir.join("second.jsonl");
    let _ = fs::write(&p2, r#"{"type":"user","uuid":"u2","parent_uuid":null,"session_id":"second","timestamp":"2024-01-01T00:00:01Z","cwd":"/tmp","message":{"role":"user","content":"second"}}"#);

    let server = MockServer::single_text("continued").await;
    let (stdout, _, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("continue", &home, &cwd)
            .args([
                "-p", "--output-format", "json", "--bare", "--setting-sources", "",
                "--continue"
            ])
            .mock("mock-model", &server)
            .arg("continue")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0);
    let v = last_result(&stdout);
    assert_eq!(v["session_id"], "second", "--continue should pick latest session");
    let requests = server.requests().await;
    let msgs_text = request_messages_text(&requests[0]);
    assert!(msgs_text.contains("second"), "should replay latest session: {}", msgs_text);
}

/// --fork-session with --resume creates a new session id but replays the transcript.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_session_creates_new_id() {
    let home = TempDir::new("fork_home");
    let cwd = TempDir::new("fork_cwd");

    // Turn 1: create a session
    let server1 = MockServer::single_text("first reply").await;
    let (stdout1, _, code1) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("fork1", &home, &cwd)
            .args([
                "-p", "--output-format", "json", "--bare", "--setting-sources", "",
                "--session-id", "00000000-0000-4000-8000-000000000005"
            ])
            .mock("mock-model", &server1)
            .arg("original prompt")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code1, 0);
    let v1 = last_result(&stdout1);
    assert_eq!(v1["session_id"], "00000000-0000-4000-8000-000000000005");

    // Turn 2: fork from that session
    let server2 = MockServer::single_text("forked reply").await;
    let (stdout2, _, code2) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("fork2", &home, &cwd)
            .args([
                "-p", "--output-format", "json", "--bare", "--setting-sources", "",
                "--resume", "00000000-0000-4000-8000-000000000005",
                "--fork-session"
            ])
            .mock("mock-model", &server2)
            .arg("forked prompt")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code2, 0);
    let v2 = last_result(&stdout2);
    let new_session_id = v2["session_id"].as_str().unwrap();
    assert_ne!(new_session_id, "00000000-0000-4000-8000-000000000005", "fork should create new session id");
    // Transcript should still be replayed
    let requests = server2.requests().await;
    let msgs_text = request_messages_text(&requests[0]);
    assert!(msgs_text.contains("original prompt"), "fork should replay transcript: {}", msgs_text);
    assert!(msgs_text.contains("forked prompt"), "new prompt should be present: {}", msgs_text);
}

/// --no-session-persistence writes no transcript file.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_session_persistence_writes_no_file() {
    let home = TempDir::new("nopersist_home");
    let cwd = TempDir::new("nopersist_cwd");

    let server = MockServer::single_text("ok").await;
    let (_, _, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("nopersist", &home, &cwd)
            .args([
                "-p", "--output-format", "json", "--bare", "--setting-sources", "",
                "--no-session-persistence",
                "--session-id", "00000000-0000-4000-8000-000000000006"
            ])
            .mock("mock-model", &server)
            .arg("hi")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0);
    // Check no transcript file was created
    let proj_dir = home.path().join(".zen").join("projects").join(session::encode_cwd(cwd.path()));
    assert!(!proj_dir.exists() || fs::read_dir(&proj_dir).unwrap().count() == 0,
        "no transcript file should exist with --no-session-persistence");
}

/// Session id is consistent across turns in the same session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_id_consistent_across_turns() {
    let home = TempDir::new("consistent_home");
    let cwd = TempDir::new("consistent_cwd");

    let server = MockServer::start(vec![
        Reply::text("reply 1"),
        Reply::text("reply 2"),
        Reply::text("reply 3"),
    ]).await;

    let (stdout1, _, code1) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("cons1", &home, &cwd)
            .args([
                "-p", "--output-format", "json", "--bare", "--setting-sources", "",
                "--session-id", "00000000-0000-4000-8000-000000000007"
            ])
            .mock("mock-model", &server)
            .arg("turn 1")
            .run_output()
    })
    .await
    .expect("timed out");
    assert_eq!(code1, 0);

    let (stdout2, _, code2) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("cons2", &home, &cwd)
            .args([
                "-p", "--output-format", "json", "--bare", "--setting-sources", "",
                "--resume", "00000000-0000-4000-8000-000000000007"
            ])
            .mock("mock-model", &server)
            .arg("turn 2")
            .run_output()
    })
    .await
    .expect("timed out");
    assert_eq!(code2, 0);

    let (stdout3, _, code3) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("cons3", &home, &cwd)
            .args([
                "-p", "--output-format", "json", "--bare", "--setting-sources", "",
                "--resume", "00000000-0000-4000-8000-000000000007"
            ])
            .mock("mock-model", &server)
            .arg("turn 3")
            .run_output()
    })
    .await
    .expect("timed out");
    assert_eq!(code3, 0);

    let v1 = last_result(&stdout1);
    let v2 = last_result(&stdout2);
    let v3 = last_result(&stdout3);
    assert_eq!(v1["session_id"], v2["session_id"]);
    assert_eq!(v2["session_id"], v3["session_id"]);
    assert_eq!(v1["session_id"], "00000000-0000-4000-8000-000000000007");
}

/// Resume with a non-existent session id fails clearly instead of silently starting fresh
/// (SPEC C9: missing session is an error), and never calls the model.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_nonexistent_session_errors() {
    let home = TempDir::new("resume_new_home");
    let cwd = TempDir::new("resume_new_cwd");

    let server = MockServer::single_text("ok").await;
    let (_, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("resume_new", &home, &cwd)
            .args([
                "-p", "--output-format", "json", "--bare", "--setting-sources", "",
                "--resume", "nonexistent-session-id"
            ])
            .mock("mock-model", &server)
            .arg("hi")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_ne!(code, 0, "missing session must fail");
    assert!(
        stderr.contains("No conversation found") && stderr.contains("nonexistent-session-id"),
        "stderr: {}",
        stderr
    );
    assert_eq!(server.requests().await.len(), 0, "model must not be called");
}

// Helper module for session path encoding (copied from zen-cli to avoid dependency)
mod session {
    use std::path::Path;
    pub fn encode_cwd(path: &Path) -> String {
        let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let s = abs.to_string_lossy();
        s.replace(['/', '.'], "-")
    }
}
/// C9.2: `--session-id` must be a valid UUID; anything else is a flag error on stderr before
/// the run (non-zero exit, no model request, no transcript).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c9_2_session_id_rejects_non_uuid() {
    let home = TempDir::new("baduuid_home");
    let cwd = TempDir::new("baduuid_cwd");
    let server = MockServer::single_text("should not be reached").await;
    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("baduuid", &home, &cwd)
            .args(["-p", "--output-format", "json", "--bare", "--setting-sources", "",
                "--session-id", "not-a-uuid"])
            .mock("mock-model", &server)
            .arg("hi")
            .run_output()
    })
    .await
    .expect("timed out");
    assert_ne!(code, 0, "stdout: {}", stdout);
    assert!(stdout.trim().is_empty(), "no result line expected: {}", stdout);
    assert!(stderr.contains("UUID"), "stderr: {}", stderr);
    assert!(server.requests().await.is_empty(), "no model request expected");
    assert!(!home.path().join(".zen").join("projects").exists());
}

/// C9.3: on resume the json result's usage covers the whole conversation, not just this run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c9_3_usage_accumulates_across_resume() {
    let home = TempDir::new("usage_home");
    let cwd = TempDir::new("usage_cwd");
    let server = MockServer::start(vec![Reply::text("one"), Reply::text("two")]).await;
    let run = |tag: &str, extra: &[&str], prompt: &str| {
        ZenRun::new(tag, &home, &cwd)
            .args(["-p", "--output-format", "json", "--bare", "--setting-sources", ""])
            .args(extra.iter().map(|s| s.to_string()))
            .mock("mock-model", &server)
            .arg(prompt)
            .run_output()
    };
    let (o1, _, _) = run("u1", &["--session-id", "00000000-0000-4000-8000-0000000000c9"], "a");
    assert_eq!(last_result(&o1)["usage"], serde_json::json!({"input_tokens": 7, "output_tokens": 3}));
    let (o2, e2, c2) = run("u2", &["--resume", "00000000-0000-4000-8000-0000000000c9"], "b");
    assert_eq!(c2, 0, "{}", e2);
    assert_eq!(last_result(&o2)["usage"], serde_json::json!({"input_tokens": 14, "output_tokens": 6}));
}
