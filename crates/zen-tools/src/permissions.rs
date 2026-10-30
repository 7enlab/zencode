use crate::builtin::{canonicalize_lenient, resolve_path};
use globset::GlobBuilder;
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use zen_core::{
    PermissionChecker, PermissionDecision, PermissionMode, PermissionSettings, ToolContext,
};

pub fn tool_is_read_only(name: &str) -> bool {
    matches!(
        name,
        "Read" | "Glob" | "Grep" | "TodoWrite" | "Task" | "Skill"
    )
}

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

fn path_rule_matches(cwd: &Path, spec: &str, candidate: &str) -> bool {
    let (pattern_path, dir_hint) = rule_pattern_path(cwd, spec);

    let target = canonicalize_lenient(&resolve_path(cwd, candidate));
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
        return wildcard_match(prefix, command)
            || wildcard_match(&format!("{} *", prefix), command);
    }
    wildcard_match(&spec, command)
}

pub fn split_compound(command: &str) -> Option<Vec<String>> {
    let chars: Vec<char> = command.chars().collect();

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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RuleKind {
    Allow,
    Ask,
    Deny,
}

fn is_edit_tool(name: &str) -> bool {
    matches!(name, "Edit" | "Write" | "MultiEdit" | "NotebookEdit")
}

pub fn rule_matches(rule: &str, tool_name: &str, input: &Value, cwd: &Path) -> bool {
    rule_matches_kind(rule, tool_name, input, cwd, RuleKind::Allow)
}

fn rule_matches_kind(
    rule: &str,
    tool_name: &str,
    input: &Value,
    cwd: &Path,
    kind: RuleKind,
) -> bool {
    let rule = rule.trim();
    if rule.is_empty() {
        return false;
    }
    if !rule.contains('(') {
        if rule.contains('*') && (rule.starts_with("mcp__") || kind != RuleKind::Allow) {
            return wildcard_match(rule, tool_name);
        }

        if rule.starts_with("mcp__") {
            return rule == tool_name || tool_name.starts_with(&format!("{}__", rule));
        }
    }

    let (rule_tool, spec) = split_rule(rule);
    let spec = spec.map(str::trim).filter(|s| !s.is_empty() && *s != "*");

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

pub fn rule_removes_tool(rule: &str, tool_name: &str) -> bool {
    let rule = rule.trim();
    let (rule_tool, spec) = split_rule(rule);
    let tool_level = spec
        .map(|s| s.trim().is_empty() || s.trim() == "*")
        .unwrap_or(true);
    tool_level
        && rule_matches_kind(
            rule_tool,
            tool_name,
            &Value::Null,
            Path::new("/"),
            RuleKind::Deny,
        )
}

fn host_raw(url: &str) -> String {
    let after_scheme = url.split("://").nth(1).unwrap_or(url);
    let host_and_path = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    let host = host_and_path
        .split('@')
        .next_back()
        .unwrap_or(host_and_path);
    let host = host.split(':').next().unwrap_or(host);
    host.to_lowercase()
}

fn host_of(url: &str) -> String {
    host_raw(url).trim_start_matches("www.").to_string()
}

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

fn any_rule<'a>(
    rules: &'a [String],
    tool_name: &str,
    input: &Value,
    cwd: &Path,
    kind: RuleKind,
) -> Option<&'a String> {
    let subs: Vec<Value> = match bash_parts(tool_name, input) {
        Some(Some(parts)) if parts.len() > 1 => parts
            .into_iter()
            .map(|p| serde_json::json!({ "command": p }))
            .collect(),
        _ => Vec::new(),
    };
    rules.iter().find(|r| {
        rule_matches_kind(r, tool_name, input, cwd, kind)
            || subs
                .iter()
                .any(|sub| rule_matches_kind(r, tool_name, sub, cwd, kind))
    })
}

fn allow_covers(rules: &[String], tool_name: &str, input: &Value, cwd: &Path) -> bool {
    let tool_level = |r: &String| {
        let (_, spec) = split_rule(r.trim());
        spec.map(|s| s.trim().is_empty() || s.trim() == "*")
            .unwrap_or(true)
    };
    match bash_parts(tool_name, input) {
        None => rules
            .iter()
            .any(|r| rule_matches_kind(r, tool_name, input, cwd, RuleKind::Allow)),
        Some(None) => rules
            .iter()
            .any(|r| tool_level(r) && rule_matches_kind(r, tool_name, input, cwd, RuleKind::Allow)),
        Some(Some(parts)) if parts.len() <= 1 => rules
            .iter()
            .any(|r| rule_matches_kind(r, tool_name, input, cwd, RuleKind::Allow)),
        Some(Some(parts)) => parts.iter().all(|p| {
            let sub = serde_json::json!({ "command": p });
            rules
                .iter()
                .any(|r| rule_matches_kind(r, tool_name, &sub, cwd, RuleKind::Allow))
        }),
    }
}

