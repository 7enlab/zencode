//! Loop guard and todo focus (r7, research item #5).
//!
//! Weak models tend to get stuck: editing the same file over and over, or repeating a call that
//! fails the same way. [`TurnGuard`] watches tool results within one user turn and adds a single
//! nudge to the result that crosses a threshold. Separately, a non-empty todo list is shown to
//! the model again every [`TODO_REINJECT_EVERY`] model calls and after compaction, so a long turn
//! does not lose the plan.
//!
//! The done-guard (roadmap v0.7.0 #9) adds a declared task scope: [`TurnGuard::set_scope`]
//! installs glob patterns (relative to the working directory, see [`TurnGuard::set_cwd`]);
//! a successful file mutation outside the scope produces one nudge per file per turn telling
//! the model to stop and confirm with the user instead of expanding scope on its own.

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use zen_core::{glob_match, TodoItem, ToolOutput};

/// Successful edits to one file in one turn before the guard nudges (on the Nth edit).
pub const EDIT_REPEAT_LIMIT: usize = 4;
/// Identical failing calls (same tool, same arguments, same error) before the guard nudges.
pub const FAILURE_REPEAT_LIMIT: usize = 3;
/// Model calls between todo re-injections (when the list is non-empty).
pub const TODO_REINJECT_EVERY: usize = 6;

const EDIT_TOOLS: &[&str] = &["Edit", "MultiEdit", "Write"];

/// Per-turn counters. Each nudge fires at most once per file / per repeated failure.
#[derive(Default)]
pub struct TurnGuard {
    edits: HashMap<String, usize>,
    failures: HashMap<String, usize>,
    nudged: HashSet<String>,
    /// Declared task scope (done-guard): glob patterns relative to [`TurnGuard::cwd`].
    /// Empty = no declared scope, no scope nudges.
    scope: Vec<String>,
    /// Working directory the scope patterns are relative to.
    cwd: PathBuf,
}

impl TurnGuard {
    /// Install the declared task scope: glob patterns (`*`, `**`, `?`) relative to the
    /// working directory set with [`TurnGuard::set_cwd`]. Mutations outside it nudge.
    pub fn set_scope(&mut self, scope: Vec<String>) {
        self.scope = scope;
    }

    /// Working directory the scope patterns (and absolute `file_path` arguments) resolve
    /// against.
    pub fn set_cwd(&mut self, cwd: PathBuf) {
        self.cwd = cwd;
    }

    /// `file_path` normalized to a `/`-separated path relative to the working directory,
    /// so scope globs match regardless of whether the tool call used an absolute path.
    fn normalize(&self, file: &str) -> String {
        let p = Path::new(file);
        let rel = if p.is_absolute() && !self.cwd.as_os_str().is_empty() {
            p.strip_prefix(&self.cwd).map(Path::to_path_buf).unwrap_or_else(|_| p.to_path_buf())
        } else {
            p.to_path_buf()
        };
        rel.to_string_lossy().replace('\\', "/")
    }

    /// One nudge per file per turn when a successful mutation lands outside the scope.
    fn check_scope(&mut self, tool: &str, args: &Value) -> Option<String> {
        if self.scope.is_empty() || !EDIT_TOOLS.contains(&tool) {
            return None;
        }
        let file = args.get("file_path").and_then(|v| v.as_str())?;
        let rel = self.normalize(file);
        if self.scope.iter().any(|p| glob_match(p, &rel)) {
            return None;
        }
        if self.nudged.insert(format!("scope:{rel}")) {
            Some(format!(
                "[zen: `{rel}` is outside your declared task scope ({}). Stop and ask the user to confirm \
                 before continuing -- do not expand scope on your own.]",
                self.scope.join(", ")
            ))
        } else {
            None
        }
    }

    /// Record one finished tool call; returns a note to append to its result when a threshold
    /// is reached for the first time this turn.
    pub fn observe(&mut self, tool: &str, args: &Value, out: &ToolOutput) -> Option<String> {
        if out.is_error {
            let key = format!("{}\u{0}{}\u{0}{}", tool, args, out.content);
            let n = self.failures.entry(key.clone()).or_default();
            *n += 1;
            if *n >= FAILURE_REPEAT_LIMIT && self.nudged.insert(format!("fail:{}", key)) {
                return Some(format!(
                    "[zen: this exact {} call has now failed {} times with the same error. Do not send it again. \
                     Re-read the relevant file or output, say what is actually wrong, and try a different approach.]",
                    tool, n
                ));
            }
            return None;
        }
        // Done-guard scope check: successful file mutations outside the declared scope.
        if let Some(note) = self.check_scope(tool, args) {
            return Some(note);
        }
        if !EDIT_TOOLS.contains(&tool) {
            return None;
        }
        let file = args.get("file_path").and_then(|v| v.as_str())?.to_string();
        let n = self.edits.entry(file.clone()).or_default();
        *n += 1;
        if *n >= EDIT_REPEAT_LIMIT && self.nudged.insert(format!("edit:{}", file)) {
            return Some(format!(
                "[zen: you have changed {} {} times in this turn. Stop editing it for a moment: re-read the file, \
                 state what is still wrong, and consider whether another approach would be simpler.]",
                file, n
            ));
        }
        None
    }
}

