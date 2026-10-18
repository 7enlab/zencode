//! `@file` mentions and `!cmd` shell passthrough for the REPL (parity P5).
//!
//! * `@<glob>` — file mentions are expanded to their contents before the prompt is sent.
//!   `@./src/main.rs`, `@src/**/*.test.ts`; multiple mentions per prompt; `.gitignore`
//!   is respected; a pattern that matches nothing is a hard error naming the pattern.
//!   Expansion over the token budget only warns (settings `mentions.tokenBudget`,
//!   default 10 000).
//! * `!cmd` — shell passthrough runs the command directly (no model round-trip) and
//!   appends stdout/stderr to the conversation as a `shell_passthrough` record. It runs
//!   under the current permission mode: the same check the agent applies to the `Bash`
//!   tool (`Allow` → run, `Ask` → prompt, `Deny`/plan mode → refuse).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Default token budget for `@` mention expansion (settings `mentions.tokenBudget`).
pub const DEFAULT_MENTION_TOKEN_BUDGET: u64 = 10_000;
/// Per-file safety rail for mention expansion: files larger than this are truncated.
pub const MAX_MENTION_FILE_BYTES: u64 = 512 * 1024;
/// Binary sniffing: a NUL byte in the first bytes marks a file as binary.
const BINARY_SNIFF_BYTES: usize = 8000;

/// `{"mentions": {"tokenBudget": n}}` → n; anything else → the default.
pub fn mention_token_budget(v: Option<&serde_json::Value>) -> u64 {
    v.and_then(|v| v.get("tokenBudget"))
        .and_then(|n| n.as_u64())
        .unwrap_or(DEFAULT_MENTION_TOKEN_BUDGET)
}

/// One `@pattern` occurrence in the input, with byte offsets for replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mention {
    pub pattern: String,
    /// Byte offset of the `@`.
    pub start: usize,
    /// Byte offset just past the (punctuation-stripped) token.
    pub end: usize,
}

/// Characters that can never end a file path; stripped from the end of a mention token
/// so `see @src/main.rs, thanks` mentions `src/main.rs` rather than `src/main.rs,`.
fn strip_trailing_punct(token: &str) -> &str {
    token.trim_end_matches([',', ';', ':', '!', '?', ')', ']', '}', '"', '\''])
}

/// True when the token looks like a file path (as opposed to e.g. `@username`).
fn looks_like_path(token: &str) -> bool {
    token.contains('/')
        || token.contains('\\')
        || token.contains('*')
        || token.contains('?')
        || token.contains('[')
        || token.contains('.')
        || token.starts_with("./")
        || token.starts_with("../")
}

/// Find `@` mentions: `@` at the start of the input or right after whitespace, followed
/// by a path-like token (or the name of a file/directory that exists in `cwd`, so
/// `@Makefile` works). `user@example.com` is not a mention (the `@` is mid-word); `@john` alone is
/// not one either (not path-like, no such file).
pub fn find_mentions(input: &str, cwd: &Path) -> Vec<Mention> {
    let bytes = input.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'@' && (i == 0 || bytes[i - 1].is_ascii_whitespace()) {
            let token_start = i + 1;
            let mut token_end = token_start;
            while token_end < bytes.len() && !bytes[token_end].is_ascii_whitespace() {
                token_end += 1;
            }
            let raw = &input[token_start..token_end];
            let token = strip_trailing_punct(raw);
            let joined = cwd.join(token);
            if !token.is_empty() && (looks_like_path(token) || joined.is_file() || joined.is_dir()) {
                out.push(Mention {
                    pattern: token.to_string(),
                    start: i,
                    end: token_start + token.len(),
                });
            }
            i = token_end;
        } else {
            i += 1;
        }
    }
    out
}

/// One expanded file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpandedFile {
    /// Path as shown to the model (relative to cwd with `/` separators when possible).
    pub path: String,
    /// Estimated tokens (chars/4, rounded up).
    pub tokens: u64,
}

/// Result of expanding all `@` mentions in a prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expansion {
    /// The prompt with every `@pattern` replaced by file content blocks.
    pub prompt: String,
    /// Files that were included, in mention order.
    pub files: Vec<ExpandedFile>,
    /// Non-fatal notices (e.g. token budget exceeded).
    pub warnings: Vec<String>,
}

/// A `@pattern` matched no files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoMatchError(pub String);

