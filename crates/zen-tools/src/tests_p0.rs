

use serde_json::json;
use zen_core::{Tool, ToolContext};

use crate::builtin::GrepTool;

fn ctx_with_file(name: &str, body: &str) -> ToolContext {
    let dir = std::env::temp_dir().join(format!("zen_tools_p0_{}", uuid_like()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(name), body).unwrap();
    ToolContext::new(dir, "p0")
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{}_{}", std::process::id(), n)
}

#[tokio::test]
async fn p0_5_grep_invalid_regex_falls_back_to_literal_with_note() {
    let ctx = ctx_with_file("a.txt", "call foo(bar) here\nother line\n");
    let out = GrepTool
        .call(
            json!({"pattern": "foo(bar", "output_mode": "content"}),
            &ctx,
        )
        .await;
    assert!(!out.is_error, "{}", out.content);
    assert!(
        out.content.contains("not a valid regex") && out.content.contains("literal"),
        "{}",
        out.content
    );
    assert!(
        out.content.contains("call foo(bar) here"),
        "{}",
        out.content
    );
    assert!(!out.content.contains("other line"), "{}", out.content);
}

#[tokio::test]
async fn p0_5_grep_valid_regex_has_no_note() {
    let ctx = ctx_with_file("a.txt", "fn alpha() {}\n");
    let out = GrepTool
        .call(
            json!({"pattern": "fn [a-z]+", "output_mode": "content"}),
            &ctx,
        )
        .await;
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("fn alpha"), "{}", out.content);
    assert!(!out.content.contains("[zen:"), "{}", out.content);
}

#[tokio::test]
async fn p0_5_grep_invalid_regex_without_literal_hits_is_still_an_error() {
    let ctx = ctx_with_file("a.txt", "nothing relevant\n");
    let out = GrepTool.call(json!({"pattern": "foo(bar"}), &ctx).await;
    assert!(out.is_error, "{}", out.content);
    assert!(out.content.contains("invalid regex"), "{}", out.content);
}
