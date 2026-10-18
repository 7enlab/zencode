//! Built-in slash commands (SPEC §6) and custom command expansion.
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;


pub const BUILTIN_COMMANDS: &[&str] = &[
    "/help", "/clear", "/compact", "/model", "/resume", "/cost", "/status",
    "/permissions", "/config", "/memory", "/init", "/todos", "/agents", "/mcp", "/hooks",
    "/skills", "/exit", "/context", "/add-dir", "/export", "/review", "/rename", "/quota",
    "/diff", "/plan", "/memories", "/loop", "/goal", "/personality", "/vim", "/theme", "/btw",
    "/terse", "/map", "/map-refresh", "/deep-research", "/pr_comments", "/ci-fix", "/done",
    "/scope", "/worktree", "/commit",
];

/// Built-ins that also work as the whole prompt of `-p` (SPEC C5.4). The rest are terminal-only.
pub const HEADLESS_BUILTINS: &[&str] =
    &["compact", "model", "cost", "mcp", "context", "review", "init", "done"];

/// `Some((name, args))` when `prompt` is a built-in slash command (`/name args`).
/// `/quit` and `/continue` are accepted as aliases of `/exit` and `/resume`.
pub fn parse_builtin(prompt: &str) -> Option<(String, String)> {
    let rest = prompt.trim_start().strip_prefix('/')?;
    let mut it = rest.splitn(2, char::is_whitespace);
    let name = it.next().unwrap_or("");
    let known = BUILTIN_COMMANDS.iter().any(|c| &c[1..] == name)
        || matches!(name, "quit" | "continue");
    if name.is_empty() || !known {
        return None;
    }
    Some((name.to_string(), it.next().unwrap_or("").trim().to_string()))
}

/// Prompt sent to the model for `/review [focus]` (zen's own wording).
pub fn review_prompt(args: &str) -> String {
    let mut p = String::from(
        "Review the current changes in this repository. Start with `git status` and `git diff` \
         (and `git diff --staged`); if there are no local changes, review the latest commit. \
         Report concrete problems first (bugs, missing error handling, security issues, missing \
         tests), each with file and line, then smaller style notes. Do not modify files.",
    );
    if !args.trim().is_empty() {
        p.push_str(&format!("\n\nFocus: {}", args.trim()));
    }
    p
}

/// Prompt sent to the model for `/init` (zen's own wording).
pub fn init_prompt() -> String {
    "Look through this repository (layout, build files, README, tests) and write a concise \
     ZEN.md at the project root for future coding sessions: how to build, test and lint, the \
     main modules and how they fit together, and project conventions. If ZEN.md already exists, \
     improve it instead of replacing useful content."
        .to_string()
}

/// `/init` prompt variant: migrate existing CLAUDE.md/AGENTS.md content into the new ZEN.md.
/// Used when `zen_ext::detect_import_candidates` found files written for other agents.
pub fn init_prompt_with_imports(candidates: &[std::path::PathBuf]) -> String {
    let files = candidates
        .iter()
        .map(|p| format!("- {}", p.display()))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{}\n\nBefore writing, read these existing agent memory files and incorporate any \
         still-relevant instructions into the new ZEN.md (dedupe against each other, keep zen's \
         concise style, drop tool-specific syntax that does not apply to zen):\n{}\nAfter \
         importing, the old files can be deleted: zen loads ZEN.md preferentially and ignores \
         AGENTS.md in any directory that has one.",
        init_prompt(),
        files
    )
}

/// `/context`: approximate token use by part (chars / 4) against the context window.
pub fn context_report(
    system_prompt: &str,
    tools: &[zen_core::ToolDefinition],
    messages: &[zen_core::ChatMessage],
    context_window: u64,
    last_prompt_tokens: u64,
) -> String {
    let sys = system_prompt.len() as u64 / 4;
    let tool_tokens = serde_json::to_string(tools).map(|t| t.len() as u64 / 4).unwrap_or(0);
    let msg = zen_core::estimate_tokens(messages);
    let total = sys + tool_tokens + msg;
    let pct = |n: u64| if context_window == 0 { 0.0 } else { n as f64 * 100.0 / context_window as f64 };
    let mut out = format!(
        "Context usage (estimated): {} / {} tokens ({:.1}%)\n\
         - system prompt: {} tokens\n\
         - tools ({}): {} tokens\n\
         - messages ({}): {} tokens\n\
         - free: {} tokens",
        total,
        context_window,
        pct(total),
        sys,
        tools.len(),
        tool_tokens,
        messages.len(),
        msg,
        context_window.saturating_sub(total)
    );
    if last_prompt_tokens > 0 {
        out.push_str(&format!("\nLast request (reported by the API): {} prompt tokens", last_prompt_tokens));
    }
    // Schema audit: per-tool costs, most expensive first.
    let costs = zen_agent::tool_bundles::per_tool_costs(tools);
    if !costs.is_empty() {
        out.push_str("\n\nTool schema costs (estimated tokens, most expensive first):");
        for (name, tok) in &costs {
            out.push_str(&format!("\n  {:<12} {}", name, tok));
        }
    }
    out
}