impl std::fmt::Display for NoMatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "@{} matched no files", self.0)
    }
}

impl std::error::Error for NoMatchError {}

fn estimate_tokens(text: &str) -> u64 {
    (text.len() as u64).div_ceil(4)
}

fn has_glob_meta(pattern: &str) -> bool {
    pattern.contains('*') || pattern.contains('?') || pattern.contains('[') || pattern.contains('{')
}

/// Normalize a mention pattern: `\` → `/`, strip a leading `./`.
fn normalize_pattern(pattern: &str) -> String {
    let p = pattern.replace('\\', "/");
    p.strip_prefix("./").unwrap_or(&p).to_string()
}

/// Display path for a matched file: relative to cwd with `/` separators when possible.
fn display_path(path: &Path, cwd: &Path) -> String {
    match path.strip_prefix(cwd) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => path.to_string_lossy().replace('\\', "/"),
    }
}

/// Read a file for mention expansion: UTF-8 lossy, binary files become a placeholder,
/// huge files are truncated (safety rail).
fn read_mention_file(path: &Path) -> String {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return format!("[could not read file: {}]", e),
    };
    if bytes.iter().take(BINARY_SNIFF_BYTES).any(|b| *b == 0) {
        return "[binary file — contents not included]".to_string();
    }
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if bytes.len() as u64 > MAX_MENTION_FILE_BYTES {
        let keep = MAX_MENTION_FILE_BYTES as usize;
        // Truncate on a char boundary.
        let mut end = keep.min(text.len());
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n[truncated: file exceeds the 512 KiB mention cap]");
    }
    text
}

/// Expand one pattern to a sorted list of absolute file paths.
fn expand_pattern(pattern: &str, cwd: &Path) -> Result<Vec<PathBuf>, NoMatchError> {
    let norm = normalize_pattern(pattern);
    let ignore_rules = zen_tools::gitignore::load_ignore_rules(cwd);

    if !has_glob_meta(&norm) {
        // Literal path.
        let abs = if Path::new(&norm).is_absolute() {
            PathBuf::from(&norm)
        } else {
            cwd.join(&norm)
        };
        if abs.is_dir() {
            return Err(NoMatchError(format!(
                "{} is a directory, not a file — use a glob like {}/**/*",
                pattern,
                pattern.trim_end_matches('/')
            )));
        }
        if !abs.is_file() {
            return Err(NoMatchError(pattern.to_string()));
        }
        if zen_tools::gitignore::is_ignored(&ignore_rules, &abs, false) {
            return Err(NoMatchError(format!(
                "{} is ignored by .gitignore",
                pattern
            )));
        }
        return Ok(vec![abs]);
    }

    // Glob: walk the tree, match relative paths, respect .gitignore (prune ignored dirs).
    let matcher = globset::GlobBuilder::new(&norm)
        .literal_separator(true)
        .build()
        .map(|g| g.compile_matcher())
        .map_err(|_| NoMatchError(pattern.to_string()))?;
    let mut hits = Vec::new();
    let walker = walkdir::WalkDir::new(cwd).into_iter().filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        if name == ".git" {
            return false;
        }
        // Prune ignored directories so `**` doesn't descend into e.g. node_modules.
        if e.file_type().is_dir() && e.depth() > 0 {
            return !zen_tools::gitignore::is_ignored(&ignore_rules, e.path(), true);
        }
        true
    });
    for entry in walker.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(rel) = path.strip_prefix(cwd) else { continue };
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if !matcher.is_match(&rel_str) {
            continue;
        }
        if zen_tools::gitignore::is_ignored(&ignore_rules, path, false) {
            continue;
        }
        hits.push(path.to_path_buf());
    }
    if hits.is_empty() {
        return Err(NoMatchError(pattern.to_string()));
    }
    hits.sort();
    Ok(hits)
}

