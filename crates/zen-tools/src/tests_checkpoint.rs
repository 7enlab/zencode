


use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use zen_core::{Tool, ToolContext};

use crate::builtin::{EditTool, MultiEditTool, ReadTool, WriteTool};
use crate::checkpoint::Checkpoints;

fn tmpdir(tag: &str) -> PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let d = std::env::temp_dir().join(format!("zen_tools_ckpt_{tag}_{}_{n}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}



async fn scripted_turn(dir: &std::path::Path, ctx: &ToolContext) -> Vec<String> {
    let calls: Vec<(&dyn Tool, serde_json::Value)> = vec![
        (&ReadTool, json!({"file_path": "a.txt"})),
        (
            &EditTool,
            json!({"file_path": "a.txt", "old_string": "alpha", "new_string": "ALPHA"}),
        ),
        (
            &MultiEditTool,
            json!({"file_path": "a.txt", "edits": [{"old_string": "beta", "new_string": "BETA"}]}),
        ),
        (
            &WriteTool,
            json!({"file_path": "sub/b.txt", "content": "brand new\n"}),
        ),
    ];
    let mut out = Vec::new();
    for (tool, args) in calls {
        let r = tool.call(args, ctx).await;
        assert!(!r.is_error, "{}", r.content);
        let canon = crate::canonicalize_lenient(dir).display().to_string();
        out.push(format!(
            "{}|{}",
            r.is_error,
            r.content.replace(&canon, "<dir>")
        ));
    }
    out
}

#[tokio::test]
async fn p0_3_tools_checkpoint_then_undo_restores() {
    let dir = tmpdir("undo");
    std::fs::write(dir.join("a.txt"), "alpha\nbeta\n").unwrap();
    let store = Arc::new(Checkpoints::new(dir.join(".ck")));
    let mut ctx = ToolContext::new(dir.clone(), "p0-3");
    ctx.checkpoints = Some(store.clone());
    store.start_turn("edit a, write b");
    scripted_turn(&dir, &ctx).await;
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        "ALPHA\nBETA\n"
    );

    let (turn, diff) = store.diff_last_turn(&dir).unwrap();
    assert_eq!(
        turn.entries.len(),
        2,
        "a.txt once (first snapshot kept), b.txt once"
    );
    assert!(diff.contains("-alpha\n-beta\n+ALPHA\n+BETA\n"), "{diff}");
    assert!(diff.contains("+brand new\n"), "{diff}");

    let r = store.undo(false).unwrap();
    assert_eq!((r.restored.len(), r.removed.len()), (1, 1));
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        "alpha\nbeta\n"
    );
    assert!(!dir.join("sub/b.txt").exists());


    let e = EditTool
        .call(
            json!({"file_path": "a.txt", "old_string": "alpha", "new_string": "x"}),
            &ctx,
        )
        .await;
    assert!(
        e.is_error && e.content.contains("changed on disk"),
        "{}",
        e.content
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn p0_3_checkpoints_do_not_change_tool_output() {
    let (plain, tracked) = (tmpdir("plain"), tmpdir("tracked"));
    let mut outputs = Vec::new();
    for (dir, on) in [(&plain, false), (&tracked, true)] {
        std::fs::write(dir.join("a.txt"), "alpha\nbeta\n").unwrap();
        let mut ctx = ToolContext::new(dir.clone(), "p0-3");
        if on {
            let store = Arc::new(Checkpoints::new(
                std::env::temp_dir().join(format!("zen_ckpt_store_{}", std::process::id())),
            ));
            store.start_turn("t");
            ctx.checkpoints = Some(store);
        }
        outputs.push(scripted_turn(dir, &ctx).await);
    }
    assert_eq!(
        outputs[0], outputs[1],
        "byte-identical tool results with and without checkpoints"
    );
    for d in [plain, tracked] {
        let _ = std::fs::remove_dir_all(d);
    }
}