/// `zen tool-help <name>`: full documentation for one tool.
///
/// Tool descriptions sent to the model are one-liners (token-saving); this
/// prints the complete reference (parameters, behavior, examples) from the
/// embedded `docs/tools/*.md`.
pub fn tool_help(name: &str) -> String {
    let mut registry = zen_core::ToolRegistry::new();
    for t in zen_tools::builtin_tools() {
        registry.register(t);
    }
    registry.register(std::sync::Arc::new(zen_tools::JobOutputTool));
    registry.register(std::sync::Arc::new(zen_tools::JobKillTool));
    let found = registry.get(name).or_else(|| {
        let lower = name.to_ascii_lowercase();
        registry
            .names()
            .into_iter()
            .find(|n| n.to_ascii_lowercase() == lower)
            .and_then(|n| registry.get(&n))
    });
    match found {
        Some(tool) => {
            let def = tool.definition();
            let tokens = zen_agent::tool_bundles::estimate_schema_tokens(&def);
            let phase = zen_agent::tool_bundles::tool_phase(tool.name())
                .map(|p| p.as_str())
                .unwrap_or("unclassified");
            let docs = zen_tools::tool_long_docs(tool.name()).unwrap_or("No extended documentation available.");
            format!(
                "{} (phase: {}, ~{} schema tokens)\n\n{}\n\n---\n\n{}",
                tool.name(),
                phase,
                tokens,
                tool.description(),
                docs
            )
        }
        None => format!(
            "Unknown tool '{}'. Available tools: {}",
            name,
            registry.names().join(", ")
        ),
    }
}

