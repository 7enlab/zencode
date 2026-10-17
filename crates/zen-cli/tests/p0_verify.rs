//! perf/p0: settings `verify.afterEdit` runs a check after each successful edit and puts a
//! failure's output into that tool result. Driven through the real binary against a mock.
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
async fn p0_6_verify_after_edit_feeds_failure_into_the_tool_result() {
    let home = TempDir::new("p0_verify_home");
    let cwd = TempDir::new("p0_verify_cwd");
    cwd.write(
        ".zen/settings.json",
        r#"{"verify": {"afterEdit": "echo AFTER-EDIT-CHECK; exit 2"}}"#,
    );
    let server = MockServer::start(vec![
        tool_call(
            "w1",
            "Write",
            serde_json::json!({"file_path": "new.txt", "content": "x"}),
        ),
        Reply::text("done"),
    ])
    .await;
    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("p0_verify", &home, &cwd)
            .args([
                "-p",
                "--output-format",
                "json",
                "--setting-sources",
                "project",
                "--permission-mode",
                "acceptEdits",
            ])
            .mock("mock-model", &server)
            .arg("make a file")
            .run_output()
    })
    .await
    .expect("timed out");
    assert_eq!(code, 0, "stderr: {}", stderr);
    assert_eq!(last_result(&stdout)["result"], "done");
    let reqs = server.finish().await;
    // No stop-time `command` configured: no extra round, the check rode on the tool result.
    assert_eq!(reqs.len(), 2);
    let text = request_messages_text(&reqs[1]);
    assert!(
        text.contains("AFTER-EDIT-CHECK") && text.contains("exit 2"),
        "{}",
        text
    );
}
