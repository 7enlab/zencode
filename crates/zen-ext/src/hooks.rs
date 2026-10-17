//! Command hooks (SPEC §13, parity checklist Appendix C3).
//!
//! Settings shape (C3.1):
//! ```json
//! {"PreToolUse": [{"matcher": "Edit|Write", "hooks": [
//!     {"type": "command", "command": "./check.sh", "timeout": 60},
//!     {"type": "command", "command": "python3", "args": ["lint.py", "--fast"]}
//! ]}]}
//! ```
//! - `command` alone runs through `bash -c`; with `args` the program is exec'd directly (no shell).
//! - `timeout` is in seconds (default 60). `onFailure: "block"` (P2) turns a timeout/spawn failure
//!   into a block instead of a non-blocking error.
//! - `disableAllHooks: true` in settings is applied by the caller (zen-cli passes empty hooks) or
//!   via [`CommandHooks::with_disabled`].
//!
//! Matcher (C3.3): empty/absent/`*` matches all; a matcher made only of `[A-Za-z0-9_\- ,|]` is a
//! list of exact names split on `|` or `,`; anything else is an unanchored regex (invalid → no match).
//!
//! Matching commands of one event run concurrently; identical command+args are run once (dedupe).
//! stdin = event payload + {session_id, transcript_path, cwd, hook_event_name, permission_mode} (C3.4).
//! Env = settings `env` + `ZEN_PROJECT_DIR` (C3.14).
//!
//! Exit codes (C3.5):
//! - 0 → stdout parsed as JSON when it is a JSON object; otherwise plain stdout is context only for
//!   UserPromptSubmit/SessionStart.
//! - 2 → block (stderr = reason) for PreToolUse, UserPromptSubmit, Stop, SubagentStop, PreCompact,
//!   PermissionRequest; PostToolUse/PostToolUseFailure → stderr fed to the model, not a block;
//!   other events → non-blocking error (stderr to the user only).
//! - other → non-blocking error (stderr to the user only).
//! - timeout → process killed, output discarded, never blocks unless `onFailure: "block"` (C3.12).
//!
//! JSON output (C3.6–C3.11): `continue:false` + `stopReason`, `systemMessage`, `suppressOutput`
//! (accepted, no-op), `decision:"block"` + `reason`, and `hookSpecificOutput` with
//! `permissionDecision`/`permissionDecisionReason`/`updatedInput` (PreToolUse),
//! `updatedToolOutput` (PostToolUse) and `additionalContext` (Stop/SubagentStop: continue with it
//! as feedback). Hook output fed back is capped at 10 000 chars per hook (C3.13).
//!
//! Combining several hooks: any block → blocked; any `continue:false` → stop; permission
//! precedence deny > ask > allow; first updatedInput/updatedToolOutput in config order wins;
//! feedback and system messages are joined with newlines.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use zen_core::{HookEvent, HookOutcome, HookRunner, PermissionDecision, PermissionMode};

/// Cap for hook output that is fed back to the model or user (C3.13).
pub const HOOK_OUTPUT_LIMIT: usize = 10_000;

/// Runs `type: command` hooks from settings `hooks`.
pub struct CommandHooks {
    pub hooks: Value,
    pub session_id: String,
    pub transcript_path: String,
    pub cwd: PathBuf,
    pub permission_mode: PermissionMode,
    /// `disableAllHooks: true` → nothing runs.
    pub disabled: bool,
    /// Settings `env`, exported to every hook process.
    pub env: HashMap<String, String>,
}

/// What to do when a hook times out or cannot be spawned (`onFailure`, P2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnFailure {
    /// Non-blocking error, logged to stderr (default).
    #[default]
    Ignore,
    /// Treat the failure as a block with the error as the reason.
    Block,
}

/// One command hook parsed from settings.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandHookSpec {
    pub matcher: Option<String>,
    pub command: String,
    /// When set, `command` is exec'd directly with these arguments (no shell).
    pub args: Option<Vec<String>>,
    pub timeout_secs: u64,
    pub on_failure: OnFailure,
}

impl CommandHooks {
    pub fn new(
        hooks: Value,
        session_id: impl Into<String>,
        transcript_path: impl Into<String>,
        cwd: PathBuf,
        permission_mode: PermissionMode,
    ) -> Self {
        Self {
            hooks,
            session_id: session_id.into(),
            transcript_path: transcript_path.into(),
            cwd,
            permission_mode,
            disabled: false,
            env: HashMap::new(),
        }
    }

    /// Builder: `disableAllHooks`.
    pub fn with_disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Builder: settings `env` exported to hook processes.
    pub fn with_env(mut self, env: HashMap<String, String>) -> Self {
        self.env = env;
        self
    }