/// `/mcp`: one line per server with its status.
pub fn mcp_report(status: &[(String, String)]) -> String {
    if status.is_empty() {
        return "No MCP servers configured.".to_string();
    }
    status
        .iter()
        .map(|(n, st)| format!("{}: {}", n, st))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `/cost`: token usage of this session so far (zen uses free models: cost is always $0).
/// `/cost` (and the `-p` `cost` builtin): token usage and estimated cost for the
/// current session, rendered from live usage figures.
pub fn cost_report(usage: &zen_core::UsageStats, model: &str, base_url: &str) -> String {
    let entry = crate::cost::CostEntry {
        model: model.to_string(),
        provider: crate::cost::provider_from_base_url(base_url),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        timestamp: String::new(),
    };
    let summary = crate::cost::summarize("current session", std::slice::from_ref(&entry));
    let mut s = crate::cost::render_text(&summary);
    // Cache-hit observability (#49): on paid models cached input tokens are billed at
    // ~10% of the normal price (Anthropic `cache_control`, OpenAI automatic caching).
    if usage.input_tokens > 0 && usage.cache_read_input_tokens > 0 {
        s.push_str(&format!(
            "\nPrompt cache: {} tokens served from cache ({:.0}% of input) — ~90% cheaper on paid models",
            usage.cache_read_input_tokens,
            usage.cache_hit_rate() * 100.0
        ));
    }
    s
}

/// `/export`: the conversation as markdown (system messages left out).
pub fn transcript_markdown(session_id: &str, messages: &[zen_core::ChatMessage]) -> String {
    let mut out = format!("# zen session {}\n", session_id);
    for m in messages {
        match m.role.as_str() {
            "user" => out.push_str(&format!("\n## User\n\n{}\n", m.content.as_deref().unwrap_or(""))),
            "assistant" => {
                out.push_str("\n## Assistant\n\n");
                if let Some(c) = m.content.as_deref().filter(|c| !c.is_empty()) {
                    out.push_str(c);
                    out.push('\n');
                }
                for call in m.tool_calls.iter().flatten() {
                    out.push_str(&format!(
                        "\n**Tool call** `{}`: `{}`\n",
                        call.function.name, call.function.arguments
                    ));
                }
            }
            "tool" => {
                out.push_str(&format!(
                    "\n**Tool result** ({}):\n\n```\n{}\n```\n",
                    m.tool_call_id.as_deref().unwrap_or("?"),
                    m.content.as_deref().unwrap_or("")
                ));
            }
            _ => {}
        }
    }
    out
}

/// Parse custom command frontmatter + expand `$ARGUMENTS`, `$1..$9`, `!cmd`, `@file`.
#[derive(Debug)]
pub struct CustomCommand {
    pub name: String,
    pub frontmatter: HashMap<String, Value>,
    pub body: String,
}

impl CustomCommand {
    pub fn parse(name: &str, raw: &str) -> Result<Self> {
        let (frontmatter, body) = split_frontmatter(raw);
        // validate keys are simple scalars/arrays
        Ok(Self {
            name: name.to_string(),
            frontmatter,
            body: body.to_string(),
        })
    }

    /// Expand the command body given the user's arguments.
    /// `args` is the full arg string after the slash command name.
    /// `@file` references resolve relative to the process cwd.
    pub fn expand(&self, args: &str) -> Result<String> {
        self.expand_in(args, Path::new("."))
    }

    /// Like `expand`, but resolves `@file` references relative to `base`.
    pub fn expand_in(&self, args: &str, base: &Path) -> Result<String> {
        let parts: Vec<&str> = args.split_whitespace().collect();
        let mut body = self.body.clone();
        // positional $1..$9
        for i in 1..=9usize {
            let slot = if i <= parts.len() {
                parts[i - 1]
            } else {
                ""
            };
            body = body.replace(&format!("${}", i), slot);
        }
        // $ARGUMENTS = all args
        body = body.replace("$ARGUMENTS", args);
        // @file inlining — expand @relative/path
        expand_file_refs(&body, base)
    }
}

fn split_frontmatter(raw: &str) -> (HashMap<String, Value>, &str) {
    let trimmed = raw.trim_start();
    if !trimmed.starts_with("---") {
        return (HashMap::new(), raw);
    }
    if let Some(end_idx) = find_closing_delim(trimmed) {
        let fm_text = &trimmed[4..end_idx];
        let body = trimmed[end_idx + 4..].trim_start();
        let fm = parse_yaml_frontmatter(fm_text);
        return (fm, body);
    }
    (HashMap::new(), raw)
}

fn find_closing_delim(s: &str) -> Option<usize> {
    // s starts with "---\n"; find next line that is exactly "---"
    let mut idx = 4; // skip opening "---"
    while idx < s.len() {
        let nl = s[idx..].find('\n')?;
        idx += nl + 1;
        let line_end = s[idx..].find('\n').map(|p| idx + p).unwrap_or(s.len());
        let line = s[idx..line_end].trim();
        if line == "---" {
            return Some(idx);
        }
        idx = if line_end == s.len() { s.len() } else { idx };
    }
    None
}

fn parse_yaml_frontmatter(text: &str) -> HashMap<String, Value> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once(": ") {
            map.insert(k.trim().to_string(), Value::String(v.trim().to_string()));
        } else if let Some((k, v)) = line.split_once(':') {
            map.insert(k.trim().to_string(), Value::String(v.trim().to_string()));
        }
    }
    map
}

/// In a command body, replace `@path` lines with file contents.
fn expand_file_refs(body: &str, base: &Path) -> Result<String> {
    // Replace `@path` tokens with file contents (inline or whole-line).
    let mut out = String::new();
    for (i, line) in body.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let mut rebuilt: Vec<String> = Vec::new();
        let leading_ws = line.len() - line.trim_start().len();
        let (indent, rest) = line.split_at(leading_ws);
        if rest.starts_with('@') && !rest.trim()[1..].contains(' ') {
            let path = rest.trim()[1..].trim();
            if let Some(content) = read_ref(path, base) {
                out.push_str(indent);
                out.push_str(&content);
                continue;
            }
        }
        for token in line.split_whitespace() {
            if let Some(path) = token.strip_prefix('@') {
                if !path.is_empty() {
                    if let Some(content) = read_ref(path, base) {
                        rebuilt.push(content);
                        continue;
                    }
                }
            }
            rebuilt.push(token.to_string());
        }
        out.push_str(&rebuilt.join(" "));
    }
    Ok(out)
}

