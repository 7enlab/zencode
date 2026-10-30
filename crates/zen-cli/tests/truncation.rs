#![cfg(unix)]

mod common;

use common::*;

fn cut(mut reply: Reply) -> Reply {
    reply.body = reply
        .body
        .replace(r#""finish_reason":"stop""#, r#""finish_reason":"length""#)
        .replace(
            r#""finish_reason":"tool_calls""#,
            r#""finish_reason":"length""#,
        );
    assert!(reply.body.contains(r#""finish_reason":"length""#));
    reply
}

fn half_write() -> Reply {
    Reply::with_tool_calls(
        "",
        Some(serde_json::json!([{
            "id": "w1",
            "type": "function",
            "function": {"name": "Write", "arguments": "{\"file_path\":\"out.txt\",\"content\":\"hal"}
        }])),
    )
}

fn lines(stdout: &str) -> Vec<serde_json::Value> {
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trunc_stream_json_continues_and_never_runs_a_half_call() {
    let home = TempDir::new("trunc_home");
    let cwd = TempDir::new("trunc_cwd");
    let server = MockServer::start(vec![
        cut(Reply::text("Writing the fi")),
        cut(half_write()),
        Reply::text("le now. Done."),
    ])
    .await;
    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("trunc", &home, &cwd)
            .args([
                "-p",
                "--output-format",
                "stream-json",
                "--bare",
                "--setting-sources",
                "",
                "--permission-mode",
                "acceptEdits",
            ])
            .mock("mock-model", &server)
            .arg("write out.txt")
            .run_output()
    })
    .await
    .expect("timed out");
    assert_eq!(code, 0, "stderr: {}", stderr);
    let recs = lines(&stdout);

    for r in &recs {
        let t = r["type"].as_str().unwrap_or("");
        assert!(
            ["system", "assistant", "user", "result"].contains(&t),
            "unexpected record {r}"
        );
    }
    let result = recs
        .iter()
        .rfind(|r| r["type"] == "result")
        .expect("result line");
    assert_eq!(result["subtype"], "success");
    assert_eq!(result["result"], "Writing the file now. Done.");

    assert!(!cwd.path().join("out.txt").exists());
    assert!(!stdout.contains("\"w1\""), "{stdout}");
    let reqs = server.finish().await;
    assert_eq!(reqs.len(), 3);
    assert!(request_messages_text(&reqs[1])
        .contains("cut off by the output token limit. Continue exactly"));
    assert!(request_messages_text(&reqs[2]).contains("no tool was run"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trunc_settings_cap_ends_with_a_clear_note() {
    let home = TempDir::new("trunc_cap_home");
    let cwd = TempDir::new("trunc_cap_cwd");
    cwd.write(".zen/settings.json", r#"{"maxContinuations": 1}"#);
    let server = MockServer::start(vec![
        cut(Reply::text("one ")),
        cut(Reply::text("two")),
        Reply::text("never"),
    ])
    .await;
    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("trunc_cap", &home, &cwd)
            .args([
                "-p",
                "--output-format",
                "stream-json",
                "--setting-sources",
                "project",
            ])
            .mock("mock-model", &server)
            .arg("go")
            .run_output()
    })
    .await
    .expect("timed out");
    assert_eq!(code, 0, "stderr: {}", stderr);
    let recs = lines(&stdout);
    let note = recs
        .iter()
        .find(|r| {
            r["type"] == "system"
                && r["subtype"] == "hook_message"
                && r["hook_event_name"] == "Truncation"
        })
        .expect("truncation note");
    assert!(
        note["message"]
            .as_str()
            .unwrap()
            .contains("may be incomplete"),
        "{note}"
    );
    let result = recs.iter().rfind(|r| r["type"] == "result").unwrap();
    assert_eq!(result["result"], "one two");
    assert_eq!(server.finish().await.len(), 2);
}
