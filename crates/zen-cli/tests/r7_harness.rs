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
async fn r7_snapshot_in_system_prompt_and_stable_across_requests() {
    let home = TempDir::new("r7_snap_home");
    let cwd = TempDir::new("r7_snap_cwd");
    cwd.write("Cargo.toml", "[package]\nname = \"demo\"\n");
    cwd.write("src/lib.rs", "pub fn f() {}\n");
    let server = MockServer::start(vec![
        tool_call("c1", "Glob", serde_json::json!({"pattern": "**/*.rs"})),
        tool_call("c2", "Read", serde_json::json!({"file_path": "src/lib.rs"})),
        Reply::text("done"),
    ])
    .await;
    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("r7_snap", &home, &cwd)
            .args(["-p", "--output-format", "json", "--setting-sources", ""])
            .mock("mock-model", &server)
            .arg("look around")
            .run_output()
    })
    .await
    .expect("timed out");
    assert_eq!(code, 0, "stderr: {}", stderr);
    assert_eq!(last_result(&stdout)["result"], "done");
    let reqs = server.finish().await;
    assert_eq!(reqs.len(), 3);
    let systems: Vec<String> = reqs.iter().map(|r| system_message(r)).collect();
    assert!(
        systems[0].contains("Project snapshot") && systems[0].contains("`cargo test`"),
        "{}",
        systems[0]
    );
    assert!(systems[0].contains("src/\n    lib.rs"), "{}", systems[0]);
    assert!(
        systems.iter().all(|s| s == &systems[0]),
        "system prompt changed mid-session"
    );

    let server = MockServer::start(vec![Reply::text("bare done")]).await;
    let (_o, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("r7_snap_bare", &home, &cwd)
            .args(["-p", "--output-format", "json", "--bare"])
            .mock("mock-model", &server)
            .arg("hi")
            .run_output()
    })
    .await
    .expect("timed out");
    assert_eq!(code, 0, "stderr: {}", stderr);
    let reqs = server.finish().await;
    assert!(!system_message(&reqs[0]).contains("Project snapshot"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r7_verify_command_from_settings_feeds_failure_back() {
    let home = TempDir::new("r7_verify_home");
    let cwd = TempDir::new("r7_verify_cwd");
    cwd.write(
        ".zen/settings.json",
        r#"{"verify": {"command": "echo VERIFY-RAN-IN-$(basename $PWD); exit 1"}}"#,
    );
    let server = MockServer::start(vec![
        tool_call(
            "w1",
            "Write",
            serde_json::json!({"file_path": "new.txt", "content": "x"}),
        ),
        Reply::text("done1"),
        Reply::text("final answer"),
    ])
    .await;
    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("r7_verify", &home, &cwd)
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
    assert_eq!(last_result(&stdout)["result"], "final answer");
    let reqs = server.finish().await;
    assert_eq!(reqs.len(), 3);
    let last = request_messages_text(&reqs[2]);
    assert!(
        last.contains("VERIFY-RAN-IN-") && last.contains("exit 1"),
        "{}",
        last
    );

    let cwd2 = TempDir::new("r7_verify_cwd2");
    let server = MockServer::start(vec![
        tool_call(
            "w1",
            "Write",
            serde_json::json!({"file_path": "new.txt", "content": "x"}),
        ),
        Reply::text("done1"),
    ])
    .await;
    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("r7_verify2", &home, &cwd2)
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
    assert_eq!(last_result(&stdout)["result"], "done1");
    assert_eq!(server.finish().await.len(), 2);
}