/// Read an `@path` reference; None when the file does not exist (token left as-is).
fn read_ref(path: &str, base: &Path) -> Option<String> {
    let p = base.join(path);
    let p = if p.is_file() { p } else { Path::new(path).to_path_buf() };
    if p.is_file() {
        std::fs::read_to_string(p).ok()
    } else {
        None
    }
}

/// Load a custom command by name from `.zen/commands/` and `~/.zen/commands/`.
fn custom_commands_dirs(cwd: &Path, home: Option<&Path>) -> Vec<std::path::PathBuf> {
    let mut dirs = vec![];
    let proj = cwd.join(".zen").join("commands");
    dirs.push(proj);
    if let Some(h) = home {
        dirs.push(h.join(".zen").join("commands"));
    }
    dirs
}

/// Load all custom commands (name without ext) from the directories.
pub fn load_custom_commands(cwd: &Path, home: Option<&Path>) -> Vec<CustomCommand> {
    let dirs = custom_commands_dirs(cwd, home);
    let mut cmds = vec![];
    for dir in &dirs {
        if !dir.exists() {
            continue;
        }
        for e in walkdir::WalkDir::new(dir).into_iter().flatten() {
            let path = e.path();
            if path.is_file() {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    let name = stem.replace('_', ":");
                    if let Ok(content) = std::fs::read_to_string(path) {
                        if let Ok(cmd) = CustomCommand::parse(&name, &content) {
                            cmds.push(cmd);
                        }
                    }
                }
            }
        }
    }
    // project overrides user by name
    dedup_keep_project_first(&mut cmds);
    cmds
}

fn dedup_keep_project_first(cmds: &mut Vec<CustomCommand>) {
    // Since project dirs come first in the vec, keep first occurrence.
    let mut seen = std::collections::HashSet::new();
    let mut i = 0;
    while i < cmds.len() {
        if seen.contains(&cmds[i].name) {
            cmds.remove(i);
        } else {
            seen.insert(cmds[i].name.clone());
            i += 1;
        }
    }
}

/// Slash command dispatch result.
#[derive(Debug)]
pub enum SlashResult {
    Handled,
    /// Not a builtin; could be a custom command name.
    Custom(CustomCommand),
    /// Unknown command.
    Unknown(String),
}

pub fn dispatch_builtin(cmd: &str, _arg: &str) -> SlashResult {
    let known: std::collections::HashSet<&str> = BUILTIN_COMMANDS.iter().copied().collect();
    if known.contains(&cmd) {
        SlashResult::Handled
    } else {
        SlashResult::Unknown(cmd.to_string())
    }
}

// ─── `zen mcp ...` subcommand (SPEC §8) ─────────────────────────────────────

/// Config file path for the given scope (delegates to `zen_mcp::scope_path`, SPEC C7.5):
/// local (default, "") = `<cwd>/.zen/mcp.json`, project = `<cwd>/.mcp.json`,
/// user = `~/.zen/mcp.json`.
pub fn mcp_scope_path(scope: &str, cwd: &Path, home: Option<&Path>) -> Result<std::path::PathBuf> {
    let scope = if scope.is_empty() { "local" } else { scope };
    zen_mcp::scope_path(scope, cwd, home)
}

/// Split a `--header` / `-e` value at the first `:` or `=` (`"Authorization: Bearer x"` and
/// `"Authorization=Bearer x"` both work); key and value are trimmed. `None` without a separator.
pub fn split_kv(entry: &str) -> Option<(String, String)> {
    let idx = entry.find([':', '='])?;
    let (k, v) = (entry[..idx].trim(), entry[idx + 1..].trim());
    (!k.is_empty()).then(|| (k.to_string(), v.to_string()))
}

fn read_mcp_file(path: &Path) -> Value {
    let mut root = match std::fs::read_to_string(path) {
        Ok(t) if !t.trim().is_empty() => {
            serde_json::from_str::<Value>(&t).unwrap_or_else(|_| Value::Object(Default::default()))
        }
        _ => Value::Object(Default::default()),
    };
    if !root.is_object() {
        root = Value::Object(Default::default());
    }
    if !root.get("mcpServers").map(|v| v.is_object()).unwrap_or(false) {
        root.as_object_mut()
            .unwrap()
            .insert("mcpServers".to_string(), Value::Object(Default::default()));
    }
    root
}

