//! Permission rules and modes (SPEC §5).
//!
//! Evaluation order is `deny > ask > allow > mode default`.

use crate::builtin::resolve_path;
use globset::GlobBuilder;
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use zen_core::{PermissionChecker, PermissionDecision, PermissionMode, PermissionSettings, ToolContext};

/// Tools that are always read-only regardless of arguments.
pub fn tool_is_read_only(name: &str) -> bool {
    matches!(name, "Read" | "Glob" | "Grep" | "TodoWrite" | "Task" | "Skill")
}

/// Split a rule into `(tool, specifier)`; `None` specifier means bare `Tool`.
fn split_rule(rule: &str) -> (&str, Option<&str>) {
    let rule = rule.trim();
    match rule.find('(') {
        Some(i) if rule.ends_with(')') => (&rule[..i], Some(&rule[i + 1..rule.len() - 1])),
        _ => (rule, None),
    }
}

fn path_arg(input: &Value) -> Option<&str> {
    input
        .get("file_path")
        .or_else(|| input.get("path"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

/// Paths in rules (gitignore-style, SPEC C2.7): `//abs` absolute, `~/x` home, `/x` and `./x`
/// relative to the settings root (cwd), `a/b` relative to cwd, and a bare name without a
/// slash (`.env`, `*.log`) matches at any depth below cwd.
/// Returns `(absolute pattern path, had_trailing_slash)`.
fn rule_pattern_path(cwd: &Path, spec: &str) -> (PathBuf, bool) {
    let dir_hint = spec.ends_with('/') || spec.ends_with("/**");
    let spec = spec.trim_end_matches("/**").trim_end_matches('/');
    if let Some(rest) = spec.strip_prefix("//") {
        (PathBuf::from(format!("/{}", rest)), dir_hint)
    } else if let Some(rest) = spec.strip_prefix("~/") {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
        (home.join(rest), dir_hint)
    } else if let Some(rest) = spec.strip_prefix("./").or_else(|| spec.strip_prefix('/')) {
        (cwd.join(rest), dir_hint)
    } else if !spec.contains('/') {
        (cwd.join("**").join(spec), dir_hint)
    } else {
        (cwd.join(spec), dir_hint)
    }
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn path_rule_matches(cwd: &Path, spec: &str, candidate: &str) -> bool {
    let (pattern_path, dir_hint) = rule_pattern_path(cwd, spec);
    let target = normalize(&resolve_path(cwd, candidate));
    let glob_pat = pattern_path.to_string_lossy().to_string();
    let mut globs = vec![glob_pat.clone()];
    if dir_hint {
        globs.push(format!("{}/**", glob_pat));
    }
    for pat in globs {
        if let Ok(g) = GlobBuilder::new(&pat).literal_separator(true).build() {
            if g.compile_matcher().is_match(&target) {
                return true;
            }
        }
    }
    target == pattern_path || (dir_hint && target.starts_with(&pattern_path))
}

/// Whole-string match where `*` matches any run of characters (spaces and `/` included).
fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut mark = 0usize;
    while ti < t.len() {
        if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// `Bash(...)` specifier (SPEC C2.3): `*` matches any text anywhere; a trailing ` *` (or the
/// legacy `:*`) also matches the bare command, so `ls *` matches `ls` but not `lsof`, while
/// `ls*` matches `lsof`.
fn bash_rule_matches(spec: &str, command: &str) -> bool {
    let command = command.trim();
    let spec = spec.trim();
    let spec = match spec.strip_suffix(":*") {
        Some(p) => format!("{} *", p.trim_end()),
        None => spec.to_string(),
    };
    if spec == "*" {
        return true;
    }
    if let Some(prefix) = spec.strip_suffix(" *") {
        let prefix = prefix.trim_end();
        return wildcard_match(prefix, command) || wildcard_match(&format!("{} *", prefix), command);
    }
    wildcard_match(&spec, command)
}

/// Split a shell command into sub-commands on `&&`, `||`, `;`, `|`, `|&`, `&` and newlines
/// outside quotes (SPEC C2.4). `2>&1` / `&>` redirections are not separators. Returns `None`
/// when a binary operator dangles (`ls &&`): the command then cannot be matched part-wise.
pub fn split_compound(command: &str) -> Option<Vec<String>> {
    let chars: Vec<char> = command.chars().collect();
    // (segment, operator that ended it)
    let mut segs: Vec<(String, &'static str)> = Vec::new();
    let mut cur = String::new();
    let (mut sq, mut dq) = (false, false);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '\\' && !sq {
            cur.push(c);
            if let Some(n) = next {
                cur.push(n);
            }
            i += 2;
            continue;
        }
        if c == '\'' && !dq {
            sq = !sq;
        } else if c == '"' && !sq {
            dq = !dq;
        } else if !sq && !dq {
            let prev = if i > 0 { Some(chars[i - 1]) } else { None };
            let redirect = c == '&' && (matches!(prev, Some('>') | Some('<')) || next == Some('>'));
            let op: Option<(&'static str, usize)> = match (c, next) {
                _ if redirect => None,
                ('&', Some('&')) => Some(("&&", 2)),
                ('|', Some('|')) => Some(("||", 2)),
                ('|', Some('&')) => Some(("|&", 2)),
                ('|', _) => Some(("|", 1)),
                ('&', _) => Some(("&", 1)),
                (';', _) => Some((";", 1)),
                ('\n', _) => Some(("\n", 1)),
                _ => None,
            };
            if let Some((op, len)) = op {
                segs.push((std::mem::take(&mut cur), op));
                i += len;
                continue;
            }
        }
        cur.push(c);
        i += 1;
    }
    segs.push((cur, ""));
    let binary = |op: &str| matches!(op, "&&" | "||" | "|" | "|&");
    let mut out = Vec::new();
    for (idx, (seg, op_after)) in segs.iter().enumerate() {
        let seg = seg.trim();
        if seg.is_empty() {
            let op_before = if idx > 0 { segs[idx - 1].1 } else { "" };
            if binary(op_before) || binary(op_after) {
                return None;
            }
            continue;
        }
        out.push(seg.to_string());
    }
    Some(out)
}

/// Which rule list a rule is evaluated for; deny/ask accept tool-name globs and a path-scoped
/// `Read` deny also guards Edit/Write/Grep/Glob.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RuleKind {
    Allow,
    Ask,
    Deny,
}

fn is_edit_tool(name: &str) -> bool {
    matches!(name, "Edit" | "Write" | "MultiEdit" | "NotebookEdit")
}

/// `Tool` / `Tool(specifier)` (SPEC §5, C2.3, C2.7–C2.9), evaluated as an allow rule.
pub fn rule_matches(rule: &str, tool_name: &str, input: &Value, cwd: &Path) -> bool {
    rule_matches_kind(rule, tool_name, input, cwd, RuleKind::Allow)
}

fn rule_matches_kind(rule: &str, tool_name: &str, input: &Value, cwd: &Path, kind: RuleKind) -> bool {
    let rule = rule.trim();
    if rule.is_empty() {
        return false;
    }
    if !rule.contains('(') {
        // Tool-name globs: `mcp__srv__*` anywhere; `*`, `mcp__*` etc. only in deny/ask.
        if rule.contains('*') && (rule.starts_with("mcp__") || kind != RuleKind::Allow) {
            return wildcard_match(rule, tool_name);
        }
        // MCP: bare `mcp__server` covers every `mcp__server__tool`.
        if rule.starts_with("mcp__") {
            return rule == tool_name || tool_name.starts_with(&format!("{}__", rule));
        }
    }

    let (rule_tool, spec) = split_rule(rule);
    let spec = spec.map(str::trim).filter(|s| !s.is_empty() && *s != "*");
    // Exact tool name (case-insensitive); `Edit` rules cover every edit tool; a path-scoped
    // `Read` deny also guards edits and searches of that path.
    let name_ok = rule_tool.eq_ignore_ascii_case(tool_name)
        || (rule_tool == "Edit" && is_edit_tool(tool_name))
        || (kind == RuleKind::Deny
            && spec.is_some()
            && rule_tool == "Read"
            && (is_edit_tool(tool_name) || matches!(tool_name, "Grep" | "Glob")));
    if !name_ok {
        return false;
    }
    let Some(spec) = spec else { return true };
    match rule_tool {
        "Bash" | "bash" => {
            let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
            bash_rule_matches(spec, cmd)
        }
        t if t == "Read" || is_edit_tool(t) => match path_arg(input) {
            Some(fp) => path_rule_matches(cwd, spec, fp),
            None => false,
        },
        "WebFetch" => {
            let url = input.get("url").and_then(|v| v.as_str()).unwrap_or("");
            match spec.strip_prefix("domain:") {
                Some(domain) => domain_matches(domain, url),
                None => url.contains(spec),
            }
        }
        _ => false,
    }
}

/// `WebFetch(domain:x)` (SPEC C2.8): case-insensitive hostname match; `*.x` = subdomains only;
/// `*` = every host. A leading `www.` is ignored on both sides.
fn domain_matches(domain: &str, url: &str) -> bool {
    let domain = domain.trim().to_lowercase();
    let raw = host_raw(url);
    if raw.is_empty() {
        return false;
    }
    if domain == "*" {
        return true;
    }
    if let Some(base) = domain.strip_prefix("*.") {
        return raw.ends_with(&format!(".{}", base));
    }
    host_of(url) == domain.trim_start_matches("www.")
}

/// True when a deny rule names the whole tool (`Bash`, `Bash(*)`, `mcp__srv`, `mcp__*`), so the
/// tool is removed from the list sent to the model rather than blocked per call (SPEC C2.2).
pub fn rule_removes_tool(rule: &str, tool_name: &str) -> bool {
    let rule = rule.trim();
    let (rule_tool, spec) = split_rule(rule);
    let tool_level = spec.map(|s| s.trim().is_empty() || s.trim() == "*").unwrap_or(true);
    tool_level && rule_matches_kind(rule_tool, tool_name, &Value::Null, Path::new("/"), RuleKind::Deny)
}

fn host_raw(url: &str) -> String {
    let after_scheme = url.split("://").nth(1).unwrap_or(url);
    let host_and_path = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    let host = host_and_path.split('@').next_back().unwrap_or(host_and_path);
    let host = host.split(':').next().unwrap_or(host);
    host.to_lowercase()
}

fn host_of(url: &str) -> String {
    host_raw(url).trim_start_matches("www.").to_string()
}

/// Bash commands that only create/move files and are therefore fine in `acceptEdits`
/// (every sub-command of a compound must qualify).
fn is_simple_fs_command(command: &str) -> bool {
    match split_compound(command) {
        Some(parts) if !parts.is_empty() => parts.iter().all(|p| {
            matches!(
                p.split_whitespace().next().unwrap_or(""),
                "mkdir" | "touch" | "mv" | "cp"
            )
        }),
        _ => false,
    }
}

/// Is `rule` matched by `tool`/`input` for the given list? For Bash, deny/ask match when the
/// whole command or any sub-command matches (SPEC C2.4).
fn any_rule<'a>(
    rules: &'a [String],
    tool_name: &str,
    input: &Value,
    cwd: &Path,
    kind: RuleKind,
) -> Option<&'a String> {
    let subs: Vec<Value> = match bash_parts(tool_name, input) {
        Some(Some(parts)) if parts.len() > 1 => {
            parts.into_iter().map(|p| serde_json::json!({ "command": p })).collect()
        }
        _ => Vec::new(),
    };
    rules.iter().find(|r| {
        rule_matches_kind(r, tool_name, input, cwd, kind)
            || subs.iter().any(|sub| rule_matches_kind(r, tool_name, sub, cwd, kind))
    })
}

/// Allow needs every sub-command of a compound Bash command to be covered (SPEC C2.4); a
/// command with a dangling operator can only be allowed by a tool-level rule (`Bash`).
fn allow_covers(rules: &[String], tool_name: &str, input: &Value, cwd: &Path) -> bool {
    let tool_level = |r: &String| {
        let (_, spec) = split_rule(r.trim());
        spec.map(|s| s.trim().is_empty() || s.trim() == "*").unwrap_or(true)
    };
    match bash_parts(tool_name, input) {
        None => rules.iter().any(|r| rule_matches_kind(r, tool_name, input, cwd, RuleKind::Allow)),
        Some(None) => rules
            .iter()
            .any(|r| tool_level(r) && rule_matches_kind(r, tool_name, input, cwd, RuleKind::Allow)),
        Some(Some(parts)) if parts.len() <= 1 => {
            rules.iter().any(|r| rule_matches_kind(r, tool_name, input, cwd, RuleKind::Allow))
        }
        Some(Some(parts)) => parts.iter().all(|p| {
            let sub = serde_json::json!({ "command": p });
            rules.iter().any(|r| rule_matches_kind(r, tool_name, &sub, cwd, RuleKind::Allow))
        }),
    }
}

/// `None` = not a Bash call; `Some(None)` = Bash with a dangling operator; else the parts.
fn bash_parts(tool_name: &str, input: &Value) -> Option<Option<Vec<String>>> {
    if !tool_name.eq_ignore_ascii_case("Bash") {
        return None;
    }
    let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
    Some(split_compound(cmd))
}

/// The permission engine (SPEC §5).
pub struct RulePermissions {
    pub mode: PermissionMode,
    pub allow: Vec<String>,
    pub ask: Vec<String>,
    pub deny: Vec<String>,
    pub local_settings_path: Option<PathBuf>,
    /// Rules added at runtime by "always allow".
    dynamic_allow: Mutex<Vec<String>>,
}

impl RulePermissions {
    pub fn new(
        mode: PermissionMode,
        settings: &PermissionSettings,
        cli_allow: Vec<String>,
        cli_deny: Vec<String>,
        local_settings_path: Option<PathBuf>,
    ) -> Self {
        let mut allow = settings.allow.clone();
        allow.extend(cli_allow);
        let mut deny = settings.deny.clone();
        deny.extend(cli_deny);
        Self {
            mode,
            allow,
            ask: settings.ask.clone(),
            deny,
            local_settings_path,
            dynamic_allow: Mutex::new(Vec::new()),
        }
    }

    /// Allow rules including ones learned at runtime.
    pub fn effective_allow(&self) -> Vec<String> {
        let mut v = self.allow.clone();
        v.extend(self.dynamic_allow.lock().unwrap().iter().cloned());
        v
    }

    fn working_dirs(&self, ctx: &ToolContext) -> Vec<PathBuf> {
        let mut dirs = vec![normalize(&ctx.cwd())];
        for d in &ctx.extra_dirs {
            dirs.push(normalize(d));
        }
        dirs
    }

    /// Write/Edit inside cwd or an --add-dir directory.
    fn path_inside_workdir(&self, input: &Value, ctx: &ToolContext) -> bool {
        let Some(fp) = path_arg(input) else { return false };
        let target = normalize(&resolve_path(&ctx.cwd(), fp));
        self.working_dirs(ctx).iter().any(|d| target.starts_with(d))
    }

    /// Every path argument of a mkdir/touch/mv/cp command stays inside the working dirs and
    /// the command uses no substitution or redirection (SPEC C2.11 acceptEdits).
    fn fs_args_inside_workdir(&self, command: &str, ctx: &ToolContext) -> bool {
        if command.contains(['`', '>', '<', '$']) {
            return false;
        }
        let cwd = ctx.cwd();
        let dirs = self.working_dirs(ctx);
        split_compound(command).unwrap_or_default().iter().all(|part| {
            part.split_whitespace().skip(1).filter(|a| !a.starts_with('-')).all(|arg| {
                let arg = arg.trim_matches(|c| c == '"' || c == '\'');
                let target = normalize(&resolve_path(&cwd, arg));
                dirs.iter().any(|d| target.starts_with(d))
            })
        })
    }

    fn evaluate(&self, tool_name: &str, input: &Value, read_only: bool, ctx: &ToolContext) -> PermissionDecision {
        let cwd = ctx.cwd();

        // 1. deny always wins, even in bypassPermissions.
        if let Some(rule) = any_rule(&self.deny, tool_name, input, &cwd, RuleKind::Deny) {
            return PermissionDecision::Deny(format!("Denied by rule '{}'", rule));
        }

        // 2. plan mode blocks every mutating tool up front.
        if self.mode == PermissionMode::Plan && !read_only {
            return PermissionDecision::Deny("plan mode: only read-only tools are available".into());
        }

        // 3. explicit ask rules.
        if let Some(rule) = any_rule(&self.ask, tool_name, input, &cwd, RuleKind::Ask) {
            return self.resolve_ask(format!("Ask rule '{}' matches", rule));
        }

        // 4. explicit allow rules (every sub-command of a compound Bash command).
        let allowed = allow_covers(&self.effective_allow(), tool_name, input, &cwd);

        // 5. bypassPermissions allows everything not denied above.
        if self.mode == PermissionMode::BypassPermissions {
            return PermissionDecision::Allow;
        }

        if allowed {
            return PermissionDecision::Allow;
        }

        // 6. mode default.
        if read_only || tool_is_read_only(tool_name) {
            return PermissionDecision::Allow;
        }
        match self.mode {
            PermissionMode::AcceptEdits => {
                let is_edit = is_edit_tool(tool_name);
                if is_edit && self.path_inside_workdir(input, ctx) {
                    return PermissionDecision::Allow;
                }
                if tool_name == "Bash" {
                    let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
                    if is_simple_fs_command(cmd) && self.fs_args_inside_workdir(cmd, ctx) {
                        return PermissionDecision::Allow;
                    }
                }
                PermissionDecision::Ask
            }
            PermissionMode::DontAsk => {
                PermissionDecision::Deny("dontAsk mode: permission request suppressed".into())
            }
            PermissionMode::Default | PermissionMode::Plan | PermissionMode::BypassPermissions => {
                PermissionDecision::Ask
            }
        }
    }

    fn resolve_ask(&self, reason: String) -> PermissionDecision {
        match self.mode {
            PermissionMode::DontAsk => {
                PermissionDecision::Deny(format!("{} (dontAsk mode denies prompts)", reason))
            }
            // In plan mode an explicit ask rule cannot be satisfied either.
            PermissionMode::Plan => PermissionDecision::Deny("plan mode".into()),
            PermissionMode::BypassPermissions => PermissionDecision::Allow,
            _ => PermissionDecision::Ask,
        }
    }
}

impl PermissionChecker for RulePermissions {
    fn check(&self, tool_name: &str, input: &Value, read_only: bool, ctx: &ToolContext) -> PermissionDecision {
        self.evaluate(tool_name, input, read_only, ctx)
    }

    fn ask_rule_matches(&self, tool_name: &str, input: &Value, ctx: &ToolContext) -> bool {
        any_rule(&self.ask, tool_name, input, &ctx.cwd(), RuleKind::Ask).is_some()
    }

    fn add_allow_rule(&self, rule: &str) {
        let rule = rule.trim();
        if rule.is_empty() {
            return;
        }
        {
            let mut dyn_rules = self.dynamic_allow.lock().unwrap();
            if !dyn_rules.iter().any(|r| r == rule) {
                dyn_rules.push(rule.to_string());
            }
        }
        if let Some(path) = &self.local_settings_path {
            if let Err(e) = persist_allow_rule(path, rule) {
                eprintln!("warning: could not save rule '{}' to {}: {}", rule, path.display(), e);
            }
        }
    }
}

/// Append `rule` to `permissions.allow` in a settings JSON file, preserving other keys.
pub fn persist_allow_rule(path: &Path, rule: &str) -> anyhow::Result<()> {
    let mut root: Value = match std::fs::read_to_string(path) {
        Ok(text) if !text.trim().is_empty() => serde_json::from_str(&text).unwrap_or_else(|_| Value::Object(Default::default())),
        _ => Value::Object(Default::default()),
    };
    if !root.is_object() {
        root = Value::Object(Default::default());
    }
    let obj = root.as_object_mut().unwrap();
    let perms = obj
        .entry("permissions")
        .or_insert_with(|| Value::Object(Default::default()));
    if !perms.is_object() {
        *perms = Value::Object(Default::default());
    }
    let perms = perms.as_object_mut().unwrap();
    let allow = perms
        .entry("allow")
        .or_insert_with(|| Value::Array(vec![]));
    if !allow.is_array() {
        *allow = Value::Array(vec![]);
    }
    let arr = allow.as_array_mut().unwrap();
    let already = arr.iter().any(|v| v.as_str() == Some(rule));
    if !already {
        arr.push(Value::String(rule.to_string()));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&root)?)?;
    Ok(())
}

/// Rules that are granted automatically for a tool call, used by the REPL's "always allow".
/// `Bash(npm run test)`-style rules come from the caller; this is only a fallback.
pub fn suggested_rule(tool_name: &str, input: &Value) -> Option<String> {
    match tool_name {
        "Bash" => input
            .get("command")
            .and_then(|v| v.as_str())
            .map(|c| format!("Bash({})", c)),
        "Read" | "Edit" | "MultiEdit" | "Write" => path_arg(input).map(|p| format!("{}({})", tool_name, p)),
        "WebFetch" => input
            .get("url")
            .and_then(|v| v.as_str())
            .and_then(|u| {
                let h = host_of(u);
                if h.is_empty() {
                    None
                } else {
                    Some(format!("WebFetch(domain:{})", h))
                }
            }),
        other => Some(other.to_string()),
    }
}

/// Rules for an "always allow" answer (SPEC C2.12): a compound Bash command gets one exact rule
/// per sub-command (at most 5); anything else gets [`suggested_rule`].
pub fn suggested_rules(tool_name: &str, input: &Value) -> Vec<String> {
    if tool_name == "Bash" {
        let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
        if let Some(parts) = split_compound(cmd) {
            if parts.len() > 1 {
                return dedup(parts.into_iter().map(|p| format!("Bash({})", p)).collect())
                    .into_iter()
                    .take(5)
                    .collect();
            }
        }
    }
    suggested_rule(tool_name, input).into_iter().collect()
}

/// Deduplicate while preserving order.
pub fn dedup(mut rules: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    rules.retain(|r| seen.insert(r.clone()));
    rules
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_parsing() {
        assert_eq!(host_of("https://www.example.com/a/b?x=1"), "example.com");
        assert_eq!(host_of("http://user:pw@api.example.com:8443/x"), "api.example.com");
        assert_eq!(host_of("example.com/x"), "example.com");
    }

    #[test]
    fn simple_fs_commands() {
        assert!(is_simple_fs_command("mkdir -p a/b"));
        assert!(is_simple_fs_command("touch x.txt"));
        assert!(!is_simple_fs_command("rm -rf /"));
        assert!(!is_simple_fs_command("cargo test"));
    }
}

#[cfg(test)]
mod permission_tests {
    use super::*;
    use serde_json::json;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zen-perm-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ctx_for(dir: &Path) -> ToolContext {
        ToolContext::new(dir.to_path_buf(), "perm-session")
    }

    fn perms(
        mode: PermissionMode,
        allow: &[&str],
        ask: &[&str],
        deny: &[&str],
        local: Option<PathBuf>,
    ) -> RulePermissions {
        let settings = PermissionSettings {
            allow: allow.iter().map(|s| s.to_string()).collect(),
            ask: ask.iter().map(|s| s.to_string()).collect(),
            deny: deny.iter().map(|s| s.to_string()).collect(),
            default_mode: None,
            additional_directories: vec![],
        };
        RulePermissions::new(mode, &settings, vec![], vec![], local)
    }

    fn is_allow(d: &PermissionDecision) -> bool {
        matches!(d, PermissionDecision::Allow)
    }
    fn is_ask(d: &PermissionDecision) -> bool {
        matches!(d, PermissionDecision::Ask)
    }
    fn deny_reason(d: &PermissionDecision) -> String {
        match d {
            PermissionDecision::Deny(r) => r.clone(),
            other => panic!("expected Deny, got {:?}", other),
        }
    }

    // ───────── rule forms ─────────

    #[test]
    fn rule_bare_tool_names() {
        let cwd = PathBuf::from("/tmp/work");
        assert!(rule_matches("Bash", "Bash", &json!({"command": "ls"}), &cwd));
        assert!(rule_matches("Read", "Read", &json!({"file_path": "x"}), &cwd));
        assert!(!rule_matches("Read", "Write", &json!({"file_path": "x"}), &cwd));
        assert!(rule_matches("Bash(*)", "Bash", &json!({"command": "anything"}), &cwd));
        assert!(rule_matches("mcp__files", "mcp__files__read", &json!({}), &cwd));
        assert!(rule_matches("mcp__files", "mcp__files", &json!({}), &cwd));
        assert!(!rule_matches("mcp__files", "mcp__other__read", &json!({}), &cwd));
        assert!(rule_matches("mcp__files__read", "mcp__files__read", &json!({}), &cwd));
        assert!(!rule_matches("mcp__files__read", "mcp__files__write", &json!({}), &cwd));
    }

    #[test]
    fn rule_bash_exact_prefix_and_space_prefix() {
        let cwd = PathBuf::from("/tmp/work");
        let cmd = |c: &str| json!({"command": c});
        // exact
        assert!(rule_matches("Bash(npm run test)", "Bash", &cmd("npm run test"), &cwd));
        assert!(!rule_matches("Bash(npm run test)", "Bash", &cmd("npm run test --watch"), &cwd));
        // prefix with a space: whole-word boundary
        assert!(rule_matches("Bash(git diff *)", "Bash", &cmd("git diff HEAD"), &cwd));
        assert!(rule_matches("Bash(git diff *)", "Bash", &cmd("git diff"), &cwd));
        assert!(!rule_matches("Bash(git diff *)", "Bash", &cmd("git diff-index"), &cwd));
        // prefix without a space: raw prefix, so `git diff-index` matches
        assert!(rule_matches("Bash(git diff*)", "Bash", &cmd("git diff-index --cached"), &cwd));
        assert!(rule_matches("Bash(git diff*)", "Bash", &cmd("git diff"), &cwd));
        assert!(!rule_matches("Bash(git diff*)", "Bash", &cmd("git status"), &cwd));
    }

    #[test]
    fn rule_path_globs_relative_absolute_and_home() {
        let cwd = PathBuf::from("/work/proj");
        let f = |p: &str| json!({"file_path": p});
        assert!(rule_matches("Read(./.env)", "Read", &f(".env"), &cwd));
        assert!(rule_matches("Read(./.env)", "Read", &f("./.env"), &cwd));
        assert!(!rule_matches("Read(./.env)", "Read", &f("src/.env"), &cwd));
        assert!(rule_matches("Edit(src/**)", "Edit", &f("src/main.rs"), &cwd));
        assert!(rule_matches("Edit(src/**)", "Edit", &f("src/deep/a/b.rs"), &cwd));
        assert!(!rule_matches("Edit(src/**)", "Edit", &f("tests/a.rs"), &cwd));
        assert!(rule_matches("Write(//etc/hosts)", "Write", &f("/etc/hosts"), &cwd));
        assert!(!rule_matches("Write(//etc/hosts)", "Write", &f("etc/hosts"), &cwd));
        assert!(rule_matches("Read(~/notes.txt)", "Read", &f("~/notes.txt"), &cwd));
        assert!(!rule_matches("Read(src/**, tests/**)", "Read", &f("src/x"), &cwd));
    }

    #[test]
    fn rule_webfetch_domain() {
        let cwd = PathBuf::from("/work");
        let u = |s: &str| json!({"url": s});
        assert!(rule_matches(
            "WebFetch(domain:example.com)",
            "WebFetch",
            &u("https://www.example.com/page"),
            &cwd
        ));
        // SPEC C2.8: plain `domain:x` is the host itself; subdomains need `*.x`.
        assert!(!rule_matches(
            "WebFetch(domain:example.com)",
            "WebFetch",
            &u("https://api.example.com/v1"),
            &cwd
        ));
        assert!(!rule_matches(
            "WebFetch(domain:example.com)",
            "WebFetch",
            &u("https://notexample.com/"),
            &cwd
        ));
        assert!(!rule_matches(
            "WebFetch(domain:example.com)",
            "WebFetch",
            &u("https://example.org/"),
            &cwd
        ));
    }

    #[test]
    fn rule_empty_and_unknown_forms_do_not_match() {
        let cwd = PathBuf::from("/work");
        assert!(!rule_matches("", "Bash", &json!({"command": "ls"}), &cwd));
        assert!(!rule_matches("Bash", "Read", &json!({"file_path": "x"}), &cwd));
        assert!(!rule_matches("Bash(ls)", "Bash", &json!({}), &cwd));
    }

    // ───────── modes ─────────

    #[test]
    fn default_mode_readonly_allowed_mutating_asks() {
        let dir = tmpdir("default");
        let ctx = ctx_for(&dir);
        let p = perms(PermissionMode::Default, &[], &[], &[], None);
        assert!(is_allow(&p.check("Read", &json!({"file_path": "a"}), true, &ctx)));
        assert!(is_allow(&p.check("Grep", &json!({"pattern": "x"}), true, &ctx)));
        assert!(is_ask(&p.check("Write", &json!({"file_path": "a"}), false, &ctx)));
        assert!(is_ask(&p.check("Bash", &json!({"command": "ls"}), false, &ctx)));
        assert!(is_ask(&p.check("WebFetch", &json!({"url": "https://x.test"}), false, &ctx)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accept_edits_mode() {
        let dir = tmpdir("accept");
        let ctx = ctx_for(&dir);
        let p = perms(PermissionMode::AcceptEdits, &[], &[], &[], None);
        assert!(is_allow(&p.check("Write", &json!({"file_path": "src/a.rs"}), false, &ctx)));
        assert!(is_allow(&p.check("Edit", &json!({"file_path": "./a.rs"}), false, &ctx)));
        assert!(is_allow(&p.check("Bash", &json!({"command": "mkdir -p a/b"}), false, &ctx)));
        assert!(is_allow(&p.check("Bash", &json!({"command": "cp a b"}), false, &ctx)));
        assert!(is_ask(&p.check("Bash", &json!({"command": "rm -rf /"}), false, &ctx)));
        assert!(is_ask(&p.check("Bash", &json!({"command": "cargo test"}), false, &ctx)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accept_edits_stays_inside_working_dirs() {
        let dir = tmpdir("accept-outside");
        let ctx = ctx_for(&dir);
        let p = perms(PermissionMode::AcceptEdits, &[], &[], &[], None);
        assert!(is_ask(&p.check(
            "Write",
            &json!({"file_path": "/definitely/outside/file.txt"}),
            false,
            &ctx
        )));
        // extra_dirs extends the allowed area
        let mut ctx2 = ctx_for(&dir);
        let extra = tmpdir("accept-extra");
        ctx2.extra_dirs = vec![extra.clone()];
        let target = extra.join("ok.txt");
        assert!(is_allow(&p.check(
            "Write",
            &json!({"file_path": target.to_string_lossy()}),
            false,
            &ctx2
        )));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&extra);
    }

    #[test]
    fn plan_mode_denies_all_mutating_tools() {
        let dir = tmpdir("plan");
        let ctx = ctx_for(&dir);
        let p = perms(PermissionMode::Plan, &[], &[], &[], None);
        assert!(is_allow(&p.check("Read", &json!({"file_path": "a"}), true, &ctx)));
        for (tool, input) in [
            ("Write", json!({"file_path": "a", "content": "x"})),
            ("Edit", json!({"file_path": "a"})),
            ("Bash", json!({"command": "ls"})),
            ("WebFetch", json!({"url": "https://x.test"})),
        ] {
            let d = p.check(tool, &input, false, &ctx);
            assert_eq!(deny_reason(&d), "plan mode: only read-only tools are available");
        }
        // even an explicit allow rule cannot unlock plan mode
        let p2 = perms(PermissionMode::Plan, &["Write"], &[], &[], None);
        assert!(matches!(
            p2.check("Write", &json!({"file_path": "a"}), false, &ctx),
            PermissionDecision::Deny(_)
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bypass_permissions_allows_unless_denied() {
        let dir = tmpdir("bypass");
        let ctx = ctx_for(&dir);
        let p = perms(PermissionMode::BypassPermissions, &[], &[], &[], None);
        assert!(is_allow(&p.check("Bash", &json!({"command": "rm -rf /"}), false, &ctx)));
        assert!(is_allow(&p.check("Write", &json!({"file_path": "/etc/x"}), false, &ctx)));
        // ask rules are also bypassed
        let p2 = perms(PermissionMode::BypassPermissions, &[], &["Bash"], &[], None);
        assert!(is_allow(&p2.check("Bash", &json!({"command": "ls"}), false, &ctx)));
        // deny still wins (zen choice, SPEC §5)
        let p3 = perms(PermissionMode::BypassPermissions, &[], &[], &["Bash(rm *)"], None);
        assert!(matches!(
            p3.check("Bash", &json!({"command": "rm -rf /"}), false, &ctx),
            PermissionDecision::Deny(_)
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dont_ask_turns_prompts_into_denials_but_keeps_allow_rules() {
        let dir = tmpdir("dontask");
        let ctx = ctx_for(&dir);
        let p = perms(PermissionMode::DontAsk, &[], &[], &[], None);
        assert!(is_allow(&p.check("Read", &json!({"file_path": "a"}), true, &ctx)));
        assert!(matches!(
            p.check("Write", &json!({"file_path": "a"}), false, &ctx),
            PermissionDecision::Deny(_)
        ));
        assert!(matches!(
            p.check("Bash", &json!({"command": "ls"}), false, &ctx),
            PermissionDecision::Deny(_)
        ));
        // explicit ask rules become denials too
        let p2 = perms(PermissionMode::DontAsk, &[], &["WebFetch"], &[], None);
        assert!(matches!(
            p2.check("WebFetch", &json!({"url": "https://x.test"}), false, &ctx),
            PermissionDecision::Deny(_)
        ));
        // allow rules still apply
        let p3 = perms(PermissionMode::DontAsk, &["Bash(cargo test)"], &[], &[], None);
        assert!(is_allow(&p3.check("Bash", &json!({"command": "cargo test"}), false, &ctx)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deny_beats_allow_and_ask() {
        let dir = tmpdir("deny-wins");
        let ctx = ctx_for(&dir);
        let p = perms(
            PermissionMode::Default,
            &["Bash"],
            &[],
            &["Bash(rm *)"],
            None,
        );
        assert!(is_allow(&p.check("Bash", &json!({"command": "ls"}), false, &ctx)));
        assert!(matches!(
            p.check("Bash", &json!({"command": "rm -rf /"}), false, &ctx),
            PermissionDecision::Deny(_)
        ));
        // deny wins over an ask rule as well
        let p2 = perms(
            PermissionMode::Default,
            &["Bash"],
            &["Bash(rm *)"],
            &["Bash(rm *)"],
            None,
        );
        assert!(matches!(
            p2.check("Bash", &json!({"command": "rm x"}), false, &ctx),
            PermissionDecision::Deny(_)
        ));
        // ask beats allow
        let p3 = perms(PermissionMode::Default, &["Bash"], &["Bash(git push *)"], &[], None);
        assert!(is_ask(&p3.check("Bash", &json!({"command": "git push origin main"}), false, &ctx)));
        assert!(is_allow(&p3.check("Bash", &json!({"command": "git status"}), false, &ctx)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_only_tools_allowed_in_default_and_accept_edits() {
        let dir = tmpdir("readonly-tools");
        let ctx = ctx_for(&dir);
        for mode in [PermissionMode::Default, PermissionMode::AcceptEdits, PermissionMode::DontAsk] {
            let p = perms(mode, &[], &[], &[], None);
            for tool in ["Read", "Glob", "Grep", "TodoWrite", "Task", "Skill"] {
                assert!(
                    is_allow(&p.check(tool, &json!({}), true, &ctx)),
                    "{} should be allowed in {}",
                    tool,
                    mode.as_str()
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ───────── add_allow_rule ─────────

    #[test]
    fn add_allow_rule_updates_state_and_file() {
        let dir = tmpdir("always");
        let settings_path = dir.join(".zen/settings.local.json");
        std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
        std::fs::write(
            &settings_path,
            r#"{"model":"keep-me","permissions":{"allow":["Read"],"deny":["Bash(rm *)"]}}"#,
        )
        .unwrap();

        let ctx = ctx_for(&dir);
        let p = perms(
            PermissionMode::Default,
            &["Read"],
            &[],
            &[],
            Some(settings_path.clone()),
        );
        assert!(is_ask(&p.check("Write", &json!({"file_path": "a"}), false, &ctx)));
        p.add_allow_rule("Write");
        assert!(is_allow(&p.check("Write", &json!({"file_path": "a"}), false, &ctx)));

        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert_eq!(saved["model"], json!("keep-me"));
        assert_eq!(saved["permissions"]["deny"], json!(["Bash(rm *)"]));
        let allow = saved["permissions"]["allow"].as_array().unwrap();
        assert!(allow.contains(&json!("Read")));
        assert!(allow.contains(&json!("Write")));

        // Adding the same rule twice does not duplicate it.
        p.add_allow_rule("Write");
        let saved2: Value = serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert_eq!(saved2["permissions"]["allow"].as_array().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_allow_rule_creates_missing_file() {
        let dir = tmpdir("always-new");
        let settings_path = dir.join("nested/deeper/settings.local.json");
        let ctx = ctx_for(&dir);
        let p = perms(PermissionMode::Default, &[], &[], &[], Some(settings_path.clone()));
        p.add_allow_rule("Bash(cargo test)");
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert_eq!(saved["permissions"]["allow"], json!(["Bash(cargo test)"]));
        assert!(is_allow(
            &p.check("Bash", &json!({"command": "cargo test"}), false, &ctx)
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_allow_rule_without_path_only_updates_memory() {
        let dir = tmpdir("always-mem");
        let ctx = ctx_for(&dir);
        let p = perms(PermissionMode::Default, &[], &[], &[], None);
        p.add_allow_rule("Edit");
        assert!(is_allow(&p.check("Edit", &json!({"file_path": "a"}), false, &ctx)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn suggested_rules_are_sane() {
        assert_eq!(
            suggested_rule("Bash", &json!({"command": "ls -la"})).unwrap(),
            "Bash(ls -la)"
        );
        assert_eq!(suggested_rule("Read", &json!({"file_path": "a.txt"})).unwrap(), "Read(a.txt)");
        assert_eq!(
            suggested_rule("WebFetch", &json!({"url": "https://www.example.com/x"})).unwrap(),
            "WebFetch(domain:example.com)"
        );
        assert_eq!(suggested_rule("TodoWrite", &json!({})).unwrap(), "TodoWrite");
    }

    // ───────── SPEC Appendix C2 parity ─────────

    fn bash(c: &str) -> Value {
        json!({ "command": c })
    }

    #[test]
    fn c2_1_first_match_wins_specificity_ignored() {
        let dir = tmpdir("c21");
        let ctx = ctx_for(&dir);
        // A very specific allow does not beat a broad ask; a broad allow loses to a narrow deny.
        let p = perms(PermissionMode::Default, &["Bash(git push origin main)"], &["Bash(git *)"], &[], None);
        assert!(is_ask(&p.check("Bash", &bash("git push origin main"), false, &ctx)));
        let p2 = perms(PermissionMode::Default, &["Bash(*)"], &[], &["Bash(curl *)"], None);
        assert!(matches!(p2.check("Bash", &bash("curl x"), false, &ctx), PermissionDecision::Deny(_)));
        assert!(is_allow(&p2.check("Bash", &bash("ls"), false, &ctx)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn c2_2_only_tool_level_deny_removes_tool() {
        assert!(rule_removes_tool("Bash", "Bash"));
        assert!(rule_removes_tool("Bash(*)", "Bash"));
        assert!(rule_removes_tool("WebFetch", "WebFetch"));
        assert!(!rule_removes_tool("Bash(rm *)", "Bash"));
        assert!(!rule_removes_tool("Read(./.env)", "Read"));
        assert!(rule_removes_tool("mcp__srv", "mcp__srv__q"));
        assert!(rule_removes_tool("mcp__*", "mcp__srv__q"));
        assert!(!rule_removes_tool("Bash", "Read"));
    }

    #[test]
    fn c2_3_bash_wildcards() {
        let cwd = PathBuf::from("/w");
        let m = |r: &str, c: &str| rule_matches(r, "Bash", &bash(c), &cwd);
        assert!(m("Bash(ls *)", "ls"));
        assert!(m("Bash(ls *)", "ls -la"));
        assert!(!m("Bash(ls *)", "lsof"));
        assert!(m("Bash(ls*)", "lsof"));
        assert!(m("Bash(ls:*)", "ls -la"));
        assert!(m("Bash(ls:*)", "ls"));
        assert!(!m("Bash(ls:*)", "lsof"));
        // `*` anywhere, spanning spaces
        assert!(m("Bash(git * main)", "git push origin main"));
        assert!(!m("Bash(git * main)", "git push origin dev"));
        assert!(m("Bash(* --version)", "node --version"));
        assert!(m("Bash(npm run *:fast)", "npm run test:fast"));
    }

    #[test]
    fn c2_4_compound_commands() {
        assert_eq!(
            split_compound("a && b || c; d | e |& f & g\nh").unwrap(),
            vec!["a", "b", "c", "d", "e", "f", "g", "h"]
        );
        assert_eq!(split_compound("echo 'a && b' \"c;d\"").unwrap().len(), 1);
        assert_eq!(split_compound("cargo test 2>&1 | tail").unwrap(), vec!["cargo test 2>&1", "tail"]);
        assert_eq!(split_compound("ls;").unwrap(), vec!["ls"]);
        assert!(split_compound("ls &&").is_none());

        let dir = tmpdir("c24");
        let ctx = ctx_for(&dir);
        let p = perms(PermissionMode::Default, &["Bash(git status)", "Bash(ls *)"], &[], &["Bash(rm *)"], None);
        // allow must cover every part
        assert!(is_allow(&p.check("Bash", &bash("git status && ls -la"), false, &ctx)));
        assert!(is_ask(&p.check("Bash", &bash("git status && curl x"), false, &ctx)));
        // deny applies if any part matches
        assert!(matches!(p.check("Bash", &bash("ls && rm -rf x"), false, &ctx), PermissionDecision::Deny(_)));
        // ask applies if any part matches
        let p2 = perms(PermissionMode::Default, &["Bash"], &["Bash(git push *)"], &[], None);
        assert!(is_ask(&p2.check("Bash", &bash("git add . && git push origin"), false, &ctx)));
        // dangling operator: not split, scoped allow cannot match
        assert!(is_ask(&p.check("Bash", &bash("git status &&"), false, &ctx)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn c2_7_path_rule_forms() {
        let cwd = PathBuf::from("/work/proj");
        let f = |p: &str| json!({ "file_path": p });
        // `/x` is relative to the settings root (cwd), `//x` absolute
        assert!(rule_matches("Read(/src/**)", "Read", &f("src/a.rs"), &cwd));
        assert!(!rule_matches("Read(/src/**)", "Read", &f("/src/a.rs"), &cwd));
        assert!(rule_matches("Read(//src/**)", "Read", &f("/src/a.rs"), &cwd));
        // bare filename matches at any depth below cwd
        assert!(rule_matches("Read(.env)", "Read", &f(".env"), &cwd));
        assert!(rule_matches("Read(.env)", "Read", &f("a/b/.env"), &cwd));
        assert!(rule_matches("Read(*.log)", "Read", &f("logs/x.log"), &cwd));
        assert!(!rule_matches("Read(.env)", "Read", &f("a/.envrc"), &cwd));
        // Edit rules cover Write
        assert!(rule_matches("Edit(src/**)", "Write", &f("src/new.rs"), &cwd));
        assert!(rule_matches("Edit", "Write", &f("x"), &cwd));
    }

    #[test]
    fn c2_7_read_deny_guards_edit_write_grep_glob() {
        let dir = tmpdir("c27");
        let ctx = ctx_for(&dir);
        let p = perms(PermissionMode::BypassPermissions, &[], &[], &["Read(./secrets/**)"], None);
        for (tool, input) in [
            ("Read", json!({"file_path": "secrets/k"})),
            ("Edit", json!({"file_path": "secrets/k"})),
            ("Write", json!({"file_path": "secrets/k", "content": ""})),
            ("Grep", json!({"pattern": "x", "path": "secrets"})),
            ("Glob", json!({"pattern": "*", "path": "secrets/sub"})),
        ] {
            assert!(
                matches!(p.check(tool, &input, false, &ctx), PermissionDecision::Deny(_)),
                "{} should be denied",
                tool
            );
        }
        assert!(is_allow(&p.check("Write", &json!({"file_path": "src/k"}), false, &ctx)));
        // A Read *allow* does not grant Write.
        let p2 = perms(PermissionMode::Default, &["Read(./src/**)"], &[], &[], None);
        assert!(is_ask(&p2.check("Write", &json!({"file_path": "src/k"}), false, &ctx)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn c2_8_webfetch_domain_forms() {
        let cwd = PathBuf::from("/w");
        let m = |r: &str, u: &str| rule_matches(r, "WebFetch", &json!({ "url": u }), &cwd);
        assert!(m("WebFetch(domain:Example.COM)", "https://EXAMPLE.com/x"));
        assert!(m("WebFetch(domain:*.example.com)", "https://api.example.com/x"));
        assert!(m("WebFetch(domain:*.example.com)", "https://a.b.example.com/x"));
        assert!(!m("WebFetch(domain:*.example.com)", "https://example.com/x"));
        assert!(!m("WebFetch(domain:*.example.com)", "https://badexample.com/x"));
        assert!(m("WebFetch(domain:*)", "https://anything.test/"));
        assert!(!m("WebFetch(domain:*)", ""));
    }

    #[test]
    fn c2_9_mcp_rules_and_globs() {
        let dir = tmpdir("c29");
        let ctx = ctx_for(&dir);
        let cwd = dir.clone();
        let v = json!({});
        assert!(rule_matches("mcp__srv__*", "mcp__srv__query", &v, &cwd));
        assert!(!rule_matches("mcp__srv__*", "mcp__other__query", &v, &cwd));
        // generic globs are honoured in deny/ask, not in allow
        let p = perms(PermissionMode::Default, &["*"], &["mcp__*"], &[], None);
        assert!(is_ask(&p.check("mcp__srv__q", &v, false, &ctx)));
        assert!(is_ask(&p.check("Bash", &bash("ls"), false, &ctx)), "allow `*` must not match");
        let p2 = perms(PermissionMode::BypassPermissions, &[], &[], &["*"], None);
        assert!(matches!(p2.check("Bash", &bash("ls"), false, &ctx), PermissionDecision::Deny(_)));
        let p3 = perms(PermissionMode::Default, &["mcp__srv__*"], &[], &[], None);
        assert!(is_allow(&p3.check("mcp__srv__q", &v, false, &ctx)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn c2_11_mode_aliases_and_accept_edits_fs_scope() {
        assert_eq!(PermissionMode::parse("manual"), Some(PermissionMode::Default));
        assert_eq!(PermissionMode::parse("auto"), Some(PermissionMode::Default));
        let dir = tmpdir("c211");
        let ctx = ctx_for(&dir);
        let p = perms(PermissionMode::AcceptEdits, &[], &[], &[], None);
        assert!(is_allow(&p.check("Bash", &bash("mkdir -p a/b && touch a/b/c"), false, &ctx)));
        assert!(is_ask(&p.check("Bash", &bash("cp a /etc/passwd"), false, &ctx)));
        assert!(is_ask(&p.check("Bash", &bash("mv a ../outside"), false, &ctx)));
        assert!(is_ask(&p.check("Bash", &bash("mkdir a && rm -rf b"), false, &ctx)));
        assert!(is_ask(&p.check("Bash", &bash("touch $(whoami)"), false, &ctx)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn c2_12_always_allow_one_rule_per_subcommand() {
        assert_eq!(
            suggested_rules("Bash", &bash("git add . && git commit -m x")),
            vec!["Bash(git add .)", "Bash(git commit -m x)"]
        );
        assert_eq!(suggested_rules("Bash", &bash("a;b;c;d;e;f;g")).len(), 5);
        assert_eq!(suggested_rules("Bash", &bash("ls")), vec!["Bash(ls)"]);
        assert_eq!(suggested_rules("Write", &json!({"file_path": "a"})), vec!["Write(a)"]);
        // Each saved rule lands in settings.local.json and the compound is then allowed.
        let dir = tmpdir("c212");
        let path = dir.join(".zen/settings.local.json");
        let ctx = ctx_for(&dir);
        let p = perms(PermissionMode::Default, &[], &[], &[], Some(path.clone()));
        for r in suggested_rules("Bash", &bash("git add . && git commit -m x")) {
            p.add_allow_rule(&r);
        }
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved["permissions"]["allow"].as_array().unwrap().len(), 2);
        assert!(is_allow(&p.check("Bash", &bash("git add . && git commit -m x"), false, &ctx)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn c2_13_ask_rule_detection() {
        let dir = tmpdir("c213");
        let ctx = ctx_for(&dir);
        let p = perms(PermissionMode::Default, &[], &["Bash(git push *)"], &[], None);
        assert!(p.ask_rule_matches("Bash", &bash("git push"), &ctx));
        assert!(p.ask_rule_matches("Bash", &bash("ls && git push x"), &ctx));
        assert!(!p.ask_rule_matches("Bash", &bash("ls"), &ctx));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
