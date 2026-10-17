//! Tests for feat/p0b in zen-tools: spilled long output, background jobs.

use serde_json::json;
use std::path::PathBuf;
use zen_core::{Tool, ToolContext};

use crate::builtin::{spill_output, BashTool, BASH_MAX_CHARS};

fn tmp_dir(tag: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let d = std::env::temp_dir().join(format!("zen_tools_p0b_{}_{}_{}", tag, std::process::id(), n));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn ctx(tag: &str) -> ToolContext {
    let d = tmp_dir(tag);
    let mut c = ToolContext::new(d.clone(), format!("p0b-{}", tag));
    c.session_dir = Some(d.join("session"));
    c
}

/// The path printed in a spill notice.
fn spill_path(reply: &str) -> PathBuf {
    let start = reply.find("Full output saved to ").expect(reply) + "Full output saved to ".len();
    let rest = &reply[start..];
    let end = rest.find(" (lines 1-").expect(reply);
    PathBuf::from(&rest[..end])
}

#[test]
fn p0b_1_short_output_is_untouched() {
    let c = ctx("short");
    assert_eq!(spill_output("hello\nworld", 100, &c), "hello\nworld");
    assert!(!c.session_dir.as_ref().unwrap().join("tool-output").exists());
}

#[test]
fn p0b_1_long_output_spills_full_text_with_counts() {
    let mut c = ctx("long");
    c.current_tool_use_id = Some("call/1:x".into());
    let text: String = (1..=5000).map(|i| format!("line {i}\n")).collect();
    let reply = spill_output(&text, 1000, &c);
    let path = spill_path(&reply);
    assert!(path.starts_with(c.session_dir.as_ref().unwrap().join("tool-output")), "{}", path.display());
    assert_eq!(path.file_name().unwrap(), "call_1_x.txt");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "file holds the exact full output");
    assert!(reply.starts_with("line 1\nline 2\n"), "head kept");
    assert!(reply.ends_with("line 5000\n"), "tail kept");
    assert!(reply.contains(&format!("{} bytes", text.len())), "{reply}");
    assert!(reply.contains("5000 lines"), "{reply}");
    assert!(reply.contains("Read with offset/limit or Grep"), "{reply}");
    // The middle is not in the reply, only in the file.
    assert!(!reply.contains("line 2500\n"));
    assert!(reply.chars().count() < 1000 + 400, "reply stays near the cap");
    // A second spill with the same call id does not overwrite the first file.
    let reply2 = spill_output(&text, 1000, &c);
    assert_ne!(spill_path(&reply2), path);
}

#[test]
fn p0b_1_no_session_dir_falls_back_to_temp() {
    let c = ToolContext::new(tmp_dir("nosess"), "p0b nosess/id");
    let text = "x".repeat(500);
    let reply = spill_output(&text, 100, &c);
    let path = spill_path(&reply);
    assert!(path.starts_with(std::env::temp_dir().join("zen-p0b_nosess_id")), "{}", path.display());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
}

#[cfg(unix)]
#[tokio::test]
async fn p0b_1_bash_long_output_is_spilled_not_lost() {
    let mut c = ctx("bash");
    c.current_tool_use_id = Some("tc_bash".into());
    let out = BashTool.call(json!({"command": "seq 1 20000"}), &c).await;
    assert!(!out.is_error, "{}", out.content);
    let path = spill_path(&out.content);
    let full = std::fs::read_to_string(&path).unwrap();
    let expect: Vec<String> = (1..=20000).map(|i| i.to_string()).collect();
    assert_eq!(full, expect.join("\n"), "middle lines are recoverable");
    assert!(out.content.contains("20000 lines"), "{}", &out.content[..200.min(out.content.len())]);
    assert!(out.content.chars().count() < BASH_MAX_CHARS + 400);
}

// ───────────── p0b 2: background jobs ─────────────

use crate::jobs::{JobKillTool, JobOutputTool};

/// The job id in a Bash background reply.
fn job_id(reply: &str) -> String {
    let start = reply.find("background job ").expect(reply) + "background job ".len();
    reply[start..].split_whitespace().next().unwrap().to_string()
}