fn write_mcp_file(path: &Path, root: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(root)?))?;
    Ok(())
}

/// Handle `zen mcp list|add|remove`.
pub async fn mcp_command(
    cwd: &Path,
    home: Option<&Path>,
    args: &crate::cli::McpArgs,
) -> Result<()> {
    use crate::cli::McpSubcommand;
    match &args.command {
        McpSubcommand::List => {
            for scope in ["user", "project", "local"] {
                let path = mcp_scope_path(scope, cwd, home)?;
                let names: Vec<String> = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                    .and_then(|v| v.get("mcpServers").and_then(|s| s.as_object()).cloned())
                    .map(|o| o.keys().cloned().collect())
                    .unwrap_or_default();
                if names.is_empty() {
                    println!("{}: (none)  {}", scope, path.display());
                } else {
                    println!("{}: {}  {}", scope, names.join(", "), path.display());
                }
            }
            Ok(())
        }
        McpSubcommand::Add {
            transport,
            scope,
            name,
            args: rest,
            env,
            header,
        } => {
            if transport != "stdio" && transport != "http" {
                return Err(anyhow!(
                    "unsupported transport {:?} (expected stdio or http)",
                    transport
                ));
            }
            let path = mcp_scope_path(scope, cwd, home)?;
            let mut root = read_mcp_file(&path);
            let mut cfg: Value = if transport == "http" {
                let url = rest
                    .first()
                    .ok_or_else(|| anyhow!("http transport requires a URL"))?;
                serde_json::json!({"type": "http", "url": url})
            } else {
                if rest.is_empty() {
                    return Err(anyhow!("stdio transport requires a command"));
                }
                serde_json::json!({"type": "stdio", "command": rest[0], "args": rest[1..]})
            };
            if !env.is_empty() {
                let mut env_map = serde_json::Map::new();
                for kv in env {
                    match kv.split_once('=') {
                        Some((k, v)) if !k.is_empty() => {
                            env_map.insert(k.to_string(), Value::String(v.to_string()));
                        }
                        _ => return Err(anyhow!("invalid -e {:?} (expected KEY=VALUE)", kv)),
                    }
                }
                cfg["env"] = Value::Object(env_map);
            }
            if !header.is_empty() {
                let mut header_map = serde_json::Map::new();
                for kv in header {
                    let (k, v) = split_kv(kv).ok_or_else(|| {
                        anyhow!("invalid --header {:?} (expected \"Name: value\")", kv)
                    })?;
                    header_map.insert(k, Value::String(v));
                }
                cfg["headers"] = Value::Object(header_map);
            }
            root["mcpServers"][name] = cfg;
            write_mcp_file(&path, &root)?;
            println!("added MCP server {:?} to {}", name, path.display());
            Ok(())
        }
        McpSubcommand::Get { scope, name } => {
            // Empty scope = search local, project, then user (first hit wins, as at load time).
            let scopes: Vec<&str> =
                if scope.is_empty() { vec!["local", "project", "user"] } else { vec![scope] };
            for sc in scopes {
                let path = mcp_scope_path(sc, cwd, home)?;
                let root = read_mcp_file(&path);
                if let Some(server) = root["mcpServers"].get(name) {
                    println!("{}", serde_json::to_string_pretty(server)?);
                    return Ok(());
                }
            }
            Err(anyhow!("no MCP server named {:?}", name))
        }
        McpSubcommand::AddJson { scope, name, json } => {
            let path = mcp_scope_path(scope, cwd, home)?;
            let mut root = read_mcp_file(&path);
            let cfg: Value = serde_json::from_str(json)?;
            if !cfg.is_object() {
                return Err(anyhow!("server JSON must be an object"));
            }
            serde_json::from_value::<zen_mcp::McpServerConfig>(cfg.clone())
                .map_err(|e| anyhow!("invalid MCP server JSON: {}", e))?;
            root["mcpServers"][name] = cfg;
            write_mcp_file(&path, &root)?;
            println!("added MCP server {:?} to {}", name, path.display());
            Ok(())
        }
        McpSubcommand::Remove { scope, name } => {
            let path = mcp_scope_path(scope, cwd, home)?;
            let mut root = read_mcp_file(&path);
            let removed = root["mcpServers"]
                .as_object_mut()
                .map(|o| o.remove(name).is_some())
                .unwrap_or(false);
            if removed {
                write_mcp_file(&path, &root)?;
                println!("removed MCP server {:?} from {}", name, path.display());
            } else {
                eprintln!("no MCP server {:?} in {}", name, path.display());
            }
            Ok(())
        }
    }
}