/// Expand every `@pattern` in `input` against `cwd`.
///
/// Each mention is replaced inline by `<file>` blocks with a path header and token
/// count. A pattern matching nothing aborts with [`NoMatchError`] naming the pattern;
/// exceeding `token_budget` only warns (expansion still proceeds).
pub fn expand_mentions(
    input: &str,
    cwd: &Path,
    token_budget: u64,
) -> Result<Expansion, NoMatchError> {
    let mentions = find_mentions(input, cwd);
    if mentions.is_empty() {
        return Ok(Expansion {
            prompt: input.to_string(),
            files: Vec::new(),
            warnings: Vec::new(),
        });
    }
    let mut prompt = String::with_capacity(input.len());
    let mut files = Vec::new();
    let mut warnings = Vec::new();
    let mut total_tokens: u64 = 0;
    let mut cursor = 0;
    for m in &mentions {
        prompt.push_str(&input[cursor..m.start]);
        let paths = expand_pattern(&m.pattern, cwd)?;
        for path in paths {
            let content = read_mention_file(&path);
            let tokens = estimate_tokens(&content);
            total_tokens += tokens;
            let disp = display_path(&path, cwd);
            prompt.push_str(&format!(
                "<file path=\"{}\" tokens=\"~{}\">\n{}</file>",
                disp, tokens, content
            ));
            files.push(ExpandedFile { path: disp, tokens });
        }
        cursor = m.end;
    }
    prompt.push_str(&input[cursor..]);
    if total_tokens > token_budget {
        warnings.push(format!(
            "@ mention expansion used ~{} tokens (budget {}); narrow the pattern or raise mentions.tokenBudget",
            total_tokens, token_budget
        ));
    }
    Ok(Expansion { prompt, files, warnings })
}

// ── `!` shell passthrough ──────────────────────────────────────────────────

/// Outcome of the permission check for a `!cmd` passthrough.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellDecision {
    Run,
    Denied(String),
}

/// Permission verdict for `!cmd`, mirroring the agent's own `Bash` tool flow
/// (zen-agent `prepare_call`): deny rules and plan mode refuse up front, `Ask`
/// goes through the REPL prompter (yes / always / no), `bypassPermissions` runs.
pub fn decide_shell(
    permissions: &Arc<dyn zen_core::PermissionChecker>,
    prompter: Option<&Arc<dyn zen_core::Prompter>>,
    ctx: &zen_core::ToolContext,
    cmd: &str,
) -> ShellDecision {
    let input = serde_json::json!({ "command": cmd });
    match permissions.check("Bash", &input, false, ctx) {
        zen_core::PermissionDecision::Allow => ShellDecision::Run,
        zen_core::PermissionDecision::Deny(reason) => {
            ShellDecision::Denied(format!("denied: {}", reason))
        }
        zen_core::PermissionDecision::Ask => match prompter {
            Some(p) => match p.ask_permission("Bash", &input) {
                zen_core::PromptAnswer::Yes => ShellDecision::Run,
                zen_core::PromptAnswer::Always(rules) => {
                    for rule in rules.lines() {
                        permissions.add_allow_rule(rule);
                    }
                    ShellDecision::Run
                }
                zen_core::PromptAnswer::No => {
                    ShellDecision::Denied("denied by user".to_string())
                }
            },
            None => ShellDecision::Denied(
                "permission required for shell passthrough in non-interactive mode".to_string(),
            ),
        },
    }
}

/// Captured output of a passthrough shell command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellOutput {
    /// stdout + stderr, as printed.
    pub combined: String,
    pub exit_code: i32,
    pub success: bool,
}

/// Run the command with the resolved shell (portable: Git Bash on Windows), in `cwd`,
/// with the configured extra env. No permission check — call [`decide_shell`] first.
pub fn run_shell_command(cmd: &str, cwd: &Path, env: &HashMap<String, String>) -> ShellOutput {
    let sh = zen_core::resolve_shell("bash");
    let mut command = std::process::Command::new(&sh.program);
    command.args(sh.args(cmd)).current_dir(cwd);
    for (k, v) in env {
        command.env(k, v);
    }
    match command.output() {
        Ok(out) => {
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            ShellOutput {
                combined,
                exit_code: out.status.code().unwrap_or(-1),
                success: out.status.success(),
            }
        }
        Err(e) => ShellOutput {
            combined: format!("failed to run command: {}\n", e),
            exit_code: -1,
            success: false,
        },
    }
}