/// The todo list as a short reminder block, or `None` when it is empty.
pub fn todo_reminder(todos: &[TodoItem]) -> Option<String> {
    if todos.is_empty() {
        return None;
    }
    let mut s = String::from("[zen: current todo list. Keep working through it and update it with TodoWrite as items finish.]\n");
    for t in todos {
        let mark = match t.status.as_str() {
            "completed" => "[x]",
            "in_progress" => "[~]",
            _ => "[ ]",
        };
        s.push_str(&format!("{} {}\n", mark, t.content));
    }
    Some(s.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ok() -> ToolOutput {
        ToolOutput::ok("fine")
    }

    #[test]
    fn fourth_edit_to_one_file_nudges_once() {
        let mut g = TurnGuard::default();
        let a = json!({"file_path": "a.rs"});
        let notes: Vec<Option<String>> = (0..6).map(|_| g.observe("Edit", &a, &ok())).collect();
        assert!(notes[..3].iter().all(Option::is_none));
        assert!(notes[3].as_deref().unwrap().contains("a.rs 4 times"));
        assert!(notes[4..].iter().all(Option::is_none), "only one nudge per file");
        // Other files and read-only tools are counted separately / not at all.
        assert!(g.observe("MultiEdit", &json!({"file_path": "b.rs"}), &ok()).is_none());
        for _ in 0..5 {
            assert!(g.observe("Read", &a, &ok()).is_none());
        }
    }

    #[test]
    fn third_identical_failure_nudges_but_different_errors_do_not() {
        let mut g = TurnGuard::default();
        let cmd = json!({"command": "cargo test"});
        let fail = ToolOutput::err("exit 101");
        assert!(g.observe("Bash", &cmd, &fail).is_none());
        assert!(g.observe("Bash", &cmd, &ToolOutput::err("exit 1")).is_none());
        assert!(g.observe("Bash", &cmd, &fail).is_none());
        assert!(g.observe("Bash", &cmd, &fail).unwrap().contains("failed 3 times"));
        assert!(g.observe("Bash", &cmd, &fail).is_none());
    }

    #[test]
    fn scope_nudge_fires_once_per_file_outside_scope() {
        let mut g = TurnGuard::default();
        g.set_cwd(PathBuf::from("/repo"));
        g.set_scope(vec!["src/**".into(), "README.md".into()]);
        let in_scope = json!({"file_path": "src/main.rs"});
        let out_scope = json!({"file_path": "other/x.rs"});
        assert!(g.observe("Edit", &in_scope, &ok()).is_none(), "in-scope edit is quiet");
        let n = g.observe("Write", &out_scope, &ok()).expect("out-of-scope write nudges");
        assert!(n.contains("outside your declared task scope") && n.contains("other/x.rs"), "{n}");
        assert!(g.observe("Edit", &out_scope, &ok()).is_none(), "one nudge per file per turn");
        // Absolute paths under cwd are normalized before matching.
        let abs_out = json!({"file_path": "/repo/other/y.rs"});
        assert!(g.observe("Edit", &abs_out, &ok()).unwrap().contains("other/y.rs"));
        let abs_in = json!({"file_path": "/repo/src/z.rs"});
        assert!(g.observe("Edit", &abs_in, &ok()).is_none(), "absolute in-scope path is quiet");
    }

    #[test]
    fn no_scope_means_no_scope_nudge() {
        let mut g = TurnGuard::default();
        assert!(g.observe("Edit", &json!({"file_path": "anywhere.rs"}), &ok()).is_none());
        assert!(g.observe("Write", &json!({"file_path": "elsewhere.rs"}), &ok()).is_none());
    }

    #[test]
    fn scope_check_ignores_failed_calls_and_reads() {
        let mut g = TurnGuard::default();
        g.set_cwd(PathBuf::from("/repo"));
        g.set_scope(vec!["src/**".into()]);
        let f = json!({"file_path": "other/x.rs"});
        assert!(g.observe("Edit", &f, &ToolOutput::err("denied")).is_none(), "failed calls do not nudge");
        assert!(g.observe("Read", &f, &ok()).is_none(), "reads never nudge");
    }

    #[test]
    fn todo_reminder_renders_status_and_skips_empty() {
        assert_eq!(todo_reminder(&[]), None);
        let items = vec![
            TodoItem { content: "write test".into(), status: "completed".into(), active_form: None },
            TodoItem { content: "fix bug".into(), status: "in_progress".into(), active_form: None },
            TodoItem { content: "docs".into(), status: "pending".into(), active_form: None },
        ];
        let r = todo_reminder(&items).unwrap();
        assert!(r.ends_with("[x] write test\n[~] fix bug\n[ ] docs"), "{}", r);
    }
}