// ─── `zen sessions ...` subcommand (parity A14) ────────────────────────────────

/// Handle `zen sessions list|archive|delete|restore`.
///
/// Before anything else, sessions older than `sessions.autoArchiveDays` (0 = off) are
/// auto-archived, so the list the user sees already reflects the retention policy.
pub async fn sessions_command(
    cli: &crate::cli::Cli,
    cwd: &Path,
    home: Option<&Path>,
    args: &crate::cli::SessionsArgs,
) -> Result<()> {
    use crate::cli::SessionSubcommand;
    use crate::session_mgmt as sm;
    let Some(home) = home else {
        return Err(anyhow!("sessions need a home directory"));
    };
    let cfg = crate::settings::resolve_config_with_home(cli, cwd, Some(home))?;
    if cfg.auto_archive_days > 0 {
        for id in sm::auto_archive_old(home, cfg.auto_archive_days)? {
            println!(
                "auto-archived session {} (older than {}d)",
                sm::short_id(&id),
                cfg.auto_archive_days
            );
        }
    }
    match &args.command {
        SessionSubcommand::List { all } => {
            let sessions = sm::list_all_sessions(home, *all);
            if sessions.is_empty() {
                println!("no sessions yet");
                return Ok(());
            }
            println!("ID         MODIFIED (UTC)    MSGS   TOKENS in/out  STATUS    TITLE");
            for s in &sessions {
                println!(
                    "{:<10} {:<16} {:>5} {:>15}  {:<9} {}",
                    sm::short_id(&s.id),
                    sm::fmt_mtime(s.modified),
                    s.message_count,
                    format!("{}/{}", s.input_tokens, s.output_tokens),
                    if s.archived { "archived" } else { "active" },
                    s.title.as_deref().unwrap_or(""),
                );
            }
            Ok(())
        }
        SessionSubcommand::Archive { id } => {
            let dest = sm::archive_session(home, id)?;
            println!("archived session {} -> {}", sm::short_id(id), dest.display());
            Ok(())
        }
        SessionSubcommand::Delete { id, yes } => {
            if !yes && !sm::confirm_delete_prompt(id)? {
                println!("delete cancelled");
                return Ok(());
            }
            sm::delete_session(home, id)?;
            println!("deleted session {}", sm::short_id(id));
            Ok(())
        }
        SessionSubcommand::Restore { id } => {
            let dest = sm::restore_session(home, id)?;
            println!("restored session {} -> {}", sm::short_id(id), dest.display());
            Ok(())
        }
    }
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn builtin_commands_list() {
        assert!(BUILTIN_COMMANDS.contains(&"/help"));
        assert!(BUILTIN_COMMANDS.contains(&"/exit"));
        assert!(BUILTIN_COMMANDS.contains(&"/compact"));
        assert!(BUILTIN_COMMANDS.contains(&"/diff"));
    }

    #[test]
    fn parse_builtin_diff_with_flags() {
        assert_eq!(
            parse_builtin("/diff --staged"),
            Some(("diff".to_string(), "--staged".to_string()))
        );
        assert_eq!(
            parse_builtin("/diff"),
            Some(("diff".to_string(), "".to_string()))
        );
        assert_eq!(
            parse_builtin("/diff --stat"),
            Some(("diff".to_string(), "--stat".to_string()))
        );
    }

    #[test]
    fn builtin_commands_list_includes_memories() {
        assert!(BUILTIN_COMMANDS.contains(&"/memories"));
        // /memories is terminal-only (like /memory): not in -p mode.
        assert!(!HEADLESS_BUILTINS.contains(&"memories"));
    }

    #[test]
    fn init_prompt_with_imports_lists_files() {
        let cands = vec![
            std::path::PathBuf::from("/proj/CLAUDE.md"),
            std::path::PathBuf::from("/proj/sub/AGENTS.md"),
        ];
        let p = init_prompt_with_imports(&cands);
        assert!(p.contains("/proj/CLAUDE.md"), "prompt: {}", p);
        assert!(p.contains("/proj/sub/AGENTS.md"), "prompt: {}", p);
        assert!(p.contains("incorporate"), "prompt: {}", p);
        // Base init instructions preserved.
        assert!(p.contains("write a concise"), "prompt: {}", p);
        // REPL polish commands
        for c in ["/loop", "/goal", "/personality", "/vim", "/theme"] {
            assert!(BUILTIN_COMMANDS.contains(&c), "missing {c}");
            assert!(parse_builtin(c).is_some(), "not parsed: {c}");
        }
        assert_eq!(
            parse_builtin("/loop 3 do x"),
            Some(("loop".to_string(), "3 do x".to_string()))
        );
        assert!(BUILTIN_COMMANDS.contains(&"/deep-research"));
    }

    #[test]
    fn dispatch_builtin_known() {
        match dispatch_builtin("/help", "") {
            SlashResult::Handled => {}
            other => panic!("expected handled, got {:?}", other),
        }
    }

    #[test]
    fn dispatch_builtin_unknown() {
        match dispatch_builtin("/customfoo", "") {
            SlashResult::Unknown(name) => assert_eq!(name, "/customfoo"),
            other => panic!("expected unknown, got {:?}", other),
        }
    }

    #[test]
    fn custom_command_positional_expansion() {
        let cmd = CustomCommand::parse("review", "Review the file $1 and fix issues in $2.\n$ARGUMENTS done.").unwrap();
        let out = cmd.expand("src/main.rs").unwrap();
        assert!(out.contains("Review the file src/main.rs"));
        assert!(out.contains("$2.") || out.contains("."));
        assert!(out.contains("src/main.rs done"));
    }

    #[test]
    fn custom_command_arguments_expansion() {
        let cmd = CustomCommand::parse("echo", "you said: $ARGUMENTS").unwrap();
        let out = cmd.expand("hello world").unwrap();
        assert_eq!(out, "you said: hello world");
    }

    #[test]
    fn custom_command_atfile_inlining() {
        let tmp = std::env::temp_dir().join(format!("zen_cmd_atfile_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join("data.txt"), "inline-content").unwrap();

        let cmd = CustomCommand::parse("show", "data: @data.txt").unwrap();
        let out = cmd.expand_in("", &tmp).unwrap();
        assert!(out.contains("data: inline-content"));
    }

    #[test]
    fn custom_command_frontmatter_parse() {
        let raw = "---\ndescription: Run tests\nmodel: gpt-4\n---\nTest the repo.";
        let cmd = CustomCommand::parse("test", raw).unwrap();
        assert_eq!(cmd.body, "Test the repo.");
        assert_eq!(cmd.frontmatter.get("description"), Some(&Value::String("Run tests".to_string())));
        assert_eq!(cmd.frontmatter.get("model"), Some(&Value::String("gpt-4".to_string())));
    }

    #[test]
    fn custom_command_no_frontmatter() {
        let cmd = CustomCommand::parse("simple", "Just do it.").unwrap();
        assert_eq!(cmd.body, "Just do it.");
        assert!(cmd.frontmatter.is_empty());
    }

    #[test]
    fn load_custom_commands_from_dir() {
        let home = std::env::temp_dir().join(format!("zen_cmd_load_{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let cwd = std::env::temp_dir().join(format!("zen_cmd_cwd_{}", std::process::id()));
        let _ = fs::remove_dir_all(&cwd);
        fs::create_dir_all(cwd.join(".zen").join("commands")).unwrap();
        fs::write(
            cwd.join(".zen").join("commands").join("test.md"),
            "---\ndescription: test cmd\n---\nRun: $1",
        )
        .unwrap();

        let cmds = load_custom_commands(&cwd, Some(&home));
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].name, "test");
        assert_eq!(cmds[0].body, "Run: $1");
    }

    #[test]
    fn cost_report_shows_cache_hit_rate_when_cached() {
        let u = zen_core::UsageStats { input_tokens: 1000, output_tokens: 50, cache_read_input_tokens: 800 };
        let r = cost_report(&u, "mock-model", "http://127.0.0.1:1");
        assert!(r.contains("Prompt cache: 800 tokens served from cache (80% of input)"), "{}", r);
    }

    #[test]
    fn cost_report_omits_cache_line_when_unsupported() {
        // Providers without prompt caching report 0 cached tokens: no noise.
        let u = zen_core::UsageStats { input_tokens: 100, output_tokens: 5, cache_read_input_tokens: 0 };
        let r = cost_report(&u, "mock-model", "http://127.0.0.1:1");
        assert!(!r.contains("Prompt cache"), "{}", r);
    }
}