    /// The command specs whose matcher matches `matcher_target` for `event`, in config order,
    /// with identical command+args deduplicated.
    pub fn matching_specs(&self, event: HookEvent, matcher_target: &str) -> Vec<CommandHookSpec> {
        if self.disabled {
            return vec![];
        }
        let Some(entries) = self.hooks.get(event.as_str()).and_then(|v| v.as_array()) else {
            return vec![];
        };
        let mut out: Vec<CommandHookSpec> = Vec::new();
        for entry in entries {
            let matcher = entry.get("matcher").and_then(|m| m.as_str());
            if !matches_target(matcher, matcher_target) {
                continue;
            }
            let Some(hooks) = entry.get("hooks").and_then(|v| v.as_array()) else {
                continue;
            };
            for h in hooks {
                if h.get("type").and_then(|t| t.as_str()) != Some("command") {
                    continue;
                }
                let Some(command) = h.get("command").and_then(|c| c.as_str()) else {
                    continue;
                };
                let args = h.get("args").and_then(|v| v.as_array()).map(|arr| {
                    arr.iter()
                        .filter_map(|a| a.as_str().map(str::to_string))
                        .collect::<Vec<_>>()
                });
                let timeout_secs = h
                    .get("timeout")
                    .and_then(|t| t.as_u64())
                    .filter(|t| *t > 0)
                    .unwrap_or(60);
                let on_failure = match h.get("onFailure").and_then(|v| v.as_str()) {
                    Some(s) if s.eq_ignore_ascii_case("block") => OnFailure::Block,
                    _ => OnFailure::Ignore,
                };
                if out.iter().any(|s| s.command == command && s.args == args) {
                    continue; // dedupe identical handlers
                }
                out.push(CommandHookSpec {
                    matcher: matcher.map(str::to_string),
                    command: command.to_string(),
                    args,
                    timeout_secs,
                    on_failure,
                });
            }
        }
        out
    }

    fn build_stdin(&self, event: HookEvent, payload: &Value) -> String {
        let mut obj = match payload {
            Value::Object(m) => m.clone(),
            _ => serde_json::Map::new(),
        };
        obj.insert("session_id".into(), json!(self.session_id));
        obj.insert("transcript_path".into(), json!(self.transcript_path));
        obj.insert("cwd".into(), json!(self.cwd.display().to_string()));
        obj.insert("hook_event_name".into(), json!(event.as_str()));
        obj.insert("permission_mode".into(), json!(self.permission_mode.as_str()));
        Value::Object(obj).to_string()
    }
}

/// Does `matcher` match `target`? (C3.3)
pub fn matches_target(matcher: Option<&str>, target: &str) -> bool {
    let Some(m) = matcher.map(str::trim) else {
        return true;
    };
    if m.is_empty() || m == "*" {
        return true;
    }
    let is_name_list = m
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ' ' | ',' | '|'));
    if is_name_list {
        return m
            .split(['|', ','])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .any(|name| name == target);
    }
    regex::Regex::new(m).map(|re| re.is_match(target)).unwrap_or(false)
}

/// Truncate to [`HOOK_OUTPUT_LIMIT`] chars (char-safe) with a note (C3.13).
fn cap_output(s: &str) -> String {
    let count = s.chars().count();
    if count <= HOOK_OUTPUT_LIMIT {
        return s.to_string();
    }
    let head: String = s.chars().take(HOOK_OUTPUT_LIMIT).collect();
    format!("{}\n[hook output truncated: {} of {} chars shown]", head, HOOK_OUTPUT_LIMIT, count)
}

/// Events where exit code 2 blocks (C3.5; PreCompact/PermissionRequest also block in zen).
fn exit2_blocks(event: HookEvent) -> bool {
    matches!(
        event,
        HookEvent::PreToolUse
            | HookEvent::UserPromptSubmit
            | HookEvent::Stop
            | HookEvent::SubagentStop
            | HookEvent::PreCompact
            | HookEvent::PermissionRequest
    )
}

fn decision_rank(d: &PermissionDecision) -> u8 {
    match d {
        PermissionDecision::Deny(_) => 2,
        PermissionDecision::Ask => 1,
        PermissionDecision::Allow => 0,
    }
}

/// Result of running a single command hook.
#[derive(Debug, Clone, PartialEq, Default)]
struct SingleOutcome {
    blocked: bool,
    feedback: Option<String>,
    permission_override: Option<PermissionDecision>,
    stop_run: bool,
    stop_reason: Option<String>,
    updated_input: Option<Value>,
    updated_output: Option<String>,
    system_message: Option<String>,
    /// Non-blocking error text (stderr to the user only, never fed back).
    logged_error: Option<String>,
}

fn push_feedback(out: &mut SingleOutcome, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    out.feedback = Some(match out.feedback.take() {
        Some(prev) if !prev.trim().is_empty() => format!("{}\n{}", prev, text),
        _ => text.to_string(),
    });
}

#[async_trait]
impl HookRunner for CommandHooks {
    async fn run(&self, event: HookEvent, matcher_target: &str, payload: Value) -> HookOutcome {
        let specs = self.matching_specs(event, matcher_target);
        if specs.is_empty() {
            return HookOutcome::default();
        }
        let stdin_json = self.build_stdin(event, &payload);
        let results = futures::future::join_all(
            specs
                .iter()
                .map(|spec| run_one(spec, &stdin_json, &self.cwd, &self.env, event)),
        )
        .await;

        let mut outcome = HookOutcome::default();
        let mut feedback_parts: Vec<String> = Vec::new();
        let mut system_parts: Vec<String> = Vec::new();
        for r in results {
            outcome.blocked |= r.blocked;
            outcome.stop_run |= r.stop_run;
            if outcome.stop_reason.is_none() {
                outcome.stop_reason = r.stop_reason;
            }
            if let Some(po) = r.permission_override {
                let stronger = outcome
                    .permission_override
                    .as_ref()
                    .map(|cur| decision_rank(&po) > decision_rank(cur))
                    .unwrap_or(true);
                if stronger {
                    outcome.permission_override = Some(po);
                }
            }
            if outcome.updated_input.is_none() {
                outcome.updated_input = r.updated_input;
            }
            if outcome.updated_output.is_none() {
                outcome.updated_output = r.updated_output;
            }
            if let Some(sm) = r.system_message {
                system_parts.push(sm);
            }
            if let Some(fb) = r.feedback {
                let fb = fb.trim();
                if !fb.is_empty() {
                    feedback_parts.push(cap_output(fb));
                }
            }
            if let Some(err) = r.logged_error {
                let err = err.trim();
                if !err.is_empty() {
                    eprintln!("{} hook error: {}", event.as_str(), cap_output(err));
                }
            }
        }
        if !feedback_parts.is_empty() {
            outcome.feedback = Some(feedback_parts.join("\n"));
        }
        if !system_parts.is_empty() {
            outcome.system_message = Some(system_parts.join("\n"));
        }
        outcome
    }
}

