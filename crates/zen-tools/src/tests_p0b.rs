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
