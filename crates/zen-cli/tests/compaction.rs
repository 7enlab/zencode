






mod common;

use common::*;


async fn turn(tag: &str, home: &TempDir, cwd: &TempDir, server: &MockServer, cont: bool, prompt: &str) -> serde_json::Value {
    let mut run = ZenRun::new(tag, home, cwd).args([
        "-p", "--output-format", "json", "--setting-sources", "project",
        "--permission-mode", "bypassPermissions",
    ]);
    if cont {
        run = run.arg("--continue");
    }
    let run = run.mock("mock-model", server).arg(prompt);
    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async { run.run_output() })
        .await
        .expect("timed out");
    assert_eq!(code, 0, "stderr: {}\nstdout: {}", stderr, stdout);
    last_result(&stdout)
}


#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_compact_triggers_summary() {
    let home = TempDir::new("compact_auto_home");
    let cwd = TempDir::new("compact_auto_cwd");

    cwd.write(".zen/settings.json", r#"{"autoCompact": 10}"#);

    let server = MockServer::start(vec![
        Reply::text("first reply"),
        Reply::text("summary of conversation so far"),
        Reply::text("second reply after compaction"),
    ]).await;

    let v = turn("compact_auto", &home, &cwd, &server, false, "turn 1").await;
    assert_eq!(v["result"], "first reply");
    let long = format!("turn 2 {}", "x".repeat(80));
    let v = turn("compact_auto", &home, &cwd, &server, true, &long).await;
    assert_eq!(v["result"], "second reply after compaction");


    let requests = server.requests().await;
    assert_eq!(requests.len(), 3, "expected 3 requests with compaction, got {}", requests.len());


    let second_req = request_json(&requests[1]);
    let no_tools = second_req.get("tools").map(|t| t.is_null() || t == &serde_json::json!([])).unwrap_or(true);
    assert!(no_tools, "compaction request should have no tools: {}", second_req["tools"]);
    let second_msgs = request_messages_text(&requests[1]);
    assert!(second_msgs.contains("Summarize this coding session"), "compaction prompt missing: {}", second_msgs);


    let third_msgs = request_messages_text(&requests[2]);
    assert!(third_msgs.contains("[Compaction summary"), "compacted history missing: {}", third_msgs);
    assert!(third_msgs.contains("summary of conversation so far"));
}


#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_compact_slash_command() {
    let home = TempDir::new("compact_manual_home");
    let cwd = TempDir::new("compact_manual_cwd");
    cwd.write(".zen/settings.json", r#"{"autoCompact": false}"#);

    let server = MockServer::start(vec![
        Reply::text("first reply"),
        Reply::text("manual summary"),
        Reply::text("after manual compact"),
    ]).await;

    let v = turn("compact_manual", &home, &cwd, &server, false, "turn 1").await;
    assert_eq!(v["result"], "first reply");
    let v = turn("compact_manual", &home, &cwd, &server, true, "/compact").await;
    assert_eq!(v["is_error"], false);
    assert_eq!(v["result"], "Conversation compacted.");
    let v = turn("compact_manual", &home, &cwd, &server, true, "next question").await;
    assert_eq!(v["result"], "after manual compact");

    let requests = server.requests().await;
    assert_eq!(requests.len(), 3, "expected 3 requests with manual compact");

    let second_msgs = request_messages_text(&requests[1]);
    assert!(second_msgs.contains("Summarize this coding session"), "manual compact prompt: {}", second_msgs);
    assert!(second_msgs.contains("turn 1"), "summary request sees the resumed history");

    let third_msgs = request_messages_text(&requests[2]);
    assert!(third_msgs.contains("[Compaction summary"), "manual compacted history: {}", third_msgs);
    assert!(third_msgs.contains("manual summary"));
}


#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compaction_keeps_last_n_messages() {
    let home = TempDir::new("compact_keep_home");
    let cwd = TempDir::new("compact_keep_cwd");

    cwd.write(".zen/settings.json", r#"{"autoCompact": 40}"#);

    let server = MockServer::start(vec![
        Reply::text("reply 1"),
        Reply::text("reply 2"),
        Reply::text("summary"),
        Reply::text("reply 3"),
    ]).await;

    turn("compact_keep", &home, &cwd, &server, false, "alpha question").await;
    turn("compact_keep", &home, &cwd, &server, true, "beta question").await;
    let long = format!("gamma question {}", "y".repeat(200));
    let v = turn("compact_keep", &home, &cwd, &server, true, &long).await;
    assert_eq!(v["result"], "reply 3");

    let requests = server.requests().await;

    assert_eq!(requests.len(), 4);


    let msgs = request_messages(&requests[3]);
    let non_system: Vec<_> = msgs.iter().filter(|m| m["role"] != "system").collect();
    assert_eq!(non_system.len(), 5, "summary + 4 kept messages: {:?}", non_system);
    let text = request_messages_text(&requests[3]);
    assert!(text.contains("[Compaction summary"));
    assert!(text.contains("beta question") && text.contains("gamma question"));
    assert!(!text.contains("alpha question"), "oldest turn must be dropped: {}", text);
}


#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pre_compact_hook_can_block() {
    let home = TempDir::new("compact_hook_home");
    let cwd = TempDir::new("compact_hook_cwd");

    cwd.write(".zen/settings.json", r#"{
        "autoCompact": 10,
        "hooks": {
            "PreCompact": [
                {"matcher": "auto", "hooks": [
                    {"type": "command", "command": "echo 'blocking compaction' >&2; exit 2", "timeout": 5}
                ]}
            ]
        }
    }"#);

    let server = MockServer::start(vec![
        Reply::text("reply 1"),
        Reply::text("reply 2 no compaction"),
    ]).await;

    turn("compact_hook", &home, &cwd, &server, false, "turn 1").await;
    let long = format!("turn 2 {}", "z".repeat(80));
    let v = turn("compact_hook", &home, &cwd, &server, true, &long).await;
    assert_eq!(v["result"], "reply 2 no compaction");


    let requests = server.requests().await;
    assert_eq!(requests.len(), 2, "compaction should be blocked, only 2 requests");
    let second = request_messages_text(&requests[1]);
    assert!(!second.contains("[Compaction summary"));
    assert!(second.contains("turn 1") && second.contains("reply 1"));
}