async fn run_one(
    spec: &CommandHookSpec,
    stdin_json: &str,
    cwd: &Path,
    env: &HashMap<String, String>,
    event: HookEvent,
) -> SingleOutcome {
    let mut cmd = match &spec.args {
        Some(args) => {
            let mut c = Command::new(&spec.command);
            c.args(args);
            c
        }
        None => {
            // `bash` on Unix; on Windows the resolved Git Bash path (never the WSL launcher).
            let sh = zen_core::resolve_shell("bash");
            let mut c = Command::new(&sh.program);
            c.args(sh.args(&spec.command));
            c
        }
    };
    cmd.current_dir(cwd)
        .envs(env)
        .env("ZEN_PROJECT_DIR", cwd.display().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let failure = |msg: String| -> SingleOutcome {
        if spec.on_failure == OnFailure::Block {
            SingleOutcome {
                blocked: true,
                feedback: Some(msg),
                ..Default::default()
            }
        } else {
            SingleOutcome {
                logged_error: Some(msg),
                ..Default::default()
            }
        }
    };

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return failure(format!("failed to spawn hook '{}': {}", spec.command, e)),
    };

    if let Some(mut stdin) = child.stdin.take() {
        // A hook that exits without reading stdin causes a broken pipe; that is not an error.
        let _ = stdin.write_all(stdin_json.as_bytes()).await;
        let _ = stdin.shutdown().await;
    }

    let timeout = Duration::from_secs(spec.timeout_secs);
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        // Dropping the future drops the child; kill_on_drop kills it. Output is discarded.
        Err(_) => failure(format!(
            "hook timed out after {}s: {}",
            spec.timeout_secs, spec.command
        )),
        Ok(Err(e)) => failure(format!("hook failed to run: {}", e)),
        Ok(Ok(output)) => interpret_output(&output, event),
    }
}

fn interpret_output(output: &std::process::Output, event: HookEvent) -> SingleOutcome {
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let reason = || if stderr.trim().is_empty() { stdout.clone() } else { stderr.clone() };

    match output.status.code() {
        Some(0) => interpret_success(&stdout, event),
        Some(2) if exit2_blocks(event) => SingleOutcome {
            blocked: true,
            feedback: Some(reason()),
            ..Default::default()
        },
        Some(2) if matches!(event, HookEvent::PostToolUse | HookEvent::PostToolUseFailure) => {
            SingleOutcome {
                feedback: Some(reason()),
                ..Default::default()
            }
        }
        other => {
            let mut msg = format!(
                "exit code {}",
                other.map(|c| c.to_string()).unwrap_or_else(|| "signal".into())
            );
            let detail = reason();
            if !detail.trim().is_empty() {
                msg.push_str(": ");
                msg.push_str(detail.trim());
            }
            SingleOutcome {
                logged_error: Some(msg),
                ..Default::default()
            }
        }
    }
}

fn interpret_success(stdout: &str, event: HookEvent) -> SingleOutcome {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return SingleOutcome::default();
    }
    if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(trimmed) {
        return interpret_json_stdout(&obj, event);
    }
    // Plain text on success is additional context only for UserPromptSubmit/SessionStart.
    if matches!(event, HookEvent::UserPromptSubmit | HookEvent::SessionStart) {
        SingleOutcome {
            feedback: Some(stdout.to_string()),
            ..Default::default()
        }
    } else {
        SingleOutcome::default()
    }
}

