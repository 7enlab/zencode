//! End-to-end tests for SPEC C8.1 (stdin cap) and C8.9 (SIGTERM / SIGINT in `-p`).
//!
//! The model asks for a Bash call that starts `sleep 30` in the background of its shell and
//! records the sleep's pid; the test then signals zen and checks the exit code, the result
//! output, the SessionEnd hook and that the grandchild `sleep` was stopped too.

mod common;

use common::*;
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
fn wait_for_file(p: &Path, limit: Duration) -> String {
    let start = Instant::now();
    loop {
        if let Ok(s) = std::fs::read_to_string(p) {
            if !s.trim().is_empty() {
                return s;
            }
        }
        assert!(start.elapsed() < limit, "timed out waiting for {}", p.display());
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(unix)]
fn wait_exit(child: &mut std::process::Child, limit: Duration) -> std::process::ExitStatus {
    let start = Instant::now();
    loop {
        if let Some(st) = child.try_wait().unwrap() {
            return st;
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            panic!("zen did not exit after the signal");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `ps` state of a pid (Linux and macOS; macOS has no /proc). None = no such process.
#[cfg(unix)]
fn ps_stat(pid: &str) -> Option<String> {
    let o = std::process::Command::new("ps").args(["-o", "stat=", "-p", pid]).output().ok()?;
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (o.status.success() && !s.is_empty()).then_some(s)
}

/// Alive = the process exists and is not a zombie (a reparented child may linger as Z).
#[cfg(unix)]
fn alive(pid: &str) -> bool {
    ps_stat(pid).is_some_and(|s| !s.starts_with('Z'))
}

#[cfg(unix)]
async fn start_sleeping_run(tag: &str, signal: &str) -> (std::process::ExitStatus, String, TempDir) {
    let home = TempDir::new(&format!("{}_home", tag));
    let cwd = TempDir::new(&format!("{}_cwd", tag));
    let pid_file = cwd.path().join("sleep.pid");
    let end_file = cwd.path().join("session_end.json");
    cwd.write(
        ".zen/settings.json",
        &format!(
            r#"{{"hooks":{{"SessionEnd":[{{"hooks":[{{"type":"command","command":"cat > '{}'"}}]}}]}}}}"#,
            end_file.display()
        ),
    );
    let cmd = format!("sleep 30 & echo $! > '{}'; wait", pid_file.display());
    let server = MockServer::start(vec![
        Reply::with_tool_calls(
            "",
            Some(serde_json::json!([{
                "id": "call_sleep",
                "type": "function",
                "function": {"name": "Bash", "arguments": serde_json::json!({"command": cmd}).to_string()}
            }])),
        ),
        Reply::text("should not be reached"),
    ])
    .await;

    let mut child = ZenRun::new(tag, &home, &cwd)
        .args([
            "-p",
            "--output-format",
            "json",
            "--setting-sources",
            "project",
            "--permission-mode",
            "bypassPermissions",
            "--no-session-persistence",
        ])
        .mock("mock-model", &server)
        .arg("sleep please")
        .spawn();

    let sleep_pid = wait_for_file(&pid_file, Duration::from_secs(20)).trim().to_string();
    assert!(alive(&sleep_pid), "sleep should be running");
    let st = std::process::Command::new("kill")
        .arg(signal)
        .arg(child.id().to_string())
        .status()
        .unwrap();
    assert!(st.success());
    let status = wait_exit(&mut child, Duration::from_secs(20));
    let mut stdout = String::new();
    use std::io::Read;
    child.stdout.take().unwrap().read_to_string(&mut stdout).unwrap();

    // The grandchild `sleep` is stopped as well (not only the direct bash child).
    let start = Instant::now();
    while alive(&sleep_pid) && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !alive(&sleep_pid),
        "sleep {} survived {} (zen exit {:?}, stdout {:?}, stat {:?})",
        sleep_pid,
        signal,
        status,
        stdout,
        ps_stat(&sleep_pid)
    );
    let end = wait_for_file(&end_file, Duration::from_secs(5));
    let end: serde_json::Value = serde_json::from_str(&end).unwrap();
    assert_eq!(end["hook_event_name"], "SessionEnd", "{}", end);
    (status, stdout, cwd)
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c8_9_sigterm_exits_143_kills_bash_and_runs_session_end() {
    let (status, stdout, _cwd) = start_sleeping_run("sigterm", "-TERM").await;
    assert_eq!(status.code(), Some(143), "stdout: {}", stdout);
    assert!(stdout.trim().is_empty(), "SIGTERM prints no result: {}", stdout);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c8_9_sigint_ends_turn_with_error_result() {
    let (status, stdout, _cwd) = start_sleeping_run("sigint", "-INT").await;
    assert_eq!(status.code(), Some(130), "stdout: {}", stdout);
    let v = last_result(&stdout);
    assert_eq!(v["type"], "result");
    assert_eq!(v["is_error"], true);
    assert_eq!(v["subtype"], "error_during_execution");
    assert_eq!(v["result"], "Interrupted by user");
}

/// C8.1: stdin over 10 MB is an error with a non-zero exit, before any model call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c8_1_stdin_over_10mb_is_rejected() {
    let home = TempDir::new("bigstdin_home");
    let cwd = TempDir::new("bigstdin_cwd");
    let server = MockServer::single_text("unused").await;
    let big = "a".repeat(10 * 1024 * 1024 + 10);
    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("bigstdin", &home, &cwd)
            .args(["-p", "--output-format", "json", "--bare", "--setting-sources", ""])
            .mock("mock-model", &server)
            .stdin(big)
            .arg("summarize")
            .run_output()
    })
    .await
    .expect("timed out");
    assert_ne!(code, 0, "stdout: {}", stdout);
    assert!(stderr.contains("10 MB"), "stderr: {}", stderr);
    assert!(server.requests().await.is_empty(), "no model call expected");
}
