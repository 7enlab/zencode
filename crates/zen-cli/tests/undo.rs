#![cfg(unix)]

mod common;

use common::*;

fn tool_call(id: &str, name: &str, args: serde_json::Value) -> Reply {
    Reply::with_tool_calls(
        "",
        Some(serde_json::json!([{
            "id": id,
            "type": "function",
            "function": {"name": name, "arguments": args.to_string()}
        }])),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p0_3_repl_undo_restores_edit_and_removes_written_file() {
    let home = TempDir::new("p0_3_home");
    let cwd = TempDir::new("p0_3_cwd");
    cwd.write("a.txt", "old line\nkeep\n");
    let server = MockServer::start(vec![
        tool_call("r1", "Read", serde_json::json!({"file_path": "a.txt"})),
        tool_call(
            "e1",
            "Edit",
            serde_json::json!({"file_path": "a.txt", "old_string": "old line", "new_string": "new line"}),
        ),
        tool_call("w1", "Write", serde_json::json!({"file_path": "b.txt", "content": "created\n"})),
        Reply::text("edited both"),
        Reply::text("second answer"),
    ])
    .await;
    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("p0_3", &home, &cwd)
            .args([
                "--setting-sources",
                "",
                "--permission-mode",
                "bypassPermissions",
            ])
            .mock("mock-model", &server)
            .stdin("edit please\n/diff --turn\n/undo\n/undo\nwhat now\n/exit\n")
            .run_output()
    })
    .await
    .expect("timed out");
    assert_eq!(code, 0, "stdout {stdout}\nstderr {stderr}");

    assert!(
        stdout.contains("--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n-old line\n+new line\n keep\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains("--- /dev/null\n+++ b/b.txt\n@@ -0,0 +1,1 @@\n+created\n"),
        "{stdout}"
    );

    assert!(stdout.contains("Undid turn 1 \"edit please\""), "{stdout}");
    assert!(stdout.contains("nothing to undo"), "{stdout}");
    assert_eq!(
        std::fs::read_to_string(cwd.path().join("a.txt")).unwrap(),
        "old line\nkeep\n"
    );
    assert!(
        !cwd.path().join("b.txt").exists(),
        "b.txt should be deleted"
    );

    let reqs = server.requests().await;
    let last = request_messages_text(reqs.last().unwrap());
    assert!(
        last.contains("[/undo]") && last.contains("what now"),
        "{last}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p0_3_repl_undo_refuses_after_outside_edit_until_forced() {
    let home = TempDir::new("p0_3f_home");
    let cwd = TempDir::new("p0_3f_cwd");
    cwd.write("a.txt", "v1\n");
    let server = MockServer::start(vec![
        tool_call("r1", "Read", serde_json::json!({"file_path": "a.txt"})),
        tool_call(
            "e1",
            "Edit",
            serde_json::json!({"file_path": "a.txt", "old_string": "v1", "new_string": "v2"}),
        ),
        Reply::text("edited"),
    ])
    .await;
    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("p0_3f", &home, &cwd)
            .args([
                "--setting-sources",
                "",
                "--permission-mode",
                "bypassPermissions",
            ])
            .mock("mock-model", &server)
            .stdin("edit\n!echo hand > a.txt\n/undo\n/undo --force\n/exit\n")
            .run_output()
    })
    .await
    .expect("timed out");
    assert_eq!(code, 0, "stdout {stdout}\nstderr {stderr}");
    assert!(stderr.contains("refusing to undo"), "stderr {stderr}");
    assert!(stdout.contains("Undid turn 1"), "{stdout}");
    assert_eq!(
        std::fs::read_to_string(cwd.path().join("a.txt")).unwrap(),
        "v1\n"
    );
}