fn interpret_json_stdout(obj: &serde_json::Map<String, Value>, event: HookEvent) -> SingleOutcome {
    let mut out = SingleOutcome::default();
    let is_stop = matches!(event, HookEvent::Stop | HookEvent::SubagentStop);

    // C3.6 universal fields. `suppressOutput` is accepted and ignored.
    if obj.get("continue").and_then(|v| v.as_bool()) == Some(false) {
        out.stop_run = true;
        out.stop_reason = obj
            .get("stopReason")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string);
    }
    if let Some(sm) = obj.get("systemMessage").and_then(|v| v.as_str()) {
        if !sm.trim().is_empty() {
            out.system_message = Some(sm.to_string());
        }
    }

    // Top-level decision (C3.8–C3.10; legacy PreToolUse "approve"/"block").
    if let Some(decision) = obj.get("decision").and_then(|v| v.as_str()) {
        let reason = obj.get("reason").and_then(|v| v.as_str()).unwrap_or("");
        if decision.eq_ignore_ascii_case("block") {
            out.blocked = true;
            push_feedback(&mut out, reason);
        } else if decision.eq_ignore_ascii_case("approve") && event == HookEvent::PreToolUse {
            out.permission_override = Some(PermissionDecision::Allow);
        }
    }

    if let Some(hso) = obj.get("hookSpecificOutput").and_then(|v| v.as_object()) {
        // C3.7 PreToolUse.
        if event == HookEvent::PreToolUse {
            if let Some(pd) = hso.get("permissionDecision").and_then(|v| v.as_str()) {
                let reason = hso
                    .get("permissionDecisionReason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                match pd.to_ascii_lowercase().as_str() {
                    "allow" => out.permission_override = Some(PermissionDecision::Allow),
                    "ask" => out.permission_override = Some(PermissionDecision::Ask),
                    "deny" => {
                        let reason = if reason.trim().is_empty() {
                            "Denied by PreToolUse hook"
                        } else {
                            reason
                        };
                        out.permission_override = Some(PermissionDecision::Deny(reason.to_string()))
                    }
                    _ => {}
                }
                // The deny reason goes to the model via the Deny decision itself; other reasons
                // are informational feedback.
                if pd.eq_ignore_ascii_case("allow") || pd.eq_ignore_ascii_case("ask") {
                    push_feedback(&mut out, reason);
                }
            }
            if let Some(ui) = hso.get("updatedInput").filter(|v| v.is_object()) {
                out.updated_input = Some(ui.clone());
            }
        }
        // C3.8 PostToolUse.
        if event == HookEvent::PostToolUse {
            if let Some(ut) = hso.get("updatedToolOutput") {
                out.updated_output = Some(match ut.as_str() {
                    Some(s) => s.to_string(),
                    None => ut.to_string(),
                });
            }
        }
        // additionalContext: context for the model; for Stop/SubagentStop it means "continue
        // with this as feedback" (C3.9).
        if let Some(ctx) = hso.get("additionalContext").and_then(|v| v.as_str()) {
            if !ctx.trim().is_empty() {
                if is_stop {
                    out.blocked = true;
                }
                push_feedback(&mut out, ctx);
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use crate::testutil::TempDir;

    fn hooks_with(event: &str, matcher: Option<&str>, command: &str, timeout: u64) -> Value {
        let matcher_val = match matcher {
            Some(m) => json!(m),
            None => Value::Null,
        };
        json!({ event: [ { "matcher": matcher_val, "hooks": [ {"type": "command", "command": command, "timeout": timeout} ] } ] })
    }

    fn runner(hooks: Value, cwd: &Path) -> CommandHooks {
        CommandHooks::new(hooks, "sess-1", "/tmp/transcript.jsonl", cwd.to_path_buf(), PermissionMode::Default)
    }

    #[tokio::test]
    async fn test_exit0_context_feedback() {
        let tmp = TempDir::new();
        let hooks = hooks_with("UserPromptSubmit", None, "echo 'extra context'", 10);
        let r = runner(hooks, tmp.path());
        let out = tokio::time::timeout(
            Duration::from_secs(10),
            r.run(HookEvent::UserPromptSubmit, "", json!({"prompt": "hi"})),
        ).await.unwrap();
        assert!(!out.blocked);
        assert_eq!(out.feedback.as_deref().map(|s| s.trim()), Some("extra context"));
    }

    #[tokio::test]
    async fn test_exit2_blocked_with_stderr() {
        let tmp = TempDir::new();
        let hooks = hooks_with("PreToolUse", Some("Bash"), "echo 'nope' >&2; exit 2", 10);
        let r = runner(hooks, tmp.path());
        let out = tokio::time::timeout(
            Duration::from_secs(10),
            r.run(HookEvent::PreToolUse, "Bash", json!({"tool_name": "Bash"})),
        ).await.unwrap();
        assert!(out.blocked);
        assert_eq!(out.feedback.as_deref().map(|s| s.trim()), Some("nope"));
    }

    #[tokio::test]
    async fn test_other_exit_non_blocking() {
        let tmp = TempDir::new();
        let hooks = hooks_with("PreToolUse", None, "echo 'oops' >&2; exit 3", 10);
        let r = runner(hooks, tmp.path());
        let out = tokio::time::timeout(
            Duration::from_secs(10),
            r.run(HookEvent::PreToolUse, "Bash", json!({})),
        ).await.unwrap();
        assert!(!out.blocked);
        assert!(out.feedback.is_none());
    }

    #[tokio::test]
    async fn test_matcher_regex_filter() {
        let tmp = TempDir::new();
        let hooks = hooks_with("SessionStart", Some("Edit|Write"), "echo matched", 10);
        let r = runner(hooks, tmp.path());

        let out_edit = tokio::time::timeout(
            Duration::from_secs(10),
            r.run(HookEvent::SessionStart, "Edit", json!({})),
        ).await.unwrap();
        assert!(out_edit.feedback.is_some());

        let out_bash = tokio::time::timeout(
            Duration::from_secs(10),
            r.run(HookEvent::SessionStart, "Bash", json!({})),
        ).await.unwrap();
        assert!(out_bash.feedback.is_none());
    }

    #[tokio::test]
    async fn test_matcher_star_matches_all() {
        let tmp = TempDir::new();
        let hooks = hooks_with("SessionStart", Some("*"), "echo matched", 10);
        let r = runner(hooks, tmp.path());
        let out = tokio::time::timeout(
            Duration::from_secs(10),
            r.run(HookEvent::SessionStart, "Anything", json!({})),
        ).await.unwrap();
        assert!(out.feedback.is_some());
    }

    #[tokio::test]
    async fn test_pretooluse_json_context_only() {
        // Plain stdout is NOT added to context for tool events.
        let tmp = TempDir::new();
        let hooks = hooks_with("PreToolUse", None, "echo plain", 10);
        let r = runner(hooks, tmp.path());
        let out = tokio::time::timeout(
            Duration::from_secs(10),
            r.run(HookEvent::PreToolUse, "Bash", json!({})),
        ).await.unwrap();
        assert!(out.feedback.is_none());
    }

    #[tokio::test]
    async fn test_json_permission_decision_deny() {
        let tmp = TempDir::new();
        let cmd = r#"printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"not allowed"}}'"#;
        let hooks = hooks_with("PreToolUse", None, cmd, 10);
        let r = runner(hooks, tmp.path());
        let out = tokio::time::timeout(
            Duration::from_secs(10),
            r.run(HookEvent::PreToolUse, "Bash", json!({})),
        ).await.unwrap();
        assert_eq!(out.permission_override, Some(PermissionDecision::Deny("not allowed".into())));
    }

    #[tokio::test]
    async fn test_json_permission_decision_allow_and_ask() {
        let tmp = TempDir::new();
        for (word, expected) in [("allow", PermissionDecision::Allow), ("ask", PermissionDecision::Ask)] {
            let cmd = format!(r#"printf '{{"hookSpecificOutput":{{"permissionDecision":"{}"}}}}'"#, word);
            let hooks = hooks_with("PreToolUse", None, &cmd, 10);
            let r = runner(hooks, tmp.path());
            let out = tokio::time::timeout(
                Duration::from_secs(10),
                r.run(HookEvent::PreToolUse, "Bash", json!({})),
            ).await.unwrap();
            assert_eq!(out.permission_override, Some(expected));
        }
    }

    #[tokio::test]
    async fn test_json_continue_false_stop_run() {
        let tmp = TempDir::new();
        let cmd = r#"printf '{"continue":false,"stopReason":"halt now"}'"#;
        let hooks = hooks_with("Stop", None, cmd, 10);
        let r = runner(hooks, tmp.path());
        let out = tokio::time::timeout(
            Duration::from_secs(10),
            r.run(HookEvent::Stop, "", json!({})),
        ).await.unwrap();
        assert!(out.stop_run);
        assert_eq!(out.stop_reason.as_deref(), Some("halt now"));
    }

    #[tokio::test]
    async fn test_json_decision_block() {
        let tmp = TempDir::new();
        let cmd = r#"printf '{"decision":"block","reason":"blocked by hook"}'"#;
        let hooks = hooks_with("PostToolUse", None, cmd, 10);
        let r = runner(hooks, tmp.path());
        let out = tokio::time::timeout(
            Duration::from_secs(10),
            r.run(HookEvent::PostToolUse, "Bash", json!({})),
        ).await.unwrap();
        assert!(out.blocked);
        assert_eq!(out.feedback.as_deref(), Some("blocked by hook"));
    }

    #[tokio::test]
    async fn test_json_additional_context() {
        let tmp = TempDir::new();
        let cmd = r#"printf '{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"project uses cargo"}}'"#;
        let hooks = hooks_with("SessionStart", None, cmd, 10);
        let r = runner(hooks, tmp.path());
        let out = tokio::time::timeout(
            Duration::from_secs(10),
            r.run(HookEvent::SessionStart, "startup", json!({})),
        ).await.unwrap();
        assert_eq!(out.feedback.as_deref(), Some("project uses cargo"));
    }

    #[tokio::test]
    async fn test_timeout_is_non_blocking() {
        let tmp = TempDir::new();
        let hooks = hooks_with("PreToolUse", None, "sleep 5", 1);
        let r = runner(hooks, tmp.path());
        let start = std::time::Instant::now();
        let out = tokio::time::timeout(
            Duration::from_secs(8),
            r.run(HookEvent::PreToolUse, "Bash", json!({})),
        ).await.unwrap();
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(!out.blocked);
        assert!(out.feedback.is_none());
    }

    #[tokio::test]
    async fn test_stdin_payload_contains_context() {
        let tmp = TempDir::new();
        let outfile = tmp.path().join("out.json");
        let cmd = format!("cat > {}", outfile.display());
        let hooks = hooks_with("UserPromptSubmit", None, &cmd, 10);
        let r = runner(hooks, tmp.path());
        let _ = tokio::time::timeout(
            Duration::from_secs(10),
            r.run(HookEvent::UserPromptSubmit, "", json!({"prompt": "hello"})),
        ).await.unwrap();

        let written = fs::read_to_string(&outfile).unwrap();
        let v: Value = serde_json::from_str(&written).unwrap();
        assert_eq!(v.get("prompt").and_then(|p| p.as_str()), Some("hello"));
        assert_eq!(v.get("session_id").and_then(|p| p.as_str()), Some("sess-1"));
        assert_eq!(v.get("hook_event_name").and_then(|p| p.as_str()), Some("UserPromptSubmit"));
        assert_eq!(v.get("permission_mode").and_then(|p| p.as_str()), Some("default"));
        assert!(v.get("cwd").is_some());
    }

    #[tokio::test]
    async fn test_env_zen_project_dir() {
        let tmp = TempDir::new();
        let outfile = tmp.path().join("env.txt");
        let cmd = format!("echo $ZEN_PROJECT_DIR > {}", outfile.display());
        let hooks = hooks_with("SessionStart", None, &cmd, 10);
        let r = runner(hooks, tmp.path());
        let _ = tokio::time::timeout(
            Duration::from_secs(10),
            r.run(HookEvent::SessionStart, "startup", json!({})),
        ).await.unwrap();
        let written = fs::read_to_string(&outfile).unwrap();
        assert!(written.trim().contains(tmp.path().to_string_lossy().as_ref()));
    }

    #[tokio::test]
    async fn test_no_hooks_for_event() {
        let tmp = TempDir::new();
        let hooks = hooks_with("Stop", None, "echo x", 10);
        let r = runner(hooks, tmp.path());
        let out = tokio::time::timeout(
            Duration::from_secs(10),
            r.run(HookEvent::SessionEnd, "", json!({})),
        ).await.unwrap();
        assert_eq!(out, HookOutcome::default());
    }

    #[test]
    fn test_matches_target_variants() {
        assert!(matches_target(None, "Bash"));
        assert!(matches_target(Some(""), "Bash"));
        assert!(matches_target(Some("*"), "Bash"));
        assert!(matches_target(Some("Edit|Write"), "Write"));
        assert!(!matches_target(Some("Edit|Write"), "Bash"));
    }

    async fn run_ev(r: &CommandHooks, ev: HookEvent, target: &str, payload: Value) -> HookOutcome {
        tokio::time::timeout(Duration::from_secs(15), r.run(ev, target, payload))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn c3_1_args_exec_form_without_shell() {
        let tmp = TempDir::new();
        let hooks = json!({"SessionStart": [{"hooks": [
            {"type": "command", "command": "printf", "args": ["%s|", "a  b", "$HOME"]}
        ]}]});
        let out = run_ev(&runner(hooks, tmp.path()), HookEvent::SessionStart, "startup", json!({})).await;
        // No shell: the argument keeps its spaces and `$HOME` is not expanded.
        assert_eq!(out.feedback.as_deref(), Some("a  b|$HOME|"));
    }

    #[tokio::test]
    async fn c3_1_disabled_runs_nothing() {
        let tmp = TempDir::new();
        let marker = tmp.path().join("ran");
        let hooks = hooks_with("PreToolUse", None, &format!("touch {}; exit 2", marker.display()), 10);
        let r = runner(hooks, tmp.path()).with_disabled(true);
        let out = run_ev(&r, HookEvent::PreToolUse, "Bash", json!({})).await;
        assert_eq!(out, HookOutcome::default());
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn c3_1_non_command_types_ignored_and_default_timeout() {
        let tmp = TempDir::new();
        let r = runner(
            json!({"Stop": [{"hooks": [
                {"type": "http", "url": "http://x"},
                {"type": "command", "command": "true"}
            ]}]}),
            tmp.path(),
        );
        let specs = r.matching_specs(HookEvent::Stop, "");
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].timeout_secs, 60);
        assert_eq!(specs[0].on_failure, OnFailure::Ignore);
    }

    #[tokio::test]
    async fn c3_14_settings_env_exported() {
        let tmp = TempDir::new();
        let outfile = tmp.path().join("env.txt");
        let hooks = hooks_with("SessionStart", None, &format!("echo \"$FOO:$ZEN_PROJECT_DIR\" > {}", outfile.display()), 10);
        let env = HashMap::from([("FOO".to_string(), "bar".to_string())]);
        let r = runner(hooks, tmp.path()).with_env(env);
        run_ev(&r, HookEvent::SessionStart, "startup", json!({})).await;
        let written = fs::read_to_string(&outfile).unwrap();
        assert_eq!(written.trim(), format!("bar:{}", tmp.path().display()));
    }

    #[test]
    fn c3_3_matcher_semantics() {
        // exact name lists
        assert!(matches_target(Some("Edit,Write"), "Write"));
        assert!(matches_target(Some("Edit | Write"), "Edit"));
        assert!(!matches_target(Some("Bash"), "BashOutput"));
        assert!(!matches_target(Some("Edit|Write"), "WriteFile"));
        // regex (unanchored)
        assert!(matches_target(Some("mcp__.*"), "mcp__srv__tool"));
        assert!(matches_target(Some("Notebook.*"), "NotebookEdit"));
        assert!(matches_target(Some("^Web"), "WebFetch"));
        assert!(!matches_target(Some("^Web"), "MyWeb"));
        // invalid regex never matches
        assert!(!matches_target(Some("(unclosed"), "(unclosed"));
    }

    #[tokio::test]
    async fn c3_5_exit2_posttooluse_feeds_model_without_block() {
        let tmp = TempDir::new();
        let hooks = hooks_with("PostToolUse", None, "echo 'lint failed' >&2; exit 2", 10);
        let out = run_ev(&runner(hooks, tmp.path()), HookEvent::PostToolUse, "Edit", json!({})).await;
        assert!(!out.blocked);
        assert_eq!(out.feedback.as_deref(), Some("lint failed"));
    }

    #[tokio::test]
    async fn c3_5_exit2_non_blockable_event_is_plain_error() {
        let tmp = TempDir::new();
        let hooks = hooks_with("SessionEnd", None, "echo 'meh' >&2; exit 2", 10);
        let out = run_ev(&runner(hooks, tmp.path()), HookEvent::SessionEnd, "", json!({})).await;
        assert_eq!(out, HookOutcome::default());
    }

    #[tokio::test]
    async fn c3_5_exit2_blocks_stop_and_precompact() {
        let tmp = TempDir::new();
        for ev in ["Stop", "SubagentStop", "PreCompact", "UserPromptSubmit"] {
            let hooks = hooks_with(ev, None, "echo 'keep going' >&2; exit 2", 10);
            let event = match ev {
                "Stop" => HookEvent::Stop,
                "SubagentStop" => HookEvent::SubagentStop,
                "PreCompact" => HookEvent::PreCompact,
                _ => HookEvent::UserPromptSubmit,
            };
            let out = run_ev(&runner(hooks, tmp.path()), event, "", json!({})).await;
            assert!(out.blocked, "{ev} should block");
            assert_eq!(out.feedback.as_deref(), Some("keep going"));
        }
    }

    #[tokio::test]
    async fn c3_5_non_object_json_stdout_is_plain_text() {
        let tmp = TempDir::new();
        let hooks = hooks_with("UserPromptSubmit", None, "echo '[1,2]'", 10);
        let out = run_ev(&runner(hooks, tmp.path()), HookEvent::UserPromptSubmit, "", json!({})).await;
        assert_eq!(out.feedback.as_deref(), Some("[1,2]"));
    }

    #[tokio::test]
    async fn c3_6_system_message_and_suppress_output() {
        let tmp = TempDir::new();
        let cmd = r#"printf '{"systemMessage":"careful","suppressOutput":true}'"#;
        let hooks = hooks_with("PostToolUse", None, cmd, 10);
        let out = run_ev(&runner(hooks, tmp.path()), HookEvent::PostToolUse, "Bash", json!({})).await;
        assert_eq!(out.system_message.as_deref(), Some("careful"));
        assert!(!out.blocked);
        assert!(out.feedback.is_none());
    }

    #[tokio::test]
    async fn c3_7_updated_input_and_deny_reason() {
        let tmp = TempDir::new();
        let cmd = r#"printf '{"hookSpecificOutput":{"permissionDecision":"allow","updatedInput":{"command":"ls -la"}}}'"#;
        let hooks = hooks_with("PreToolUse", Some("Bash"), cmd, 10);
        let out = run_ev(&runner(hooks, tmp.path()), HookEvent::PreToolUse, "Bash", json!({})).await;
        assert_eq!(out.permission_override, Some(PermissionDecision::Allow));
        assert_eq!(out.updated_input, Some(json!({"command": "ls -la"})));

        let cmd = r#"printf '{"hookSpecificOutput":{"permissionDecision":"deny"}}'"#;
        let hooks = hooks_with("PreToolUse", None, cmd, 10);
        let out = run_ev(&runner(hooks, tmp.path()), HookEvent::PreToolUse, "Bash", json!({})).await;
        assert_eq!(
            out.permission_override,
            Some(PermissionDecision::Deny("Denied by PreToolUse hook".into()))
        );
    }

    #[tokio::test]
    async fn c3_7_updated_input_ignored_outside_pretooluse() {
        let tmp = TempDir::new();
        let cmd = r#"printf '{"hookSpecificOutput":{"updatedInput":{"x":1},"permissionDecision":"deny"}}'"#;
        let hooks = hooks_with("PostToolUse", None, cmd, 10);
        let out = run_ev(&runner(hooks, tmp.path()), HookEvent::PostToolUse, "Bash", json!({})).await;
        assert!(out.updated_input.is_none());
        assert!(out.permission_override.is_none());
    }

    #[tokio::test]
    async fn c3_8_updated_tool_output_and_context() {
        let tmp = TempDir::new();
        let cmd = r#"printf '{"decision":"block","reason":"tests fail","hookSpecificOutput":{"updatedToolOutput":"[redacted]","additionalContext":"see CI"}}'"#;
        let hooks = hooks_with("PostToolUse", None, cmd, 10);
        let out = run_ev(&runner(hooks, tmp.path()), HookEvent::PostToolUse, "Bash", json!({})).await;
        assert_eq!(out.updated_output.as_deref(), Some("[redacted]"));
        assert_eq!(out.feedback.as_deref(), Some("tests fail\nsee CI"));
    }

    #[tokio::test]
    async fn c3_9_stop_block_and_additional_context_continue() {
        let tmp = TempDir::new();
        let cmd = r#"printf '{"decision":"block","reason":"run the tests first"}'"#;
        let out = run_ev(&runner(hooks_with("Stop", None, cmd, 10), tmp.path()), HookEvent::Stop, "", json!({})).await;
        assert!(out.blocked);
        assert_eq!(out.feedback.as_deref(), Some("run the tests first"));

        let cmd = r#"printf '{"hookSpecificOutput":{"additionalContext":"also update docs"}}'"#;
        let out = run_ev(&runner(hooks_with("SubagentStop", None, cmd, 10), tmp.path()), HookEvent::SubagentStop, "", json!({})).await;
        assert!(out.blocked);
        assert_eq!(out.feedback.as_deref(), Some("also update docs"));
    }

    #[tokio::test]
    async fn c3_10_user_prompt_submit_block_and_context() {
        let tmp = TempDir::new();
        let cmd = r#"printf '{"decision":"block","reason":"no secrets"}'"#;
        let out = run_ev(&runner(hooks_with("UserPromptSubmit", None, cmd, 10), tmp.path()), HookEvent::UserPromptSubmit, "", json!({})).await;
        assert!(out.blocked);
        assert_eq!(out.feedback.as_deref(), Some("no secrets"));

        let cmd = r#"printf '{"hookSpecificOutput":{"additionalContext":"today is Thursday"}}'"#;
        let out = run_ev(&runner(hooks_with("UserPromptSubmit", None, cmd, 10), tmp.path()), HookEvent::UserPromptSubmit, "", json!({})).await;
        assert!(!out.blocked);
        assert_eq!(out.feedback.as_deref(), Some("today is Thursday"));
    }

    #[tokio::test]
    async fn c3_12_timeout_discards_output_and_on_failure_block() {
        let tmp = TempDir::new();
        let hooks = hooks_with("PreToolUse", None, "echo early; echo bad >&2; sleep 5; exit 2", 1);
        let out = run_ev(&runner(hooks, tmp.path()), HookEvent::PreToolUse, "Bash", json!({})).await;
        assert_eq!(out, HookOutcome::default());

        let hooks = json!({"PreToolUse": [{"hooks": [
            {"type": "command", "command": "sleep 5", "timeout": 1, "onFailure": "block"}
        ]}]});
        let start = std::time::Instant::now();
        let out = run_ev(&runner(hooks, tmp.path()), HookEvent::PreToolUse, "Bash", json!({})).await;
        assert!(start.elapsed() < Duration::from_secs(4));
        assert!(out.blocked);
        assert!(out.feedback.unwrap().contains("timed out"));
    }

    #[tokio::test]
    async fn c3_13_output_capped() {
        let tmp = TempDir::new();
        let hooks = hooks_with("UserPromptSubmit", None, "head -c 25000 /dev/zero | tr '\\0' 'x'", 10);
        let out = run_ev(&runner(hooks, tmp.path()), HookEvent::UserPromptSubmit, "", json!({})).await;
        let fb = out.feedback.unwrap();
        assert!(fb.starts_with(&"x".repeat(HOOK_OUTPUT_LIMIT)));
        assert!(!fb.contains(&"x".repeat(HOOK_OUTPUT_LIMIT + 1)));
        assert!(fb.contains("truncated"));
    }

    #[test]
    fn c3_13_cap_is_char_safe() {
        let s = "é".repeat(HOOK_OUTPUT_LIMIT + 5);
        let capped = cap_output(&s);
        assert!(capped.starts_with(&"é".repeat(HOOK_OUTPUT_LIMIT)));
        assert!(capped.contains("truncated"));
    }

    #[tokio::test]
    async fn parallel_run_and_dedupe() {
        let tmp = TempDir::new();
        let log = tmp.path().join("log");
        let dup = format!("sleep 1; echo x >> {}", log.display());
        let hooks = json!({"PreToolUse": [
            {"matcher": "Bash", "hooks": [{"type": "command", "command": dup}, {"type": "command", "command": "sleep 1"}]},
            {"matcher": "*", "hooks": [{"type": "command", "command": dup}]}
        ]});
        let r = runner(hooks, tmp.path());
        assert_eq!(r.matching_specs(HookEvent::PreToolUse, "Bash").len(), 2);
        let start = std::time::Instant::now();
        run_ev(&r, HookEvent::PreToolUse, "Bash", json!({})).await;
        assert!(start.elapsed() < Duration::from_millis(1900), "hooks should run concurrently");
        assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), 1);
    }

    #[tokio::test]
    async fn combine_precedence_and_fields() {
        let tmp = TempDir::new();
        let hooks = json!({"PreToolUse": [{"hooks": [
            {"type": "command", "command": r#"printf '{"hookSpecificOutput":{"permissionDecision":"allow","updatedInput":{"a":1}},"systemMessage":"one"}'"#},
            {"type": "command", "command": r#"printf '{"hookSpecificOutput":{"permissionDecision":"deny","permissionDecisionReason":"policy"},"systemMessage":"two"}'"#},
            {"type": "command", "command": r#"printf '{"hookSpecificOutput":{"permissionDecision":"ask","updatedInput":{"a":2}}}'"#}
        ]}]});
        let out = run_ev(&runner(hooks, tmp.path()), HookEvent::PreToolUse, "Bash", json!({})).await;
        assert_eq!(out.permission_override, Some(PermissionDecision::Deny("policy".into())));
        assert_eq!(out.updated_input, Some(json!({"a": 1})));
        assert_eq!(out.system_message.as_deref(), Some("one\ntwo"));
    }

    #[tokio::test]
    async fn c3_4_tool_event_payload_passthrough() {
        let tmp = TempDir::new();
        let outfile = tmp.path().join("in.json");
        let hooks = hooks_with("PostToolUse", Some("Bash"), &format!("cat > {}", outfile.display()), 10);
        run_ev(
            &runner(hooks, tmp.path()),
            HookEvent::PostToolUse,
            "Bash",
            json!({"tool_name": "Bash", "tool_input": {"command": "ls"}, "tool_use_id": "t1", "tool_response": "ok"}),
        )
        .await;
        let v: Value = serde_json::from_str(&fs::read_to_string(&outfile).unwrap()).unwrap();
        assert_eq!(v["tool_name"], "Bash");
        assert_eq!(v["tool_input"]["command"], "ls");
        assert_eq!(v["tool_use_id"], "t1");
        assert_eq!(v["tool_response"], "ok");
        assert_eq!(v["hook_event_name"], "PostToolUse");
        assert_eq!(v["transcript_path"], "/tmp/transcript.jsonl");
    }
}
