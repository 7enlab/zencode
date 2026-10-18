//! The stream-json contract (docs/stream-json-v1.md) cannot drift from the binary.
//!
//! Each scenario runs the real `zen -p --output-format stream-json` against the mock LLM, then
//! normalises the values that change per run (uuids, temp paths, the mock's port, durations) and
//! compares the stream with `docs/fixtures/stream-json/<name>.ndjson`. A difference fails the
//! test; `ZEN_UPDATE_FIXTURES=1 cargo test -p zen-cli --test stream_json_contract` rewrites the
//! fixtures. A second check makes sure every record type, subtype and top-level field that shows
//! up in a fixture is named in the doc.
#![cfg(unix)]

mod common;

use common::*;
use serde_json::{json, Value};
use std::path::PathBuf;

const SESSION: &str = "00000000-0000-4000-8000-0000000000aa";

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/fixtures/stream-json")
}

fn doc_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/stream-json-v1.md")
}

/// `reply` with its finish_reason changed to `length` (output cut off by the token limit).
fn cut(mut reply: Reply) -> Reply {
    reply.body = reply
        .body
        .replace(r#""finish_reason":"stop""#, r#""finish_reason":"length""#)
        .replace(r#""finish_reason":"tool_calls""#, r#""finish_reason":"length""#);
    reply
}

fn tool_call(id: &str, name: &str, args: Value) -> Reply {
    Reply::with_tool_calls(
        "",
        Some(json!([{"id": id, "type": "function", "function": {"name": name, "arguments": args.to_string()}}])),
    )
}

/// Replace per-run values so the stream is byte-stable.
fn normalise(stdout: &str, subs: &[(String, &str)]) -> String {
    let mut out = String::new();
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let mut l = line.to_string();
        for (from, to) in subs {
            if !from.is_empty() {
                l = l.replace(from.as_str(), to);
            }
        }
        let mut v: Value = serde_json::from_str(&l).unwrap_or_else(|e| panic!("not JSON ({e}): {line}"));
        if let Some(o) = v.as_object_mut() {
            for (k, placeholder) in [("uuid", json!("<uuid>")), ("duration_ms", json!(0)), ("duration_api_ms", json!(0))] {
                if o.contains_key(k) {
                    o.insert(k.into(), placeholder);
                }
            }
        }
        out.push_str(&serde_json::to_string(&v).unwrap());
        out.push('\n');
    }
    out
}

/// Run one scenario and compare (or rewrite) its fixture.
async fn check(name: &str, replies: Vec<Reply>, extra_args: &[&str], setup: impl FnOnce(&TempDir)) {
    check_with_prompt(name, replies, extra_args, setup, "hello").await;
}

/// Run one scenario with a custom user prompt and compare (or rewrite) its fixture.
async fn check_with_prompt(
    name: &str,
    replies: Vec<Reply>,
    extra_args: &[&str],
    setup: impl FnOnce(&TempDir),
    prompt: &str,
) {
    let home = TempDir::new(&format!("sj_{name}_home"));
    let cwd = TempDir::new(&format!("sj_{name}_cwd"));
    setup(&cwd);
    let server = MockServer::start(replies).await;
    let addr = server.addr().to_string();
    let (stdout, stderr, _code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new(name, &home, &cwd)
            .args(["-p", "--output-format", "stream-json", "--session-id", SESSION])
            .args(extra_args.iter().copied())
            .mock("mock-model", &server)
            .arg(prompt)
            .run_output()
    })
    .await
    .expect("timed out");
    let got = normalise(
        &stdout,
        &[
            (cwd.path().display().to_string(), "<cwd>"),
            (home.path().display().to_string(), "<home>"),
            (addr, "<mock>"),
        ],
    );
    assert!(got.contains(r#""type":"result""#), "{name}: no result line\nstdout: {stdout}\nstderr: {stderr}");
    let path = fixtures_dir().join(format!("{name}.ndjson"));
    if std::env::var_os("ZEN_UPDATE_FIXTURES").is_some() {
        std::fs::create_dir_all(fixtures_dir()).unwrap();
        std::fs::write(&path, &got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (run with ZEN_UPDATE_FIXTURES=1 to create it)", path.display()));
    assert_eq!(
        got, want,
        "{name}: stream-json output differs from {}. If the change is intended, update \
         docs/stream-json-v1.md and rerun with ZEN_UPDATE_FIXTURES=1.",
        path.display()
    );
}

const BARE: &[&str] = &["--bare", "--setting-sources", ""];

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fixture_plain_text() {
    check("plain_text", vec![Reply::text("Hello from zen.")], BARE, |_| {}).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fixture_tool_call() {
    check(
        "tool_call",
        vec![tool_call("call_read_1", "Read", json!({"file_path": "notes.txt"})), Reply::text("The file says hi.")],
        BARE,
        |cwd| {
            cwd.write("notes.txt", "hi\n");
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fixture_subagent() {
    let agents = r#"{"researcher":{"description":"Research specialist","prompt":"You are a researcher.","tools":["Read"]}}"#;
    let mut args = BARE.to_vec();
    args.extend(["--permission-mode", "bypassPermissions", "--agents", agents]);
    check(
        "subagent",
        vec![
            tool_call(
                "call_task_1",
                "Task",
                json!({"description": "Research", "prompt": "Find X", "subagent_type": "researcher"}),
            ),
            Reply::text("X is a thing."),
            Reply::text("The researcher says X is a thing."),
        ],
        &args,
        |_| {},
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fixture_api_retry() {
    check(
        "api_retry",
        vec![Reply::status(500, "upstream overloaded"), Reply::text("Answer after one retry.")],
        BARE,
        |_| {},
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fixture_permission_denied() {
    check(
        "permission_denied",
        vec![
            tool_call("call_write_1", "Write", json!({"file_path": "out.txt", "content": "x"})),
            Reply::text("I was not allowed to write the file."),
        ],
        BARE,
        |_| {},
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fixture_compact_boundary() {
    // `--autocompact 1` forces auto-compaction before the first request: the mock serves the
    // summarization call first, then the real answer. The prompt is long enough (>1 token) to
    // trip the threshold since only `messages` (not the system prompt) counts.
    let mut args = BARE.to_vec();
    args.extend(["--autocompact", "1"]);
    check_with_prompt(
        "compact_boundary",
        vec![Reply::text("SUMMARY: user asked to be greeted."), Reply::text("Hello after compaction.")],
        &args,
        |_| {},
        "hello, please greet me warmly and at length",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fixture_hook_message() {
    // A PreToolUse hook emitting {"systemMessage": ...} surfaces as a
    // system/hook_message line in stream-json (docs/hooks.md).
    check(
        "hook_message",
        vec![
            tool_call("call_read_1", "Read", json!({"file_path": "notes.txt"})),
            Reply::text("The file says hi."),
        ],
        &["--setting-sources", "project"],
        |cwd| {
            cwd.write("notes.txt", "hi\n");
            cwd.write(
                ".zen/settings.json",
                r#"{"hooks": {"PreToolUse": [{"matcher": "Read", "hooks": [{"type": "command", "command": "printf '{\"systemMessage\": \"hook says hi\"}'", "timeout": 5}]}]}}"#,
            );
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fixture_truncation_note() {
    check(
        "truncation_note",
        vec![cut(Reply::text("First part, ")), cut(Reply::text("second part")), Reply::text("never sent")],
        &["--setting-sources", "project"],
        |cwd| {
            cwd.write(".zen/settings.json", r#"{"maxContinuations": 1}"#);
        },
    )
    .await;
}

/// Every record type, subtype and top-level key in the fixtures is named in the doc (`like_this`).
#[test]
fn doc_names_every_field_in_the_fixtures() {
    let doc = std::fs::read_to_string(doc_path()).expect("docs/stream-json-v1.md");
    let mut missing = Vec::new();
    let entries = std::fs::read_dir(fixtures_dir()).expect("fixtures dir");
    let mut seen = 0;
    for e in entries {
        let p = e.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) != Some("ndjson") {
            continue;
        }
        seen += 1;
        for line in std::fs::read_to_string(&p).unwrap().lines() {
            let v: Value = serde_json::from_str(line).unwrap();
            let o = v.as_object().unwrap();
            let mut names: Vec<String> = o.keys().cloned().collect();
            for k in ["type", "subtype"] {
                if let Some(s) = o.get(k).and_then(|x| x.as_str()) {
                    names.push(s.to_string());
                }
            }
            for n in names {
                if !doc.contains(&format!("`{n}`")) && !missing.contains(&n) {
                    missing.push(n);
                }
            }
        }
    }
    assert_eq!(seen, 8, "expected 8 fixture streams");
    assert!(missing.is_empty(), "docs/stream-json-v1.md does not mention: {missing:?}");
}
