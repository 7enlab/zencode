use serde_json::json;
use std::path::{Path, PathBuf};
use zen_core::{Tool, ToolContext};

use crate::atomic::TEMP_PREFIX;
use crate::builtin::{EditTool, MultiEditTool, ReadTool, WriteTool};

fn tmpdir(tag: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let d = std::env::temp_dir().join(format!(
        "zen_tools_atomic_{}_{}_{}",
        tag,
        std::process::id(),
        n
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn no_temp_files(dir: &Path) {
    let left: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(TEMP_PREFIX))
        .collect();
    assert!(left.is_empty(), "temp files left behind: {:?}", left);
}

#[tokio::test]
async fn p0_1_write_edit_multiedit_leave_no_temp_files() {
    let dir = tmpdir("tools");
    let ctx = ToolContext::new(dir.clone(), "p0-1");
    let w = WriteTool
        .call(json!({"file_path": "a.txt", "content": "one\ntwo\n"}), &ctx)
        .await;
    assert!(!w.is_error, "{}", w.content);
    let e = EditTool
        .call(
            json!({"file_path": "a.txt", "old_string": "one", "new_string": "uno"}),
            &ctx,
        )
        .await;
    assert!(!e.is_error, "{}", e.content);
    let m = MultiEditTool
        .call(
            json!({"file_path": "a.txt", "edits": [
                {"old_string": "uno", "new_string": "1"},
                {"old_string": "two", "new_string": "2"}
            ]}),
            &ctx,
        )
        .await;
    assert!(!m.is_error, "{}", m.content);
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        "1\n2\n"
    );
    no_temp_files(&dir);
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[tokio::test]
async fn p0_1_edit_keeps_mode_and_symlink() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmpdir("modelink");
    let real = dir.join("run.sh");
    std::fs::write(&real, "#!/bin/sh\necho old\n").unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("run.sh", dir.join("link.sh")).unwrap();
    let ctx = ToolContext::new(dir.clone(), "p0-1");
    let r = ReadTool.call(json!({"file_path": "link.sh"}), &ctx).await;
    assert!(!r.is_error, "{}", r.content);
    let e = EditTool
        .call(
            json!({"file_path": "link.sh", "old_string": "old", "new_string": "new"}),
            &ctx,
        )
        .await;
    assert!(!e.is_error, "{}", e.content);
    assert!(std::fs::symlink_metadata(dir.join("link.sh"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        std::fs::read_to_string(&real).unwrap(),
        "#!/bin/sh\necho new\n"
    );
    assert_eq!(
        std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
        0o755
    );
    no_temp_files(&dir);
    let _ = std::fs::remove_dir_all(&dir);
}