fn substitution_bodies(command: &str) -> Vec<String> {
    fn scan(chars: &[char], out: &mut Vec<String>) {
        let (mut sq, mut dq) = (false, false);
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c == '\\' && !sq {
                i += 2;
                continue;
            }
            if c == '\'' && !dq {
                sq = !sq;
            } else if c == '"' && !sq {
                dq = !dq;
            } else if !sq {
                if c == '$' && chars.get(i + 1) == Some(&'(') {
                    let mut depth = 1usize;
                    let mut j = i + 2;
                    let (mut jsq, mut jdq) = (false, false);
                    while j < chars.len() && depth > 0 {
                        let d = chars[j];
                        if d == '\\' && !jsq {
                            j += 2;
                            continue;
                        }
                        if d == '\'' && !jdq {
                            jsq = !jsq;
                        } else if d == '"' && !jsq {
                            jdq = !jdq;
                        } else if !jsq && !jdq {
                            if d == '(' {
                                depth += 1;
                            } else if d == ')' {
                                depth -= 1;
                            }
                        }
                        j += 1;
                    }
                    if depth == 0 {
                        let inner: String = chars[i + 2..j - 1].iter().collect();
                        out.push(inner.clone());
                        scan(&inner.chars().collect::<Vec<_>>(), out);
                        i = j;
                        continue;
                    }
                } else if c == '`' {
                    let mut j = i + 1;
                    while j < chars.len() {
                        if chars[j] == '\\' {
                            j += 2;
                            continue;
                        }
                        if chars[j] == '`' {
                            break;
                        }
                        j += 1;
                    }
                    if j < chars.len() {
                        let inner: String = chars[i + 1..j].iter().collect();
                        out.push(inner.clone());
                        scan(&inner.chars().collect::<Vec<_>>(), out);
                        i = j + 1;
                        continue;
                    }
                } else if c == '$' && chars.get(i + 1) == Some(&'{') {
                    let mut depth = 1usize;
                    let mut j = i + 2;
                    while j < chars.len() && depth > 0 {
                        match chars[j] {
                            '{' => depth += 1,
                            '}' => depth -= 1,
                            _ => {}
                        }
                        j += 1;
                    }
                    if depth == 0 {
                        let inner: String = chars[i + 2..j - 1].iter().collect();
                        scan(&inner.chars().collect::<Vec<_>>(), out);
                        i = j;
                        continue;
                    }
                }
            }
            i += 1;
        }
    }
    let mut out = Vec::new();
    scan(&command.chars().collect::<Vec<_>>(), &mut out);
    out
}

fn bash_parts(tool_name: &str, input: &Value) -> Option<Option<Vec<String>>> {
    if !tool_name.eq_ignore_ascii_case("Bash") {
        return None;
    }
    let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
    Some(match split_compound(cmd) {
        None => None,
        Some(parts) => {
            let mut seen: HashSet<String> = parts.iter().cloned().collect();
            let mut all = parts;

            for part in all.clone() {
                for body in substitution_bodies(&part) {
                    match split_compound(&body) {
                        Some(inner) if !inner.is_empty() => {
                            for frag in inner {
                                if seen.insert(frag.clone()) {
                                    all.push(frag);
                                }
                            }
                        }
                        _ => {
                            if seen.insert(body.clone()) {
                                all.push(body);
                            }
                        }
                    }
                }
            }
            Some(all)
        }
    })
}

pub struct RulePermissions {
    pub mode: PermissionMode,
    pub allow: Vec<String>,
    pub ask: Vec<String>,
    pub deny: Vec<String>,
    pub local_settings_path: Option<PathBuf>,

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

    pub fn effective_allow(&self) -> Vec<String> {
        let mut v = self.allow.clone();
        v.extend(self.dynamic_allow.lock().unwrap().iter().cloned());
        v
    }

    fn working_dirs(&self, ctx: &ToolContext) -> Vec<PathBuf> {
        let mut dirs = vec![canonicalize_lenient(&ctx.cwd())];
        for d in &ctx.extra_dirs {
            dirs.push(canonicalize_lenient(d));
        }
        dirs
    }

    fn path_inside_workdir(&self, input: &Value, ctx: &ToolContext) -> bool {
        let Some(fp) = path_arg(input) else {
            return false;
        };
        let target = canonicalize_lenient(&resolve_path(&ctx.cwd(), fp));
        self.working_dirs(ctx).iter().any(|d| target.starts_with(d))
    }

