
#![cfg(unix)]

mod common;

use common::*;

fn base_args() -> Vec<&'static str> {
    vec![
        "-p",
        "--output-format",
        "json",
        "--bare",
        "--setting-sources",
        "",
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn until_passes_first_try_no_retry() {
    let home = TempDir::new("until_home");
    let cwd = TempDir::new("until_cwd");
    let server = MockServer::start(vec![Reply::text("did the thing")]).await;
    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("until_pass", &home, &cwd)
            .args(base_args())
            .args(["--until", "exit 0"])
            .mock("mock-model", &server)
            .arg("do it")
            .run_output()
    })
    .await
    .expect("timed out");
    assert_eq!(code, 0, "stderr: {}", stderr);
    assert_eq!(last_result(&stdout)["result"], "did the thing");

    assert_eq!(server.finish().await.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn until_failure_retries_then_gives_up() {
    let home = TempDir::new("until_home");
    let cwd = TempDir::new("until_cwd");

    let server =
        MockServer::start(vec![Reply::text("try one"), Reply::text("try two"), Reply::text("try three")])
            .await;
    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("until_fail", &home, &cwd)
            .args(base_args())
            .args(["--until", "exit 3", "--max-verify-retries", "2"])
            .mock("mock-model", &server)
            .arg("do it")
            .run_output()
    })
    .await
    .expect("timed out");
    assert_eq!(code, 1, "stderr: {}", stderr);
    let reqs = server.finish().await;
    assert_eq!(reqs.len(), 3, "expected 1 run + 2 retries");
    let result = last_result(&stdout);
    assert_eq!(result["is_error"], true);
    let text = result["result"].as_str().unwrap_or_default();
    assert!(text.contains("Verify failed"), "{text}");
    assert!(text.contains("exited with code 3"), "{text}");
    assert!(text.contains("3 attempt(s)"), "{text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn until_failure_output_feeds_back_into_retry() {
    let home = TempDir::new("until_home");
    let cwd = TempDir::new("until_cwd");
    let server = MockServer::start(vec![Reply::text("first"), Reply::text("fixed")]).await;
    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("until_feedback", &home, &cwd)
            .args(base_args())
            .args(["--until", "echo VERIFY-OUTPUT-MARKER; exit 1", "--max-verify-retries", "1"])
            .mock("mock-model", &server)
            .arg("do it")
            .run_output()
    })
    .await
    .expect("timed out");
    assert_eq!(code, 1, "stderr: {}", stderr);
    let reqs = server.finish().await;
    assert_eq!(reqs.len(), 2);

    let retry_text = request_messages_text(&reqs[1]);
    assert!(retry_text.contains("VERIFY-OUTPUT-MARKER"), "{retry_text}");
    assert!(retry_text.contains("verification command"), "{retry_text}");
    assert_eq!(last_result(&stdout)["is_error"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn until_retry_succeeds_when_command_passes() {
    let home = TempDir::new("until_home");
    let cwd = TempDir::new("until_cwd");



    cwd.write("counter", "0");
    let server = MockServer::start(vec![Reply::text("first"), Reply::text("fixed")]).await;
    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("until_eventual", &home, &cwd)
            .args(base_args())
            .args([
                "--until",
                "n=$(cat counter); n=$((n+1)); echo $n > counter; test $n -ge 2",
                "--max-verify-retries",
                "3",
            ])
            .mock("mock-model", &server)
            .arg("do it")
            .run_output()
    })
    .await
    .expect("timed out");
    assert_eq!(code, 0, "stderr: {}", stderr);
    assert_eq!(last_result(&stdout)["result"], "fixed");
    assert_eq!(server.finish().await.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn until_emits_verify_event_in_stream_json() {
    let home = TempDir::new("until_home");
    let cwd = TempDir::new("until_cwd");
    let server = MockServer::start(vec![Reply::text("done")]).await;
    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("until_stream", &home, &cwd)
            .args(["-p", "--output-format", "stream-json", "--bare", "--setting-sources", ""])
            .args(["--until", "exit 0"])
            .mock("mock-model", &server)
            .arg("do it")
            .run_output()
    })
    .await
    .expect("timed out");
    assert_eq!(code, 0, "stderr: {}", stderr);

    assert!(
        stdout.contains("\"hook_event_name\":\"verify\""),
        "no verify event in stream-json output"
    );
    assert!(stdout.contains("\"type\":\"system\""));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_mode_plans_first_then_executes_with_yes() {
    let home = TempDir::new("plan_home");
    let cwd = TempDir::new("plan_cwd");
    let server = MockServer::start(vec![
        Reply::text("1. Create foo.txt\n2. Done\nExitPlanMode"),
        Reply::text("executed"),
    ])
    .await;
    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("plan_yes", &home, &cwd)
            .args(base_args())
            .args(["--plan", "--yes"])
            .mock("mock-model", &server)
            .arg("make foo")
            .run_output()
    })
    .await
    .expect("timed out");
    assert_eq!(code, 0, "stderr: {}", stderr);
    let reqs = server.finish().await;
    assert_eq!(reqs.len(), 2, "plan turn + execution turn");

    let plan_sys = system_message(&reqs[0]);
    assert!(plan_sys.contains("Plan mode"), "{plan_sys}");
    let plan_tools = request_tool_names(&reqs[0]);
    assert!(!plan_tools.iter().any(|t| t == "Write"), "Write visible in plan mode: {plan_tools:?}");

    let exec_tools = request_tool_names(&reqs[1]);
    assert!(exec_tools.iter().any(|t| t == "Write"), "Write missing in exec turn: {exec_tools:?}");
    let exec_text = request_messages_text(&reqs[1]);
    assert!(exec_text.contains("Approved plan"), "{exec_text}");
    assert!(exec_text.contains("Create foo.txt"), "{exec_text}");

    assert_eq!(last_result(&stdout)["result"], "executed");
}