#[cfg(unix)]
#[tokio::test]
async fn p0b_2_background_job_reads_incrementally_and_reports_exit() {
    let c = ctx("job");
    let out = BashTool
        .call(json!({"command": "echo one; echo err >&2; sleep 0.4; echo two; exit 3", "background": true}), &c)
        .await;
    assert!(!out.is_error, "{}", out.content);
    let id = job_id(&out.content);
    assert!(id.starts_with("job-"), "{id}");
    assert!(out.content.contains(&c.session_dir.as_ref().unwrap().join("jobs").display().to_string()), "{}", out.content);

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let first = JobOutputTool.call(json!({"job_id": id}), &c).await;
    assert!(!first.is_error, "{}", first.content);
    assert!(first.content.lines().next().unwrap().ends_with("running"), "{}", first.content);
    // The status line repeats the command, so look only at the output part.
    let body = |s: &str| s.split_once("New output").map(|(_, b)| b.to_string()).unwrap_or_default();
    let b1 = body(&first.content);
    assert!(b1.contains("one") && b1.contains("err"), "stdout+stderr: {}", first.content);
    assert!(!b1.contains("two"), "{}", first.content);

    let second = JobOutputTool.call(json!({"job_id": id, "wait_ms": 5000}), &c).await;
    assert!(second.content.contains("exited with code 3"), "{}", second.content);
    let b2 = body(&second.content);
    assert!(b2.contains("two"), "{}", second.content);
    assert!(!b2.contains("one"), "only new output: {}", second.content);

    let third = JobOutputTool.call(json!({"job_id": id}), &c).await;
    assert!(third.content.contains("no new output"), "{}", third.content);
}

#[cfg(unix)]
#[tokio::test]
async fn p0b_2_job_kill_stops_the_whole_tree() {
    let c = ctx("kill");
    let marker = c.cwd().join("still-alive");
    // A child that would write the marker after 1 s unless the whole group is killed.
    let cmd = format!("(sleep 1; touch '{}') & sleep 30", marker.display());
    let out = BashTool.call(json!({"command": cmd, "run_in_background": true}), &c).await;
    let id = job_id(&out.content);
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let t0 = std::time::Instant::now();
    let k = JobKillTool.call(json!({"job_id": id}), &c).await;
    assert!(!k.is_error, "{}", k.content);
    assert!(k.content.contains("killed"), "{}", k.content);
    assert!(t0.elapsed() < std::time::Duration::from_secs(5));
    let st = JobOutputTool.call(json!({"job_id": id}), &c).await;
    assert!(st.content.contains("killed"), "{}", st.content);
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    assert!(!marker.exists(), "child of the job survived the kill");
}

#[tokio::test]
async fn p0b_2_unknown_job_and_missing_id_are_errors() {
    let c = ctx("unknown");
    let o = JobOutputTool.call(json!({"job_id": "job-999999"}), &c).await;
    assert!(o.is_error && o.content.contains("no background job 'job-999999'"), "{}", o.content);
    let k = JobKillTool.call(json!({"job_id": "nope"}), &c).await;
    assert!(k.is_error, "{}", k.content);
    let m = JobOutputTool.call(json!({}), &c).await;
    assert!(m.is_error && m.content.contains("job_id"), "{}", m.content);
}

#[cfg(unix)]
#[tokio::test]
async fn p0b_2_bash_default_timeout_comes_from_context() {
    let mut c = ctx("bto");
    c.tool_timeout_ms = Some(200);
    let t0 = std::time::Instant::now();
    let out = BashTool.call(json!({"command": "sleep 5"}), &c).await;
    assert!(out.is_error && out.content.contains("timed out after 200 ms"), "{}", out.content);
    assert!(t0.elapsed() < std::time::Duration::from_secs(3));
    // An explicit timeout_ms still wins.
    let out = BashTool.call(json!({"command": "sleep 0.3; echo ok", "timeout_ms": 5000}), &c).await;
    assert_eq!(out.content, "ok");
}