    fn fs_args_inside_workdir(&self, command: &str, ctx: &ToolContext) -> bool {
        if command.contains(['`', '>', '<', '$']) {
            return false;
        }
        let cwd = ctx.cwd();
        let dirs = self.working_dirs(ctx);
        split_compound(command)
            .unwrap_or_default()
            .iter()
            .all(|part| {
                part.split_whitespace()
                    .skip(1)
                    .filter(|a| !a.starts_with('-'))
                    .all(|arg| {
                        let arg = arg.trim_matches(|c| c == '"' || c == '\'');

                        let target = canonicalize_lenient(&resolve_path(&cwd, arg));
                        dirs.iter().any(|d| target.starts_with(d))
                    })
            })
    }

    fn evaluate(
        &self,
        tool_name: &str,
        input: &Value,
        read_only: bool,
        ctx: &ToolContext,
    ) -> PermissionDecision {
        let cwd = ctx.cwd();

        if let Some(rule) = any_rule(&self.deny, tool_name, input, &cwd, RuleKind::Deny) {
            return PermissionDecision::Deny(format!("Denied by rule '{}'", rule));
        }

        if self.mode == PermissionMode::Plan && !read_only {
            return PermissionDecision::Deny(
                "plan mode: only read-only tools are available".into(),
            );
        }

        if let Some(rule) = any_rule(&self.ask, tool_name, input, &cwd, RuleKind::Ask) {
            return self.resolve_ask(format!("Ask rule '{}' matches", rule));
        }

        let allowed = allow_covers(&self.effective_allow(), tool_name, input, &cwd);

        if self.mode == PermissionMode::BypassPermissions {
            return PermissionDecision::Allow;
        }

        if allowed {
            return PermissionDecision::Allow;
        }

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

            PermissionMode::Plan => PermissionDecision::Deny("plan mode".into()),
            PermissionMode::BypassPermissions => PermissionDecision::Allow,
            _ => PermissionDecision::Ask,
        }
    }
}

impl PermissionChecker for RulePermissions {
    fn check(
        &self,
        tool_name: &str,
        input: &Value,
        read_only: bool,
        ctx: &ToolContext,
    ) -> PermissionDecision {
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
                eprintln!(
                    "warning: could not save rule '{}' to {}: {}",
                    rule,
                    path.display(),
                    e
                );
            }
        }
    }
}