/// The `shell_passthrough` record appended to the conversation after `!cmd` runs.
pub fn shell_passthrough_record(cmd: &str, out: &ShellOutput) -> String {
    format!(
        "[shell_passthrough `!{}` — exit {}]\n{}",
        cmd,
        out.exit_code,
        out.combined.trim_end()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "zen_mentions_{}_{}_{}",
            tag,
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(dir: &Path, rel: &str, content: &str) -> PathBuf {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
        p
    }

    // ── find_mentions ──

    #[test]
    fn find_single_mention() {
        let cwd = tmpdir("find1");
        let ms = find_mentions("review @src/main.rs please", &cwd);
        assert_eq!(ms.len(), 1);
        assert_eq!(ms[0].pattern, "src/main.rs");
    }

    #[test]
    fn find_multiple_mentions() {
        let cwd = tmpdir("find2");
        let ms = find_mentions("@a.rs and @b.rs", &cwd);
        assert_eq!(ms.len(), 2);
        assert_eq!(ms[0].pattern, "a.rs");
        assert_eq!(ms[1].pattern, "b.rs");
    }

    #[test]
    fn find_ignores_email() {
        let cwd = tmpdir("find3");
        assert!(find_mentions("contact user@example.com", &cwd).is_empty());
    }

    #[test]
    fn find_ignores_bare_username() {
        let cwd = tmpdir("find4");
        // `@john` is not path-like and no such file exists → not a mention.
        assert!(find_mentions("ask @john about it", &cwd).is_empty());
    }

    #[test]
    fn find_bare_filename_that_exists() {
        let cwd = tmpdir("find5");
        write(&cwd, "Makefile", "all:\n");
        let ms = find_mentions("explain @Makefile", &cwd);
        assert_eq!(ms.len(), 1);
        assert_eq!(ms[0].pattern, "Makefile");
    }

    #[test]
    fn find_strips_trailing_punctuation() {
        let cwd = tmpdir("find6");
        let ms = find_mentions("see @src/main.rs, thanks", &cwd);
        assert_eq!(ms.len(), 1);
        assert_eq!(ms[0].pattern, "src/main.rs");
        // Replacement offsets must not swallow the comma.
        assert_eq!(&"see @src/main.rs, thanks"[ms[0].end..], ", thanks");
    }

    // ── expand_mentions ──

    #[test]
    fn expand_literal_file() {
        let cwd = tmpdir("exp1");
        write(&cwd, "src/main.rs", "fn main() {}\n");
        let exp = expand_mentions("review @src/main.rs", &cwd, DEFAULT_MENTION_TOKEN_BUDGET).unwrap();
        assert!(exp.prompt.contains("<file path=\"src/main.rs\""));
        assert!(exp.prompt.contains("fn main() {}"));
        assert!(exp.prompt.contains("</file>"));
        assert_eq!(exp.files.len(), 1);
        assert_eq!(exp.files[0].path, "src/main.rs");
        assert!(exp.files[0].tokens > 0);
        assert!(exp.warnings.is_empty());
    }

    #[test]
    fn expand_glob_multi() {
        let cwd = tmpdir("exp2");
        write(&cwd, "a.test.ts", "t1");
        write(&cwd, "sub/b.test.ts", "t2");
        write(&cwd, "c.ts", "nope");
        let exp =
            expand_mentions("check @**/*.test.ts", &cwd, DEFAULT_MENTION_TOKEN_BUDGET).unwrap();
        assert_eq!(exp.files.len(), 2);
        assert!(exp.prompt.contains("a.test.ts"));
        assert!(exp.prompt.contains("sub/b.test.ts"));
        assert!(exp.files.iter().all(|f| f.path != "c.ts"));
    }

    #[test]
    fn expand_missing_is_clear_error() {
        let cwd = tmpdir("exp3");
        let err = expand_mentions("look @nope/*.rs", &cwd, DEFAULT_MENTION_TOKEN_BUDGET).unwrap_err();
        assert_eq!(err, NoMatchError("nope/*.rs".to_string()));
        assert_eq!(err.to_string(), "@nope/*.rs matched no files");
    }

    #[test]
    fn expand_respects_gitignore() {
        let cwd = tmpdir("exp4");
        write(&cwd, ".gitignore", "dist/\n");
        write(&cwd, "dist/bundle.js", "ignored");
        write(&cwd, "src/app.js", "kept");
        let exp =
            expand_mentions("check @**/*.js", &cwd, DEFAULT_MENTION_TOKEN_BUDGET).unwrap();
        assert_eq!(exp.files.len(), 1);
        assert_eq!(exp.files[0].path, "src/app.js");
    }

    #[test]
    fn expand_token_budget_warns() {
        let cwd = tmpdir("exp5");
        write(&cwd, "big.txt", &"x".repeat(4000)); // ~1000 tokens
        let exp = expand_mentions("read @big.txt", &cwd, 10).unwrap();
        assert_eq!(exp.warnings.len(), 1);
        assert!(exp.warnings[0].contains("budget"));
        // Warns but still expands.
        assert!(exp.prompt.contains("big.txt"));
    }

    #[test]
    fn expand_binary_placeholder() {
        let cwd = tmpdir("exp6");
        let p = write(&cwd, "blob.bin", "");
        std::fs::write(&p, [0u8, 1, 2, 3, 0, 5]).unwrap();
        let exp = expand_mentions("check @blob.bin", &cwd, DEFAULT_MENTION_TOKEN_BUDGET).unwrap();
        assert!(exp.prompt.contains("binary file"));
        assert!(!exp.prompt.contains('\0'));
    }

    #[test]
    fn expand_directory_is_clear_error() {
        let cwd = tmpdir("exp7");
        std::fs::create_dir_all(cwd.join("src")).unwrap();
        let err = expand_mentions("check @src", &cwd, DEFAULT_MENTION_TOKEN_BUDGET).unwrap_err();
        assert!(err.to_string().contains("directory"));
    }

    #[test]
    fn expand_no_mentions_passthrough() {
        let cwd = tmpdir("exp8");
        let exp = expand_mentions("plain prompt", &cwd, DEFAULT_MENTION_TOKEN_BUDGET).unwrap();
        assert_eq!(exp.prompt, "plain prompt");
        assert!(exp.files.is_empty());
    }

    // ── mention_token_budget ──

    #[test]
    fn budget_config_parsing() {
        assert_eq!(mention_token_budget(None), DEFAULT_MENTION_TOKEN_BUDGET);
        assert_eq!(
            mention_token_budget(Some(&serde_json::json!({"tokenBudget": 500}))),
            500
        );
        assert_eq!(
            mention_token_budget(Some(&serde_json::json!({"tokenBudget": "lots"}))),
            DEFAULT_MENTION_TOKEN_BUDGET
        );
        assert_eq!(
            mention_token_budget(Some(&serde_json::json!({}))),
            DEFAULT_MENTION_TOKEN_BUDGET
        );
    }

    // ── shell passthrough ──

    struct StubChecker {
        decision: zen_core::PermissionDecision,
        added: Mutex<Vec<String>>,
    }
    impl zen_core::PermissionChecker for StubChecker {
        fn check(
            &self,
            _tool: &str,
            _input: &serde_json::Value,
            _ro: bool,
            _ctx: &zen_core::ToolContext,
        ) -> zen_core::PermissionDecision {
            self.decision.clone()
        }
        fn add_allow_rule(&self, rule: &str) {
            self.added.lock().unwrap().push(rule.to_string());
        }
    }

    struct StubPrompter(zen_core::PromptAnswer);
    impl zen_core::Prompter for StubPrompter {
        fn ask_permission(
            &self,
            _tool: &str,
            _input: &serde_json::Value,
        ) -> zen_core::PromptAnswer {
            self.0.clone()
        }
    }

    fn test_ctx(cwd: &Path) -> zen_core::ToolContext {
        zen_core::ToolContext::new(cwd.to_path_buf(), "test-session")
    }

    #[test]
    fn shell_deny_refuses() {
        let cwd = tmpdir("sh1");
        let perms: Arc<dyn zen_core::PermissionChecker> = Arc::new(StubChecker {
            decision: zen_core::PermissionDecision::Deny("plan mode".into()),
            added: Mutex::new(vec![]),
        });
        let d = decide_shell(&perms, None, &test_ctx(&cwd), "rm -rf /");
        assert_eq!(d, ShellDecision::Denied("denied: plan mode".to_string()));
    }

    #[test]
    fn shell_allow_runs() {
        let cwd = tmpdir("sh2");
        let perms: Arc<dyn zen_core::PermissionChecker> = Arc::new(StubChecker {
            decision: zen_core::PermissionDecision::Allow,
            added: Mutex::new(vec![]),
        });
        let d = decide_shell(&perms, None, &test_ctx(&cwd), "echo hi");
        assert_eq!(d, ShellDecision::Run);
    }

    #[test]
    fn shell_ask_no_denies() {
        let cwd = tmpdir("sh3");
        let perms: Arc<dyn zen_core::PermissionChecker> = Arc::new(StubChecker {
            decision: zen_core::PermissionDecision::Ask,
            added: Mutex::new(vec![]),
        });
        let prompter: Arc<dyn zen_core::Prompter> = Arc::new(StubPrompter(zen_core::PromptAnswer::No));
        let d = decide_shell(&perms, Some(&prompter), &test_ctx(&cwd), "echo hi");
        assert_eq!(d, ShellDecision::Denied("denied by user".to_string()));
    }

    #[test]
    fn shell_ask_yes_runs() {
        let cwd = tmpdir("sh4");
        let perms: Arc<dyn zen_core::PermissionChecker> = Arc::new(StubChecker {
            decision: zen_core::PermissionDecision::Ask,
            added: Mutex::new(vec![]),
        });
        let prompter: Arc<dyn zen_core::Prompter> =
            Arc::new(StubPrompter(zen_core::PromptAnswer::Yes));
        let d = decide_shell(&perms, Some(&prompter), &test_ctx(&cwd), "echo hi");
        assert_eq!(d, ShellDecision::Run);
    }

    #[test]
    fn shell_ask_without_prompter_denies() {
        let cwd = tmpdir("sh5");
        let perms: Arc<dyn zen_core::PermissionChecker> = Arc::new(StubChecker {
            decision: zen_core::PermissionDecision::Ask,
            added: Mutex::new(vec![]),
        });
        let d = decide_shell(&perms, None, &test_ctx(&cwd), "echo hi");
        assert!(matches!(d, ShellDecision::Denied(_)));
    }

    #[test]
    fn shell_output_capture() {
        let cwd = tmpdir("sh6");
        let out = run_shell_command("echo hello", &cwd, &HashMap::new());
        assert!(out.success);
        assert_eq!(out.exit_code, 0);
        assert!(out.combined.contains("hello"));
        let rec = shell_passthrough_record("echo hello", &out);
        assert!(rec.starts_with("[shell_passthrough `!echo hello` — exit 0]"));
        assert!(rec.contains("hello"));
    }

    #[test]
    fn shell_failure_captured() {
        let cwd = tmpdir("sh7");
        let out = run_shell_command("exit 3", &cwd, &HashMap::new());
        assert!(!out.success);
        assert_eq!(out.exit_code, 3);
    }

    /// `!` then `@` compose: shell output lands in the messages and the next prompt
    /// carries both the passthrough record and the expanded file.
    #[test]
    fn shell_then_mention_compose() {
        let cwd = tmpdir("sh8");
        write(&cwd, "note.txt", "file-content");
        let out = run_shell_command("echo shell-content", &cwd, &HashMap::new());
        assert!(out.success);
        let mut messages = vec![zen_core::ChatMessage::user(shell_passthrough_record(
            "echo shell-content",
            &out,
        ))];
        let exp = expand_mentions("summarize @note.txt", &cwd, DEFAULT_MENTION_TOKEN_BUDGET).unwrap();
        messages.push(zen_core::ChatMessage::user(exp.prompt));
        let all: String = messages
            .iter()
            .filter_map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains("shell-content"));
        assert!(all.contains("file-content"));
        assert!(all.contains("shell_passthrough"));
    }

    #[test]
    fn plan_mode_checker_denies_shell() {
        // End-to-end with the real RulePermissions: plan mode must refuse `!cmd`.
        let cwd = tmpdir("sh9");
        let settings = zen_core::PermissionSettings::default();
        let perms: Arc<dyn zen_core::PermissionChecker> = Arc::new(
            zen_tools::permissions::RulePermissions::new(
                zen_core::PermissionMode::Plan,
                &settings,
                vec![],
                vec![],
                None,
            ),
        );
        let d = decide_shell(&perms, None, &test_ctx(&cwd), "echo hi");
        assert!(matches!(d, ShellDecision::Denied(_)), "plan mode must deny, got {:?}", d);
    }

    #[test]
    fn bypass_mode_checker_allows_shell() {
        let cwd = tmpdir("sh10");
        let settings = zen_core::PermissionSettings::default();
        let perms: Arc<dyn zen_core::PermissionChecker> = Arc::new(
            zen_tools::permissions::RulePermissions::new(
                zen_core::PermissionMode::BypassPermissions,
                &settings,
                vec![],
                vec![],
                None,
            ),
        );
        let d = decide_shell(&perms, None, &test_ctx(&cwd), "echo hi");
        assert_eq!(d, ShellDecision::Run);
    }
}