pub fn persist_allow_rule(path: &Path, rule: &str) -> anyhow::Result<()> {
    let mut root: Value = match std::fs::read_to_string(path) {
        Ok(text) if !text.trim().is_empty() => {
            serde_json::from_str(&text).unwrap_or_else(|_| Value::Object(Default::default()))
        }
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
    let allow = perms.entry("allow").or_insert_with(|| Value::Array(vec![]));
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

pub fn suggested_rule(tool_name: &str, input: &Value) -> Option<String> {
    match tool_name {
        "Bash" => input
            .get("command")
            .and_then(|v| v.as_str())
            .map(|c| format!("Bash({})", c)),
        "Read" | "Edit" | "MultiEdit" | "Write" => {
            path_arg(input).map(|p| format!("{}({})", tool_name, p))
        }
        "WebFetch" => input.get("url").and_then(|v| v.as_str()).and_then(|u| {
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

pub fn dedup(mut rules: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    rules.retain(|r| seen.insert(r.clone()));
    rules
}

#[cfg(test)]
mod tests {
    use super::*;
    use zen_core::PermissionMode;

    fn test_perms(deny: &[&str], allow: &[&str]) -> RulePermissions {
        RulePermissions::new(
            PermissionMode::Default,
            &PermissionSettings {
                deny: deny.iter().map(|s| s.to_string()).collect(),
                allow: allow.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
            vec![],
            vec![],
            None,
        )
    }

    fn check_bash(perms: &RulePermissions, cmd: &str) -> PermissionDecision {
        let ctx = ToolContext::new(std::path::PathBuf::from("/tmp"), "test");
        perms.check("Bash", &serde_json::json!({ "command": cmd }), false, &ctx)
    }

    fn is_deny(d: &PermissionDecision) -> bool {
        matches!(d, PermissionDecision::Deny(_))
    }

    #[test]
    fn substitution_deny_bypass_blocked() {
        let perms = test_perms(&["Bash(rm *)"], &["Bash(echo *)"]);
        assert!(
            is_deny(&check_bash(&perms, "echo $(rm -rf ~)")),
            "$() bypass not blocked"
        );
        assert!(
            is_deny(&check_bash(&perms, "echo `rm -rf ~`")),
            "backtick bypass not blocked"
        );
        assert!(
            is_deny(&check_bash(&perms, "echo $(echo $(rm -rf ~))")),
            "nested bypass not blocked"
        );
        assert!(
            is_deny(&check_bash(&perms, "echo ${X:-$(rm -rf ~)}")),
            "${{}} bypass not blocked"
        );
        assert!(is_deny(&check_bash(&perms, "rm -rf /tmp/x")));
    }

    #[test]
    fn substitution_allow_fails_closed() {
        let perms = test_perms(&[], &["Bash(echo *)", "Bash(date *)"]);
        assert!(matches!(
            check_bash(&perms, "echo $(date)"),
            PermissionDecision::Allow
        ));
        let perms2 = test_perms(&[], &["Bash(echo *)"]);
        assert!(matches!(
            check_bash(&perms2, "echo $(date)"),
            PermissionDecision::Ask
        ));
    }

    #[test]
    fn substitution_inside_double_quotes_blocked() {
        let perms = test_perms(&["Bash(rm *)"], &["Bash(echo *)"]);
        assert!(
            is_deny(&check_bash(&perms, "echo \"$(rm -rf ~)\"")),
            "dq $() bypass not blocked"
        );
        assert!(
            is_deny(&check_bash(&perms, "echo \"x `rm -rf ~` y\"")),
            "dq backtick bypass not blocked"
        );
        assert!(
            is_deny(&check_bash(&perms, "echo \"${X:-$(rm -rf ~)}\"")),
            "dq ${{}} bypass not blocked"
        );
    }

    #[test]
    fn substitution_single_quoted_not_extracted() {
        let perms = test_perms(&["Bash(rm *)"], &[]);
        assert!(!is_deny(&check_bash(&perms, "echo '$(rm -rf ~)'")));
        assert!(substitution_bodies("echo '$(rm)'").is_empty());
    }

    #[test]
    fn substitution_bodies_extraction() {
        assert_eq!(substitution_bodies("echo $(rm -rf ~)"), vec!["rm -rf ~"]);
        assert_eq!(substitution_bodies("echo `rm -rf ~`"), vec!["rm -rf ~"]);
        assert_eq!(substitution_bodies("echo hi"), Vec::<String>::new());
        let nested = substitution_bodies("a $(b $(c)) d");
        assert!(nested.contains(&"b $(c)".to_string()));
        assert!(nested.contains(&"c".to_string()));
        assert!(substitution_bodies("echo $(oops").is_empty());
    }

    #[test]
    fn compound_without_substitution_unchanged() {
        let perms = test_perms(&[], &["Bash(echo *)", "Bash(ls *)"]);
        assert!(matches!(
            check_bash(&perms, "echo hi && ls /tmp"),
            PermissionDecision::Allow
        ));
        assert!(matches!(
            check_bash(&perms, "echo hi && rm -rf /"),
            PermissionDecision::Ask
        ));
    }

    #[test]
    fn symlink_inside_workdir_blocked() {
        let dir = std::env::temp_dir().join(format!("zen-perm-symlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("work")).unwrap();
        #[cfg(unix)]
        {
            std::fs::create_dir_all(dir.join("outside")).unwrap();
            std::os::unix::fs::symlink(dir.join("outside"), dir.join("work").join("link")).unwrap();
        }
        let perms = test_perms(&[], &[]);
        let ctx = ToolContext::new(dir.join("work"), "test");
        let ok = serde_json::json!({ "file_path": "sub/file.txt" });
        assert!(perms.path_inside_workdir(&ok, &ctx));
        #[cfg(unix)]
        {
            let evil = serde_json::json!({ "file_path": "link/secret.txt" });
            assert!(
                !perms.path_inside_workdir(&evil, &ctx),
                "symlink escape passed the workdir check"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn host_parsing() {
        assert_eq!(host_of("https://www.example.com/a/b?x=1"), "example.com");
        assert_eq!(
            host_of("http://user:pw@api.example.com:8443/x"),
            "api.example.com"
        );
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

    #[test]
    fn rule_bare_tool_names() {
        let cwd = PathBuf::from("/tmp/work");
        assert!(rule_matches(
            "Bash",
            "Bash",
            &json!({"command": "ls"}),
            &cwd
        ));
        assert!(rule_matches(
            "Read",
            "Read",
            &json!({"file_path": "x"}),
            &cwd
        ));
        assert!(!rule_matches(
            "Read",
            "Write",
            &json!({"file_path": "x"}),
            &cwd
        ));
        assert!(rule_matches(
            "Bash(*)",
            "Bash",
            &json!({"command": "anything"}),
            &cwd
        ));
        assert!(rule_matches(
            "mcp__files",
            "mcp__files__read",
            &json!({}),
            &cwd
        ));
        assert!(rule_matches("mcp__files", "mcp__files", &json!({}), &cwd));
        assert!(!rule_matches(
            "mcp__files",
            "mcp__other__read",
            &json!({}),
            &cwd
        ));
        assert!(rule_matches(
            "mcp__files__read",
            "mcp__files__read",
            &json!({}),
            &cwd
        ));
        assert!(!rule_matches(
            "mcp__files__read",
            "mcp__files__write",
            &json!({}),
            &cwd
        ));
    }

    #[test]
    fn rule_bash_exact_prefix_and_space_prefix() {
        let cwd = PathBuf::from("/tmp/work");
        let cmd = |c: &str| json!({"command": c});

        assert!(rule_matches(
            "Bash(npm run test)",
            "Bash",
            &cmd("npm run test"),
            &cwd
        ));
        assert!(!rule_matches(
            "Bash(npm run test)",
            "Bash",
            &cmd("npm run test --watch"),
            &cwd
        ));

        assert!(rule_matches(
            "Bash(git diff *)",
            "Bash",
            &cmd("git diff HEAD"),
            &cwd
        ));
        assert!(rule_matches(
            "Bash(git diff *)",
            "Bash",
            &cmd("git diff"),
            &cwd
        ));
        assert!(!rule_matches(
            "Bash(git diff *)",
            "Bash",
            &cmd("git diff-index"),
            &cwd
        ));

        assert!(rule_matches(
            "Bash(git diff*)",
            "Bash",
            &cmd("git diff-index --cached"),
            &cwd
        ));
        assert!(rule_matches(
            "Bash(git diff*)",
            "Bash",
            &cmd("git diff"),
            &cwd
        ));
        assert!(!rule_matches(
            "Bash(git diff*)",
            "Bash",
            &cmd("git status"),
            &cwd
        ));
    }

    #[test]
    fn rule_path_globs_relative_absolute_and_home() {
        let cwd = PathBuf::from("/work/proj");
        let f = |p: &str| json!({"file_path": p});
        assert!(rule_matches("Read(./.env)", "Read", &f(".env"), &cwd));
        assert!(rule_matches("Read(./.env)", "Read", &f("./.env"), &cwd));
        assert!(!rule_matches("Read(./.env)", "Read", &f("src/.env"), &cwd));
        assert!(rule_matches(
            "Edit(src/**)",
            "Edit",
            &f("src/main.rs"),
            &cwd
        ));
        assert!(rule_matches(
            "Edit(src/**)",
            "Edit",
            &f("src/deep/a/b.rs"),
            &cwd
        ));
        assert!(!rule_matches(
            "Edit(src/**)",
            "Edit",
            &f("tests/a.rs"),
            &cwd
        ));
        assert!(rule_matches(
            "Write(//etc/hosts)",
            "Write",
            &f("/etc/hosts"),
            &cwd
        ));
        assert!(!rule_matches(
            "Write(//etc/hosts)",
            "Write",
            &f("etc/hosts"),
            &cwd
        ));
        assert!(rule_matches(
            "Read(~/notes.txt)",
            "Read",
            &f("~/notes.txt"),
            &cwd
        ));
        assert!(!rule_matches(
            "Read(src/**, tests/**)",
            "Read",
            &f("src/x"),
            &cwd
        ));
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
        assert!(!rule_matches(
            "Bash",
            "Read",
            &json!({"file_path": "x"}),
            &cwd
        ));
        assert!(!rule_matches("Bash(ls)", "Bash", &json!({}), &cwd));
    }

    #[test]
    fn default_mode_readonly_allowed_mutating_asks() {
        let dir = tmpdir("default");
        let ctx = ctx_for(&dir);
        let p = perms(PermissionMode::Default, &[], &[], &[], None);
        assert!(is_allow(&p.check(
            "Read",
            &json!({"file_path": "a"}),
            true,
            &ctx
        )));
        assert!(is_allow(&p.check(
            "Grep",
            &json!({"pattern": "x"}),
            true,
            &ctx
        )));
        assert!(is_ask(&p.check(
            "Write",
            &json!({"file_path": "a"}),
            false,
            &ctx
        )));
        assert!(is_ask(&p.check(
            "Bash",
            &json!({"command": "ls"}),
            false,
            &ctx
        )));
        assert!(is_ask(&p.check(
            "WebFetch",
            &json!({"url": "https://x.test"}),
            false,
            &ctx
        )));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accept_edits_mode() {
        let dir = tmpdir("accept");
        let ctx = ctx_for(&dir);
        let p = perms(PermissionMode::AcceptEdits, &[], &[], &[], None);
        assert!(is_allow(&p.check(
            "Write",
            &json!({"file_path": "src/a.rs"}),
            false,
            &ctx
        )));
        assert!(is_allow(&p.check(
            "Edit",
            &json!({"file_path": "./a.rs"}),
            false,
            &ctx
        )));
        assert!(is_allow(&p.check(
            "Bash",
            &json!({"command": "mkdir -p a/b"}),
            false,
            &ctx
        )));
        assert!(is_allow(&p.check(
            "Bash",
            &json!({"command": "cp a b"}),
            false,
            &ctx
        )));
        assert!(is_ask(&p.check(
            "Bash",
            &json!({"command": "rm -rf /"}),
            false,
            &ctx
        )));
        assert!(is_ask(&p.check(
            "Bash",
            &json!({"command": "cargo test"}),
            false,
            &ctx
        )));
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
        assert!(is_allow(&p.check(
            "Read",
            &json!({"file_path": "a"}),
            true,
            &ctx
        )));
        for (tool, input) in [
            ("Write", json!({"file_path": "a", "content": "x"})),
            ("Edit", json!({"file_path": "a"})),
            ("Bash", json!({"command": "ls"})),
            ("WebFetch", json!({"url": "https://x.test"})),
        ] {
            let d = p.check(tool, &input, false, &ctx);
            assert_eq!(
                deny_reason(&d),
                "plan mode: only read-only tools are available"
            );
        }

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
        assert!(is_allow(&p.check(
            "Bash",
            &json!({"command": "rm -rf /"}),
            false,
            &ctx
        )));
        assert!(is_allow(&p.check(
            "Write",
            &json!({"file_path": "/etc/x"}),
            false,
            &ctx
        )));

        let p2 = perms(PermissionMode::BypassPermissions, &[], &["Bash"], &[], None);
        assert!(is_allow(&p2.check(
            "Bash",
            &json!({"command": "ls"}),
            false,
            &ctx
        )));

        let p3 = perms(
            PermissionMode::BypassPermissions,
            &[],
            &[],
            &["Bash(rm *)"],
            None,
        );
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
        assert!(is_allow(&p.check(
            "Read",
            &json!({"file_path": "a"}),
            true,
            &ctx
        )));
        assert!(matches!(
            p.check("Write", &json!({"file_path": "a"}), false, &ctx),
            PermissionDecision::Deny(_)
        ));
        assert!(matches!(
            p.check("Bash", &json!({"command": "ls"}), false, &ctx),
            PermissionDecision::Deny(_)
        ));

        let p2 = perms(PermissionMode::DontAsk, &[], &["WebFetch"], &[], None);
        assert!(matches!(
            p2.check("WebFetch", &json!({"url": "https://x.test"}), false, &ctx),
            PermissionDecision::Deny(_)
        ));

        let p3 = perms(
            PermissionMode::DontAsk,
            &["Bash(cargo test)"],
            &[],
            &[],
            None,
        );
        assert!(is_allow(&p3.check(
            "Bash",
            &json!({"command": "cargo test"}),
            false,
            &ctx
        )));
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
        assert!(is_allow(&p.check(
            "Bash",
            &json!({"command": "ls"}),
            false,
            &ctx
        )));
        assert!(matches!(
            p.check("Bash", &json!({"command": "rm -rf /"}), false, &ctx),
            PermissionDecision::Deny(_)
        ));

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

        let p3 = perms(
            PermissionMode::Default,
            &["Bash"],
            &["Bash(git push *)"],
            &[],
            None,
        );
        assert!(is_ask(&p3.check(
            "Bash",
            &json!({"command": "git push origin main"}),
            false,
            &ctx
        )));
        assert!(is_allow(&p3.check(
            "Bash",
            &json!({"command": "git status"}),
            false,
            &ctx
        )));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_only_tools_allowed_in_default_and_accept_edits() {
        let dir = tmpdir("readonly-tools");
        let ctx = ctx_for(&dir);
        for mode in [
            PermissionMode::Default,
            PermissionMode::AcceptEdits,
            PermissionMode::DontAsk,
        ] {
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
        assert!(is_ask(&p.check(
            "Write",
            &json!({"file_path": "a"}),
            false,
            &ctx
        )));
        p.add_allow_rule("Write");
        assert!(is_allow(&p.check(
            "Write",
            &json!({"file_path": "a"}),
            false,
            &ctx
        )));

        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert_eq!(saved["model"], json!("keep-me"));
        assert_eq!(saved["permissions"]["deny"], json!(["Bash(rm *)"]));
        let allow = saved["permissions"]["allow"].as_array().unwrap();
        assert!(allow.contains(&json!("Read")));
        assert!(allow.contains(&json!("Write")));

        p.add_allow_rule("Write");
        let saved2: Value =
            serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert_eq!(saved2["permissions"]["allow"].as_array().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_allow_rule_creates_missing_file() {
        let dir = tmpdir("always-new");
        let settings_path = dir.join("nested/deeper/settings.local.json");
        let ctx = ctx_for(&dir);
        let p = perms(
            PermissionMode::Default,
            &[],
            &[],
            &[],
            Some(settings_path.clone()),
        );
        p.add_allow_rule("Bash(cargo test)");
        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert_eq!(saved["permissions"]["allow"], json!(["Bash(cargo test)"]));
        assert!(is_allow(&p.check(
            "Bash",
            &json!({"command": "cargo test"}),
            false,
            &ctx
        )));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_allow_rule_without_path_only_updates_memory() {
        let dir = tmpdir("always-mem");
        let ctx = ctx_for(&dir);
        let p = perms(PermissionMode::Default, &[], &[], &[], None);
        p.add_allow_rule("Edit");
        assert!(is_allow(&p.check(
            "Edit",
            &json!({"file_path": "a"}),
            false,
            &ctx
        )));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn suggested_rules_are_sane() {
        assert_eq!(
            suggested_rule("Bash", &json!({"command": "ls -la"})).unwrap(),
            "Bash(ls -la)"
        );
        assert_eq!(
            suggested_rule("Read", &json!({"file_path": "a.txt"})).unwrap(),
            "Read(a.txt)"
        );
        assert_eq!(
            suggested_rule("WebFetch", &json!({"url": "https://www.example.com/x"})).unwrap(),
            "WebFetch(domain:example.com)"
        );
        assert_eq!(
            suggested_rule("TodoWrite", &json!({})).unwrap(),
            "TodoWrite"
        );
    }

    fn bash(c: &str) -> Value {
        json!({ "command": c })
    }

    #[test]
    fn c2_1_first_match_wins_specificity_ignored() {
        let dir = tmpdir("c21");
        let ctx = ctx_for(&dir);

        let p = perms(
            PermissionMode::Default,
            &["Bash(git push origin main)"],
            &["Bash(git *)"],
            &[],
            None,
        );
        assert!(is_ask(&p.check(
            "Bash",
            &bash("git push origin main"),
            false,
            &ctx
        )));
        let p2 = perms(
            PermissionMode::Default,
            &["Bash(*)"],
            &[],
            &["Bash(curl *)"],
            None,
        );
        assert!(matches!(
            p2.check("Bash", &bash("curl x"), false, &ctx),
            PermissionDecision::Deny(_)
        ));
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
        assert_eq!(
            split_compound("cargo test 2>&1 | tail").unwrap(),
            vec!["cargo test 2>&1", "tail"]
        );
        assert_eq!(split_compound("ls;").unwrap(), vec!["ls"]);
        assert!(split_compound("ls &&").is_none());

        let dir = tmpdir("c24");
        let ctx = ctx_for(&dir);
        let p = perms(
            PermissionMode::Default,
            &["Bash(git status)", "Bash(ls *)"],
            &[],
            &["Bash(rm *)"],
            None,
        );

        assert!(is_allow(&p.check(
            "Bash",
            &bash("git status && ls -la"),
            false,
            &ctx
        )));
        assert!(is_ask(&p.check(
            "Bash",
            &bash("git status && curl x"),
            false,
            &ctx
        )));

        assert!(matches!(
            p.check("Bash", &bash("ls && rm -rf x"), false, &ctx),
            PermissionDecision::Deny(_)
        ));

        let p2 = perms(
            PermissionMode::Default,
            &["Bash"],
            &["Bash(git push *)"],
            &[],
            None,
        );
        assert!(is_ask(&p2.check(
            "Bash",
            &bash("git add . && git push origin"),
            false,
            &ctx
        )));

        assert!(is_ask(&p.check(
            "Bash",
            &bash("git status &&"),
            false,
            &ctx
        )));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn c2_7_path_rule_forms() {
        let cwd = PathBuf::from("/work/proj");
        let f = |p: &str| json!({ "file_path": p });

        assert!(rule_matches("Read(/src/**)", "Read", &f("src/a.rs"), &cwd));
        assert!(!rule_matches(
            "Read(/src/**)",
            "Read",
            &f("/src/a.rs"),
            &cwd
        ));
        assert!(rule_matches(
            "Read(//src/**)",
            "Read",
            &f("/src/a.rs"),
            &cwd
        ));

        assert!(rule_matches("Read(.env)", "Read", &f(".env"), &cwd));
        assert!(rule_matches("Read(.env)", "Read", &f("a/b/.env"), &cwd));
        assert!(rule_matches("Read(*.log)", "Read", &f("logs/x.log"), &cwd));
        assert!(!rule_matches("Read(.env)", "Read", &f("a/.envrc"), &cwd));

        assert!(rule_matches(
            "Edit(src/**)",
            "Write",
            &f("src/new.rs"),
            &cwd
        ));
        assert!(rule_matches("Edit", "Write", &f("x"), &cwd));
    }

    #[test]
    fn c2_7_read_deny_guards_edit_write_grep_glob() {
        let dir = tmpdir("c27");
        let ctx = ctx_for(&dir);
        let p = perms(
            PermissionMode::BypassPermissions,
            &[],
            &[],
            &["Read(./secrets/**)"],
            None,
        );
        for (tool, input) in [
            ("Read", json!({"file_path": "secrets/k"})),
            ("Edit", json!({"file_path": "secrets/k"})),
            ("Write", json!({"file_path": "secrets/k", "content": ""})),
            ("Grep", json!({"pattern": "x", "path": "secrets"})),
            ("Glob", json!({"pattern": "*", "path": "secrets/sub"})),
        ] {
            assert!(
                matches!(
                    p.check(tool, &input, false, &ctx),
                    PermissionDecision::Deny(_)
                ),
                "{} should be denied",
                tool
            );
        }
        assert!(is_allow(&p.check(
            "Write",
            &json!({"file_path": "src/k"}),
            false,
            &ctx
        )));

        let p2 = perms(PermissionMode::Default, &["Read(./src/**)"], &[], &[], None);
        assert!(is_ask(&p2.check(
            "Write",
            &json!({"file_path": "src/k"}),
            false,
            &ctx
        )));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn c2_8_webfetch_domain_forms() {
        let cwd = PathBuf::from("/w");
        let m = |r: &str, u: &str| rule_matches(r, "WebFetch", &json!({ "url": u }), &cwd);
        assert!(m("WebFetch(domain:Example.COM)", "https://EXAMPLE.com/x"));
        assert!(m(
            "WebFetch(domain:*.example.com)",
            "https://api.example.com/x"
        ));
        assert!(m(
            "WebFetch(domain:*.example.com)",
            "https://a.b.example.com/x"
        ));
        assert!(!m(
            "WebFetch(domain:*.example.com)",
            "https://example.com/x"
        ));
        assert!(!m(
            "WebFetch(domain:*.example.com)",
            "https://badexample.com/x"
        ));
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

        let p = perms(PermissionMode::Default, &["*"], &["mcp__*"], &[], None);
        assert!(is_ask(&p.check("mcp__srv__q", &v, false, &ctx)));
        assert!(
            is_ask(&p.check("Bash", &bash("ls"), false, &ctx)),
            "allow `*` must not match"
        );
        let p2 = perms(PermissionMode::BypassPermissions, &[], &[], &["*"], None);
        assert!(matches!(
            p2.check("Bash", &bash("ls"), false, &ctx),
            PermissionDecision::Deny(_)
        ));
        let p3 = perms(PermissionMode::Default, &["mcp__srv__*"], &[], &[], None);
        assert!(is_allow(&p3.check("mcp__srv__q", &v, false, &ctx)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn c2_11_mode_aliases_and_accept_edits_fs_scope() {
        assert_eq!(
            PermissionMode::parse("manual"),
            Some(PermissionMode::Default)
        );
        assert_eq!(PermissionMode::parse("auto"), Some(PermissionMode::Default));
        let dir = tmpdir("c211");
        let ctx = ctx_for(&dir);
        let p = perms(PermissionMode::AcceptEdits, &[], &[], &[], None);
        assert!(is_allow(&p.check(
            "Bash",
            &bash("mkdir -p a/b && touch a/b/c"),
            false,
            &ctx
        )));
        assert!(is_ask(&p.check(
            "Bash",
            &bash("cp a /etc/passwd"),
            false,
            &ctx
        )));
        assert!(is_ask(&p.check(
            "Bash",
            &bash("mv a ../outside"),
            false,
            &ctx
        )));
        assert!(is_ask(&p.check(
            "Bash",
            &bash("mkdir a && rm -rf b"),
            false,
            &ctx
        )));
        assert!(is_ask(&p.check(
            "Bash",
            &bash("touch $(whoami)"),
            false,
            &ctx
        )));
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
        assert_eq!(
            suggested_rules("Write", &json!({"file_path": "a"})),
            vec!["Write(a)"]
        );

        let dir = tmpdir("c212");
        let path = dir.join(".zen/settings.local.json");
        let ctx = ctx_for(&dir);
        let p = perms(PermissionMode::Default, &[], &[], &[], Some(path.clone()));
        for r in suggested_rules("Bash", &bash("git add . && git commit -m x")) {
            p.add_allow_rule(&r);
        }
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved["permissions"]["allow"].as_array().unwrap().len(), 2);
        assert!(is_allow(&p.check(
            "Bash",
            &bash("git add . && git commit -m x"),
            false,
            &ctx
        )));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn c2_13_ask_rule_detection() {
        let dir = tmpdir("c213");
        let ctx = ctx_for(&dir);
        let p = perms(
            PermissionMode::Default,
            &[],
            &["Bash(git push *)"],
            &[],
            None,
        );
        assert!(p.ask_rule_matches("Bash", &bash("git push"), &ctx));
        assert!(p.ask_rule_matches("Bash", &bash("ls && git push x"), &ctx));
        assert!(!p.ask_rule_matches("Bash", &bash("ls"), &ctx));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
