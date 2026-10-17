//! Assembles the fully-wired agent from all crates and runs headless turns (SPEC §1/§3/§14).
//!
//! This is the orchestration layer of zen-cli: it knows how to turn flags + resolved settings
//! into a live [`zen_agent::Agent`] (tool registry, permissions, hooks, MCP servers, skills,
//! sub-agents) and how to print the result in the three headless formats.
//!
//! Design decisions worth knowing about:
//! - **No duplicate settings logic**: layering lives in `zen_ext::load_settings`; here we only
//!   apply CLI-level precedence ([`crate::settings`]).
//! - **Permissions are layered twice on purpose**: a ["core"](zen_core) `RulePermissions` instance
//!   receives the settings + CLI rules, and we wrap it in [`ClaimingPermissions`], which uses the
//!   optional [`Prompter`] only for tools that can actually run in this configuration. In headless
//!   `-p` mode there is no prompter, so anything that would ask is denied and recorded.
//! - **Plan mode hides mutating tools** (B4) rather than denying them at call time, so the model
//!   never even sees them in the tool list.

use crate::cli::{Cli, OutputFormat};
use crate::commands;
use crate::output::{print_result, Buffered, StreamJsonSink};
use crate::settings::ResolvedConfig;
use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use zen_core::{
    AgentEvent, EventSink, HookRunner, NoHooks, PermissionChecker,
    PermissionDenial, PermissionMode, Prompter, RunResult, Tool, ToolContext, ToolRegistry,
};

/// Everything an agent run needs, built once.
pub struct AgentAssembly {
    pub registry: ToolRegistry,
    pub permissions: Arc<dyn PermissionChecker>,
    pub hooks: Arc<dyn HookRunner>,
    pub mcp_status: Vec<(String, String)>,
    /// MCP entries skipped by validation: (server name, reason such as `url_missing_type`).
    pub mcp_errors: Vec<(String, String)>,
    pub slash_commands: Vec<String>,
    pub skills: Vec<zen_core::SkillInfo>,
    /// When `--agent <name>` selected a definition, the body of that definition
    /// (used as the main system prompt) is exposed here for the caller.
    pub agent_system_prompt: Option<String>,
    /// Sink the sub-agent executor (Task tool) emits through. It starts as a no-op because the
    /// run's real sink is created after assembly; [`ForwardSink::set`] points it there so
    /// sub-agent messages reach stream-json with their `parent_tool_use_id` (SPEC C6.4).
    pub subagent_sink: Arc<ForwardSink>,
    /// Records every model request/response into the session file (p0b); None when the session
    /// is not persisted.
    pub recorder: Option<Arc<crate::eventlog::Recorder>>,
}

/// An [`EventSink`] whose target can be set after construction (no-op until then).
#[derive(Default)]
pub struct ForwardSink {
    target: std::sync::Mutex<Option<Arc<dyn EventSink>>>,
}

impl ForwardSink {
    pub fn set(&self, sink: Arc<dyn EventSink>) {
        *self.target.lock().unwrap() = Some(sink);
    }
}

impl EventSink for ForwardSink {
    fn emit(&self, ev: AgentEvent) {
        let target = self.target.lock().unwrap().clone();
        if let Some(t) = target {
            t.emit(ev);
        }
    }
}

/// Convert our clap enum into zen-core's permission mode.
pub fn core_mode(m: crate::cli::PermissionMode) -> PermissionMode {
    match m {
        crate::cli::PermissionMode::Default => PermissionMode::Default,
        crate::cli::PermissionMode::AcceptEdits => PermissionMode::AcceptEdits,
        crate::cli::PermissionMode::Plan => PermissionMode::Plan,
        crate::cli::PermissionMode::BypassPermissions => PermissionMode::BypassPermissions,
        crate::cli::PermissionMode::DontAsk => PermissionMode::DontAsk,
    }
}

/// Prompter used by the REPL: ask on stderr, read a line from stdin.
pub struct ReplPrompter;

impl Prompter for ReplPrompter {
    fn ask_permission(&self, tool_name: &str, input: &serde_json::Value) -> zen_core::PromptAnswer {
        use std::io::Write;
        eprint!(
            "Allow {} {}? [y]es / [a]lways / [n]o: ",
            tool_name,
            input
        );
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return zen_core::PromptAnswer::No;
        }
        match line.trim().to_lowercase().as_str() {
            "y" | "yes" => zen_core::PromptAnswer::Yes,
            "a" | "always" => {
                // One rule per sub-command of a compound Bash command, one per line (C2.12).
                let rules = zen_tools::suggested_rules(tool_name, input);
                let rule = if rules.is_empty() { tool_name.to_string() } else { rules.join("\n") };
                zen_core::PromptAnswer::Always(rule)
            }
            _ => zen_core::PromptAnswer::No,
        }
    }
}

/// Build the tool registry, permissions and hooks per SPEC §1/§3/§4.
/// Built-in tools selected by `--tools` (case-insensitive names):
/// * absent or `default` → all built-ins; `""` → none;
/// * `Read,Bash` → exactly those (allowlist);
/// * only `+Name`/`-Name` entries (e.g. `-Bash,-Write`) → the default set with those added or
///   removed — a delta form borrowed from Pi's `--tools` (docs/research/pi-and-oh-my-pi.md), so
///   "everything but Bash" no longer needs every other name spelled out.
///
/// Mixing plain names with `+`/`-` entries is an error, as is a `+`/`-` name that is not a built-in
/// (a typo would otherwise silently keep or drop the wrong tool).
pub fn select_builtin_tools(spec: Option<&str>, all: Vec<Arc<dyn Tool>>) -> Result<Vec<Arc<dyn Tool>>> {
    let list = match spec {
        None | Some("default") => return Ok(all),
        Some("") => return Ok(vec![]),
        Some(list) => list,
    };
    let entries: Vec<&str> = list.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    let deltas = entries.iter().filter(|e| e.starts_with('+') || e.starts_with('-')).count();
    if deltas == 0 {
        return Ok(all.into_iter().filter(|t| entries.iter().any(|a| a.eq_ignore_ascii_case(t.name()))).collect());
    }
    if deltas != entries.len() {
        anyhow::bail!("--tools: cannot mix plain names with +name/-name entries: {list}");
    }
    let mut keep: Vec<bool> = vec![true; all.len()];
    for e in &entries {
        let (add, name) = (e.starts_with('+'), &e[1..]);
        let Some(i) = all.iter().position(|t| t.name().eq_ignore_ascii_case(name)) else {
            anyhow::bail!("--tools: unknown built-in tool '{name}'");
        };
        keep[i] = add;
    }
    Ok(all.into_iter().zip(keep).filter(|(_, k)| *k).map(|(t, _)| t).collect())
}

#[allow(clippy::too_many_arguments)]
pub async fn assemble(
    cli: &Cli,
    cwd: &Path,
    home: Option<&Path>,
    cfg: &ResolvedConfig,
    session_id: &str,
    transcript_path: Option<&Path>,
) -> Result<AgentAssembly> {
    // ── session event log (p0b): shared by the main agent and sub-agents ──
    let recorder = transcript_path.map(|p| crate::eventlog::Recorder::new(p.to_path_buf()));

    // ── tools ────────────────────────────────────────────────────────────
    let mut registry = ToolRegistry::new();
    let all_builtins = zen_tools::builtin_tools();
    let selected = select_builtin_tools(cli.tools.as_deref(), all_builtins)?;
    for t in selected {
        registry.register(t);
    }
    // Background-job tools come with Bash (p0b).
    if registry.get("Bash").is_some() {
        for t in zen_tools::job_tools() {
            registry.register(t);
        }
    }

    // ── skills (skipped in --bare) ────────────────────────────────────────
    let mut mcp_status: Vec<(String, String)> = vec![];
    let mut mcp_errors: Vec<(String, String)> = vec![];
    let mut skills: Vec<zen_core::SkillInfo> = vec![];
    if !cli.bare {
        skills = zen_ext::discover_skills(cwd, home);
        if cli.tools.as_deref() != Some("") {
            registry.register(Arc::new(zen_ext::SkillTool::new(skills.clone())));
        }
    }

    // ── MCP: --bare skips auto-discovery (project/user files) but still honours explicit
    // --mcp-config entries, i.e. it behaves like --strict-mcp-config (SPEC §1). ──────────
    if !cli.bare || !cli.mcp_config.is_empty() {
        let mcp_configs = match zen_mcp::load_mcp_configs(
            cwd,
            home,
            &cli.mcp_config,
            cli.strict_mcp_config || cli.bare,
        ) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("warning: MCP config ignored: {}", e);
                Default::default()
            }
        };
        let timeout_ms = std::env::var("MCP_TIMEOUT")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(30_000);
        let manager = zen_mcp::McpManager::start(mcp_configs, timeout_ms).await;
        mcp_status = manager.status();
        mcp_errors = manager
            .errors()
            .into_iter()
            .map(|e| (e.server_name, e.reason))
            .collect();
        // Invalid entries are reported in stream-json init; a human at a terminal also gets a
        // warning on stderr (SPEC C7.3). Piped stderr stays clean for scripts.
        {
            use std::io::IsTerminal;
            if std::io::stderr().is_terminal() {
                for (name, reason) in &mcp_errors {
                    eprintln!("warning: MCP server '{}' skipped: {}", name, reason);
                }
            }
        }
        for t in manager.tools() {
            registry.register(t);
        }
        // The registered tools keep their server connections alive for the whole run; the
        // manager itself is not shut down here (that would kill the servers the tools use).
    }

    // ── --allowedTools: bare names restrict the tool set (SPEC §1, §7) ────
    // Rules with an argument pattern (e.g. `Bash(git status)`) are permission rules and are
    // handled by RulePermissions, not by removing the tool.
    let allowed_bare: Vec<String> = split_rule_names(&cli.allowed_tools)
        .into_iter()
        .filter(|r| !r.contains('('))
        .collect();
    if !allowed_bare.is_empty() {
        let kept: Vec<String> = registry
            .names()
            .into_iter()
            .filter(|n| allowed_bare.iter().any(|a| a.eq_ignore_ascii_case(n)))
            .collect();
        registry = registry.filtered(&kept);
    }

    // ── whole-tool removal: a tool-level deny (`Bash`, `Bash(*)`, `mcp__srv`, `mcp__*`) from
    // settings or --disallowedTools hides the tool from the model (SPEC C2.2); scoped denies
    // keep the tool and block matching calls. ─────────────────────────────
    let mut deny_rules = cfg.permissions.deny.clone();
    deny_rules.extend(split_rule_names(&cli.disallowed_tools));
    let tool_denied =
        |name: &str| deny_rules.iter().any(|r| zen_tools::rule_removes_tool(r, name));
    let kept: Vec<String> = registry.names().into_iter().filter(|n| !tool_denied(n)).collect();
    registry = registry.filtered(&kept);

    // ── sub-agent definitions (Task tool) ────────────────────────────────
    // `--agent <name>` promotes one definition to be the main agent; its body replaces the
    // built-in system prompt and its `tools` list narrows the registry (B1).
    let mut agent_system_prompt: Option<String> = None;
    // --bare skips agent auto-discovery but still honours an explicit --agents flag.
    let agents = if cli.tools.as_deref() == Some("") || (cli.bare && cli.agents.is_none()) {
        Vec::new()
    } else if cli.bare {
        zen_mcp::load_cli_agents(cli.agents.as_deref())
    } else {
        zen_mcp::load_agents(cwd, home, cli.agents.as_deref())
    };
    if let Some(name) = &cli.agent {
        if let Some(def) = agents.iter().find(|d| d.name == *name) {
            agent_system_prompt = Some(def.system_prompt.clone());
            if let Some(tools) = &def.tools {
                let kept: Vec<String> = registry
                    .names()
                    .into_iter()
                    .filter(|n| tools.iter().any(|t| t.eq_ignore_ascii_case(n)))
                    .collect();
                registry = registry.filtered(&kept);
            }
        }
    }

    // ── Task tool (needs a sub-agent runner built first) ──────────────────
    let mut assembly_permissions: Arc<dyn PermissionChecker> = build_permissions(cwd, cfg, cli);
    let hooks = build_hooks(cli, cfg, session_id, transcript_path, cwd);
    let _ = &assembly_permissions;

    let subagent_sink = Arc::new(ForwardSink::default());
    if !agents.is_empty() && cli.tools.as_deref() != Some("") && !tool_denied("Task") {
        // Forwarding sink: the run's real sink is attached once it exists (C6.4).
        let fwd: Arc<dyn EventSink> = subagent_sink.clone();
        let executor = zen_agent::SubAgentExecutor::new(
            crate::eventlog::wrap(Arc::new(cfg.llm_client(fwd.clone())), recorder.as_ref()),
            registry.clone(),
            assembly_permissions.clone(),
            hooks.clone(),
            fwd,
            ToolContext::new(cwd.to_path_buf(), session_id.to_string()),
            base_agent_config(cfg, cli, ""),
        );
        registry.register(Arc::new(zen_mcp::TaskTool::new(
            agents,
            Arc::new(executor),
        )));
        assembly_permissions = build_permissions(cwd, cfg, cli);
    }

    let slash_commands: Vec<String> = commands::BUILTIN_COMMANDS
        .iter()
        .map(|s| s.to_string())
        .collect();

    Ok(AgentAssembly {
        registry,
        permissions: assembly_permissions,
        hooks,
        mcp_status,
        mcp_errors,
        slash_commands,
        skills,
        agent_system_prompt,
        subagent_sink,
        recorder,
    })
}

fn split_rule_names(rules: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for raw in rules {
        for part in raw.split(',') {
            let t = part.trim();
            if !t.is_empty() && !out.contains(&t.to_string()) {
                out.push(t.to_string());
            }
        }
    }
    out
}

fn build_permissions(cwd: &Path, cfg: &ResolvedConfig, cli: &Cli) -> Arc<dyn PermissionChecker> {
    build_permissions_with(cwd, cfg, cli, &[])
}

/// Permissions with extra allow rules (a custom command's `allowed-tools`, SPEC C5.2).
pub(crate) fn build_permissions_with(
    cwd: &Path,
    cfg: &ResolvedConfig,
    cli: &Cli,
    extra_allow: &[String],
) -> Arc<dyn PermissionChecker> {
    let mode = core_mode(cfg.permission_mode);
    let local_path = if cli.bare {
        None
    } else {
        Some(cwd.join(".zen").join("settings.local.json"))
    };
    let mut cli_allow: Vec<String> = split_rule_names(&cli.allowed_tools);
    cli_allow.extend(split_rule_names(extra_allow));
    let cli_deny: Vec<String> = split_rule_names(&cli.disallowed_tools);
    Arc::new(zen_tools::RulePermissions::new(
        mode,
        &cfg.permissions,
        cli_allow,
        cli_deny,
        local_path,
    ))
}

fn build_hooks(
    cli: &Cli,
    cfg: &ResolvedConfig,
    session_id: &str,
    transcript_path: Option<&Path>,
    cwd: &Path,
) -> Arc<dyn HookRunner> {
    if cli.bare {
        return Arc::new(NoHooks);
    }
    let has_hooks = cfg
        .hooks
        .as_object()
        .map(|o| !o.is_empty())
        .unwrap_or(false);
    if !has_hooks {
        return Arc::new(NoHooks);
    }
    Arc::new(
        zen_ext::CommandHooks::new(
            cfg.hooks.clone(),
            session_id,
            transcript_path
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            cwd.to_path_buf(),
            core_mode(cfg.permission_mode),
        )
        // Settings `env` is exported to hook processes (C3.14).
        .with_env(cfg.env.clone()),
    )
}

fn base_agent_config(cfg: &ResolvedConfig, cli: &Cli, system_prompt: &str) -> zen_agent::AgentConfig {
    let context_window = cfg.context_window.unwrap_or(128_000);
    // `--autocompact` beats the `autoCompact` setting (C10.1).
    let flag = cli.autocompact.as_deref().map(|v| match v {
        "auto" => serde_json::Value::Bool(true),
        "off" => serde_json::Value::Bool(false),
        n => serde_json::Value::from(n.parse::<u64>().unwrap_or(0)),
    });
    let auto_compact_threshold = match flag.as_ref().or(cfg.auto_compact.as_ref()) {
        Some(serde_json::Value::Bool(false)) => None,
        Some(serde_json::Value::Bool(true)) | None => Some(context_window * 80 / 100),
        Some(serde_json::Value::Number(n)) => n.as_u64(),
        _ => Some(context_window * 80 / 100),
    };
    let mut config = zen_agent::AgentConfig {
        model: cfg.model.clone(),
        fallback_models: cfg.fallback_models.clone(),
        system_prompt: system_prompt.to_string(),
        max_turns: cfg.max_turns.max(1),
        permission_mode: core_mode(cfg.permission_mode),
        context_window,
        auto_compact_threshold,
        keep_after_compact: 4,
        depth: 0,
        tool_timeouts: cfg.tool_timeouts.clone(),
        ..Default::default()
    };
    apply_verify_settings(&mut config, cfg, false);
    config
}

/// Settings `verify` → the agent's check-before-finish gate (r7).
///
/// `{"verify": {"enabled": true|false, "command": "cargo test -q", "timeoutSecs": 300,
///   "afterEdit": true | "cargo check -q", "afterEditTimeoutSecs": 60}}`.
/// The one-time reminder is on by default in the REPL; in headless `-p` runs it is on only when
/// a `verify` object is configured, so scripted runs stay exactly as they were. `enabled: false`
/// turns both the reminder and the command off.
pub fn apply_verify_settings(config: &mut zen_agent::AgentConfig, cfg: &ResolvedConfig, interactive: bool) {
    let v = cfg.verify.as_ref().filter(|v| v.is_object());
    let enabled = v.and_then(|v| v.get("enabled")).and_then(|e| e.as_bool());
    if enabled == Some(false) {
        config.verify_reminder = false;
        config.verify_command = None;
        config.verify_after_edit = None;
        return;
    }
    // `afterEdit`: `true` reuses `command`, a string is its own command; off when absent (p0).
    config.verify_after_edit = match v.and_then(|v| v.get("afterEdit")) {
        Some(serde_json::Value::Bool(true)) => v.and_then(|v| v.get("command")).and_then(|c| c.as_str()),
        Some(serde_json::Value::String(c)) => Some(c.as_str()),
        _ => None,
    }
    .map(str::trim)
    .filter(|c| !c.is_empty())
    .map(String::from);
    if let Some(t) = v.and_then(|v| v.get("afterEditTimeoutSecs")).and_then(|t| t.as_u64()) {
        config.verify_after_edit_timeout_secs = t.max(1);
    }
    config.verify_reminder = interactive || v.is_some();
    config.verify_command = v
        .and_then(|v| v.get("command"))
        .and_then(|c| c.as_str())
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(String::from);
    if let Some(t) = v.and_then(|v| v.get("timeoutSecs")).and_then(|t| t.as_u64()) {
        config.verify_timeout_secs = t.max(1);
    }
}

/// Public wrapper for building a ToolContext (used by repl.rs).
pub fn tool_context_public(
    cwd: &Path,
    cfg: &ResolvedConfig,
    cli: &Cli,
    session_id: &str,
) -> ToolContext {
    tool_context(cwd, cfg, cli, session_id)
}

fn tool_context(cwd: &Path, cfg: &ResolvedConfig, cli: &Cli, session_id: &str) -> ToolContext {
    let mut ctx = ToolContext::new(cwd.to_path_buf(), session_id.to_string());
    ctx.env = cfg.env.clone();
    let mut extra: Vec<std::path::PathBuf> = cli.add_dir.clone();
    for d in &cfg.permissions.additional_directories {
        extra.push(std::path::PathBuf::from(d));
    }
    ctx.extra_dirs = extra;
    ctx
}

/// Run one headless turn and print per the chosen output format. Returns the process exit code.
#[allow(clippy::too_many_arguments)]
pub async fn run_agent_headless(
    cli: &Cli,
    cwd: &Path,
    home: Option<&Path>,
    cfg: &ResolvedConfig,
    system_prompt: &str,
    prompt: &str,
    session_id: &str,
    transcript_path: Option<&Path>,
    history_from: Option<&Path>,
) -> Result<i32> {
    let resume = history_from.is_some();
    // Install SIGTERM/SIGINT handlers before anything slow (MCP start-up) so a signal that
    // arrives early is queued and handled when the turn starts (SPEC C8.9).
    let mut signals = SignalWatch::install();
    // Built-in slash commands win over custom commands of the same name (SPEC C5.1/C5.4).
    let builtin = commands::parse_builtin(prompt);
    if let Some((name, _)) = &builtin {
        if !commands::HEADLESS_BUILTINS.contains(&name.as_str()) {
            let buf = Buffered::new();
            let text = format!("/{} isn't available in -p mode (interactive sessions only).", name);
            let result = local_result(session_id, &cfg.model, Default::default(), text, true, 0);
            print_result(&result, &cli.output_format, &buf, false);
            print!("{}", buf.text());
            if cli.output_format == OutputFormat::Text {
                println!();
            }
            return Ok(1);
        }
    }
    // Expand a leading `/custom-command` in -p mode (SPEC §6); `/review` and `/init` become
    // their prompt templates.
    let (prompt, slash_cmd) = match &builtin {
        Some((name, args)) if name == "review" => (commands::review_prompt(args), None),
        Some((name, _)) if name == "init" => (commands::init_prompt(), None),
        Some(_) => (prompt.to_string(), None),
        None => expand_slash_prompt_cmd(cli, cwd, home, prompt, cfg, session_id)?,
    };
    // A custom command's `model` frontmatter picks the model for this turn (SPEC C5.2).
    let effective_model = slash_cmd
        .as_ref()
        .and_then(|c| c.model.clone())
        .filter(|m| !m.is_empty() && m != "inherit")
        .unwrap_or_else(|| cfg.model.clone());

    let assembly = assemble(cli, cwd, home, cfg, session_id, transcript_path).await?;

    let buf = Arc::new(Buffered::new());
    // text/json modes print only the final result; stream-json needs every event line.
    let mut stream_sink: Option<Arc<StreamJsonSink>> = None;
    let sink: Arc<dyn EventSink> = match cli.output_format {
        OutputFormat::StreamJson => {
            let s = Arc::new(StreamJsonSink::with_partial(
                buf.clone(),
                session_id.to_string(),
                cwd.display().to_string(),
                effective_model.clone(),
                assembly.registry.names(),
                assembly.mcp_status.clone(),
                cfg.permission_mode.to_string(),
                assembly.slash_commands.clone(),
                cli.include_partial_messages,
            ));
            s.set_mcp_errors(assembly.mcp_errors.clone());
            s.init_now();
            stream_sink = Some(s.clone());
            s
        }
        _ => Arc::new(zen_core::NullSink),
    };
    assembly.subagent_sink.set(sink.clone());

    let llm = crate::eventlog::wrap(Arc::new(cfg.llm_client(sink.clone())), assembly.recorder.as_ref());

    let mut config = base_agent_config(cfg, cli, system_prompt);
    config.model = effective_model.clone();
    // `--agent <name>` replaces the main system prompt with the definition body (B1); the
    // environment block is still appended so the agent knows where it is running.
    config.system_prompt = match &assembly.agent_system_prompt {
        Some(body) => format!("{}\n\n{}", body, crate::system_prompt::environment_block(cwd)),
        None => system_prompt.to_string(),
    };

    let mut ctx = tool_context(cwd, cfg, cli, session_id);
    ctx.session_dir = transcript_path.map(crate::session::session_scratch_dir);

    // A custom command's `allowed-tools` are allowed without asking for this turn (C5.2).
    let permissions = match slash_cmd.as_ref().filter(|c| !c.allowed_tools.is_empty()) {
        Some(c) => build_permissions_with(cwd, cfg, cli, &c.allowed_tools),
        None => assembly.permissions.clone(),
    };

    let mut agent = zen_agent::Agent::new(
        llm,
        assembly.registry.clone(),
        permissions,
        None, // headless: anything that would prompt is denied
        assembly.hooks.clone(),
        sink.clone(),
        ctx,
        config,
    );
    agent.journal = assembly.recorder.clone().map(|r| r as Arc<dyn zen_core::ToolJournal>);

    // Resuming / forking: replay the source transcript so the model sees prior context
    // (SPEC C9.2/C9.3). For `--fork-session` the source differs from `transcript_path`; the
    // replayed history is then written into the new transcript as a first "turn" so the fork is
    // self-contained.
    let mut writer = transcript_path
        .map(|p| crate::session::TranscriptWriter::new(p.to_path_buf(), session_id, cwd));
    if let Some(src) = history_from {
        // A turn that died mid-tool comes back with the tool results it already had.
        let (committed, history) = crate::eventlog::resume_history(src)?;
        if !history.is_empty() {
            if let Some(w) = writer.as_mut() {
                if transcript_path != Some(src) {
                    w.persist_turn(&[], &history)?;
                } else if history != committed {
                    w.persist_turn(&committed, &history)?;
                }
            }
            agent.set_messages(history);
        }
        // Usage of the earlier runs counts towards this result (C9.3); a fork carries it over.
        agent.usage = crate::session::load_usage(src);
        if transcript_path != Some(src) {
            if let Some(p) = transcript_path {
                if agent.usage != zen_core::UsageStats::default() {
                    crate::session::append_usage(p, session_id, &agent.usage)?;
                }
            }
        }
    }
    let usage_before = agent.usage.clone();

    // `/compact [focus]` in -p (SPEC C5.4): compact the (resumed) history instead of running a
    // turn, persist the summary boundary, and report the outcome as the result.
    // Built-ins answered locally, without a model turn (SPEC C5.4).
    if let Some((name, args)) = &builtin {
        let local = match name.as_str() {
            "model" if args.is_empty() => Some(format!("Current model: {}", agent.config.model)),
            "model" => {
                agent.config.model = args.clone();
                Some(format!("Set model to {}", args))
            }
            "cost" => Some(commands::cost_report(&agent.usage)),
            "mcp" => Some(commands::mcp_report(&assembly.mcp_status)),
            "context" => Some(commands::context_report(
                &agent.config.system_prompt,
                &agent.tools.definitions(),
                agent.messages(),
                agent.config.context_window,
                agent.last_prompt_tokens,
            )),
            _ => None,
        };
        if let Some(text) = local {
            run_session_end(&assembly.hooks, session_id, cwd, cfg, "other").await;
            let result =
                local_result(session_id, &agent.config.model, agent.usage.clone(), text, false, 0);
            print_result(&result, &cli.output_format, &buf, false);
            print!("{}", buf.text());
            if cli.output_format == OutputFormat::Text {
                println!();
            }
            return Ok(0);
        }
    }

    if let Some(focus) = compact_focus(&prompt) {
        let started = std::time::Instant::now();
        let before = agent.messages().to_vec();
        let outcome = agent.compact(focus, "manual").await;
        if let Some(w) = writer.as_mut() {
            if let Err(e) = w.persist_turn(&before, agent.messages()) {
                eprintln!("warning: could not write transcript: {}", e);
            }
        }
        persist_usage(transcript_path, session_id, &usage_before, &agent.usage);
        run_session_end(&assembly.hooks, session_id, cwd, cfg, "other").await;
        let (is_error, text) = match outcome {
            Ok(()) if before.is_empty() => (false, "Nothing to compact.".to_string()),
            Ok(()) if agent.messages() == before.as_slice() => {
                (false, "Compaction skipped.".to_string())
            }
            Ok(()) => (false, "Conversation compacted.".to_string()),
            Err(e) => (true, format!("Compaction failed: {}", e)),
        };
        let result = local_result(
            session_id,
            &cfg.model,
            agent.usage.clone(),
            text,
            is_error,
            started.elapsed().as_millis() as u64,
        );
        print_result(&result, &cli.output_format, &buf, false);
        print!("{}", buf.text());
        if cli.output_format == OutputFormat::Text {
            println!();
        }
        return Ok(if is_error { 1 } else { 0 });
    }

    // SessionStart hook: exit-0 stdout becomes additional context (SPEC §13).
    let mut prompt = prompt;
    if let Some(ctx) = run_session_start(
        &assembly.hooks,
        session_id,
        cwd,
        cfg,
        if resume { "resume" } else { "startup" },
    )
    .await {
        prompt.push_str("\n\n");
        prompt.push_str(&ctx);
    }
    // Tell the stream-json sink which user messages we already persisted, so the replayed
    // history does not duplicate them as extra `user` lines.
    if let Some(s) = &stream_sink {
        s.set_prompt_input(prompt.clone());
    }

    let before = agent.messages().to_vec();
    let started = std::time::Instant::now();
    let outcome = {
        let turn = agent.run_turn(&prompt);
        tokio::pin!(turn);
        match signals.as_mut() {
            Some(sig) => tokio::select! {
                r = &mut turn => Ok(r),
                i = sig.recv() => {
                    // Stop the whole process tree while the turn (and its bash child) is still
                    // alive: dropping it first would SIGKILL only bash and orphan e.g. `sleep`.
                    kill_descendants();
                    Err(i)
                }
            },
            None => Ok(turn.await),
        }
    };
    let result: RunResult = match outcome {
        Ok(r) => r,
        Err(interrupt) => {
            // The interrupted turn is not persisted: it may end in a tool call without a
            // result, which would leave an orphaned tool message on resume (C10.3).
            run_session_end(&assembly.hooks, session_id, cwd, cfg, "other").await;
            if interrupt == Interrupt::Term {
                return Ok(143);
            }
            let result = RunResult {
                record_type: "result".into(),
                subtype: "error_during_execution".into(),
                is_error: true,
                duration_ms: started.elapsed().as_millis() as u64,
                duration_api_ms: 0,
                num_turns: 0,
                result: "Interrupted by user".into(),
                session_id: session_id.into(),
                total_cost_usd: 0.0,
                usage: agent.usage.clone(),
                model: cfg.model.clone(),
                permission_denials: vec![],
            };
            print_result(&result, &cli.output_format, &buf, false);
            print!("{}", buf.text());
            if cli.output_format == OutputFormat::Text {
                println!();
            }
            return Ok(130);
        }
    };

    // Persist the whole exchange of this turn (prompt, tool calls, tool results, answer).
    if let Some(w) = writer.as_mut() {
        if let Err(e) = w.persist_turn(&before, agent.messages()) {
            eprintln!("warning: could not write transcript: {}", e);
        }
    }
    persist_usage(transcript_path, session_id, &usage_before, &agent.usage);

    // SessionEnd hook.
    run_session_end(&assembly.hooks, session_id, cwd, cfg, "other").await;

    print_result(&result, &cli.output_format, &buf, false);
    // Text mode adds the result text; json/stream-json emit newline-terminated lines.
    print!("{}", buf.text());
    if cli.output_format == OutputFormat::Text && !result.result.ends_with('\n') {
        println!();
    }

    Ok(if result.is_error { 1 } else { 0 })
}

/// Record the usage this run added (if any) in the transcript (C9.3).
fn persist_usage(
    path: Option<&Path>,
    session_id: &str,
    before: &zen_core::UsageStats,
    after: &zen_core::UsageStats,
) {
    let delta = zen_core::UsageStats {
        input_tokens: after.input_tokens.saturating_sub(before.input_tokens),
        output_tokens: after.output_tokens.saturating_sub(before.output_tokens),
    };
    if let (Some(p), true) = (path, delta != zen_core::UsageStats::default()) {
        if let Err(e) = crate::session::append_usage(p, session_id, &delta) {
            eprintln!("warning: could not write transcript: {}", e);
        }
    }
}

/// A result for a built-in answered without a model turn.
fn local_result(
    session_id: &str,
    model: &str,
    usage: zen_core::UsageStats,
    text: String,
    is_error: bool,
    duration_ms: u64,
) -> RunResult {
    RunResult {
        record_type: "result".into(),
        subtype: if is_error { "error_during_execution" } else { "success" }.into(),
        is_error,
        duration_ms,
        duration_api_ms: 0,
        num_turns: 0,
        result: text,
        session_id: session_id.into(),
        total_cost_usd: 0.0,
        usage,
        model: model.into(),
        permission_denials: vec![],
    }
}

/// Which signal ended a headless turn (SPEC C8.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interrupt {
    /// SIGTERM: exit 143 after the SessionEnd hook, no result line.
    Term,
    /// SIGINT (Ctrl-C): the turn ends, an error result is printed, exit 130.
    Int,
}

/// SIGTERM + SIGINT listeners for `-p`. `None` when the handlers cannot be installed.
pub struct SignalWatch {
    #[cfg(unix)]
    term: tokio::signal::unix::Signal,
    #[cfg(unix)]
    int: tokio::signal::unix::Signal,
}

impl SignalWatch {
    pub fn install() -> Option<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            Some(Self {
                term: signal(SignalKind::terminate()).ok()?,
                int: signal(SignalKind::interrupt()).ok()?,
            })
        }
        #[cfg(not(unix))]
        {
            Some(Self {})
        }
    }

    pub async fn recv(&mut self) -> Interrupt {
        #[cfg(unix)]
        {
            tokio::select! {
                _ = self.term.recv() => Interrupt::Term,
                _ = self.int.recv() => Interrupt::Int,
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            Interrupt::Int
        }
    }
}

/// SIGTERM every descendant process of zen (Bash children and their own children, stdio MCP
/// servers). Dropping the turn only kills the direct `bash` child, which would orphan e.g. a
/// `sleep` it started. Linux-only (/proc); a no-op elsewhere.
pub fn kill_descendants() {
    kill_new_descendants(&[]);
}

/// Like [`kill_descendants`], but spares processes in `keep` (e.g. MCP servers that were already
/// running before a REPL turn started).
pub fn kill_new_descendants(keep: &[u32]) {
    let pids: Vec<u32> = descendants_of(std::process::id())
        .into_iter()
        .filter(|p| !keep.contains(p))
        .collect();
    if pids.is_empty() {
        return;
    }
    let mut cmd = std::process::Command::new("kill");
    cmd.arg("-TERM").args(pids.iter().map(|p| p.to_string()));
    if let Err(e) = cmd.stderr(std::process::Stdio::null()).status() {
        eprintln!("warning: could not stop child processes: {}", e);
    }
}

/// All transitive children of `root`, read from /proc/<pid>/stat (empty without /proc).
pub fn descendants_of(root: u32) -> Vec<u32> {
    let mut parent_of: Vec<(u32, u32)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for e in entries.flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(e.path().join("stat")) else { continue };
            // Format: "pid (comm) state ppid ..."; comm may contain spaces/parens.
            let Some(after) = stat.rfind(')').map(|i| &stat[i + 1..]) else { continue };
            if let Some(ppid) = after.split_whitespace().nth(1).and_then(|p| p.parse().ok()) {
                parent_of.push((pid, ppid));
            }
        }
    }
    let mut out = Vec::new();
    let mut frontier = vec![root];
    while let Some(p) = frontier.pop() {
        for &(pid, ppid) in &parent_of {
            if ppid == p && !out.contains(&pid) {
                out.push(pid);
                frontier.push(pid);
            }
        }
    }
    out
}

/// SessionStart hook; `source` (= matcher target) is startup | resume | clear | compact.
/// Returns the additional context its hooks produced, if any.
pub(crate) async fn run_session_start(
    hooks: &Arc<dyn HookRunner>,
    session_id: &str,
    cwd: &Path,
    cfg: &ResolvedConfig,
    source: &str,
) -> Option<String> {
    use zen_core::HookEvent;
    let payload = serde_json::json!({
        "session_id": session_id,
        "cwd": cwd.display().to_string(),
        "permission_mode": cfg.permission_mode.to_string(),
        "source": source,
    });
    // Exit-0 stdout of SessionStart hooks is additional context (SPEC §13); zen-agent applies
    // UserPromptSubmit feedback but not SessionStart, so we fold it into the first prompt here.
    hooks
        .run(HookEvent::SessionStart, source, payload)
        .await
        .feedback
        .filter(|s| !s.trim().is_empty())
}

/// SessionEnd hook; `reason` (= matcher target) is clear | prompt_input_exit | other.
pub(crate) async fn run_session_end(
    hooks: &Arc<dyn HookRunner>,
    session_id: &str,
    cwd: &Path,
    cfg: &ResolvedConfig,
    reason: &str,
) {
    use zen_core::HookEvent;
    let payload = serde_json::json!({
        "session_id": session_id,
        "cwd": cwd.display().to_string(),
        "permission_mode": cfg.permission_mode.to_string(),
        "reason": reason,
    });
    let _ = hooks.run(HookEvent::SessionEnd, reason, payload).await;
}

/// `Some(focus)` when the prompt is the built-in `/compact [focus]` command.
fn compact_focus(prompt: &str) -> Option<Option<&str>> {
    let rest = prompt.trim().strip_prefix("/compact")?;
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None; // e.g. `/compactify` is not the built-in
    }
    let focus = rest.trim();
    Some(if focus.is_empty() { None } else { Some(focus) })
}

/// If the prompt starts with `/name` and `name` is a custom command or user-invocable skill,
/// expand it (SPEC C5.4). `` !`cmd` `` only runs when the permission rules allow it as Bash.
pub fn expand_slash_prompt(
    cli: &Cli,
    cwd: &Path,
    home: Option<&Path>,
    prompt: &str,
    cfg: &ResolvedConfig,
    session_id: &str,
) -> Result<String> {
    Ok(expand_slash_prompt_cmd(cli, cwd, home, prompt, cfg, session_id)?.0)
}

/// Like [`expand_slash_prompt`], also returning the command that was expanded (its `model` and
/// `allowed-tools` frontmatter apply to the turn, SPEC C5.2).
pub fn expand_slash_prompt_cmd(
    cli: &Cli,
    cwd: &Path,
    home: Option<&Path>,
    prompt: &str,
    cfg: &ResolvedConfig,
    session_id: &str,
) -> Result<(String, Option<zen_ext::SlashCommand>)> {
    if cli.bare || !prompt.trim_start().starts_with('/') {
        return Ok((prompt.to_string(), None));
    }
    let perms = build_permissions(cwd, cfg, cli);
    let tctx = zen_core::ToolContext::new(cwd.to_path_buf(), session_id);
    let gate = move |cmd: &str| {
        matches!(
            perms.check("Bash", &serde_json::json!({ "command": cmd }), false, &tctx),
            zen_core::PermissionDecision::Allow
        )
    };
    let ectx = zen_ext::ExpandContext {
        project_dir: cwd.to_path_buf(),
        session_id: session_id.to_string(),
        allow_shell: Some(&gate),
    };
    Ok(match zen_ext::expand_slash_prompt(prompt, cwd, home, &ectx) {
        Some((cmd, text)) => (text, Some(cmd)),
        None => (prompt.to_string(), None),
    })
}

/// Emit a permission-denial result helper used by tests.
pub fn denied_result(session_id: &str, model: &str, denials: Vec<PermissionDenial>) -> RunResult {
    RunResult {
        record_type: "result".into(),
        subtype: "error_during_execution".into(),
        is_error: true,
        duration_ms: 0,
        duration_api_ms: 0,
        num_turns: 0,
        result: String::new(),
        session_id: session_id.into(),
        total_cost_usd: 0.0,
        usage: Default::default(),
        model: model.into(),
        permission_denials: denials,
    }
}

/// Emit an event without a sink (used by the REPL for simple notices).
pub fn emit_notice(_ev: AgentEvent) {}

#[cfg(test)]
mod tools_flag_tests {
    use super::*;

    fn names(v: &[Arc<dyn Tool>]) -> Vec<String> {
        v.iter().map(|t| t.name().to_string()).collect()
    }

    #[test]
    fn tools_flag_default_empty_and_allowlist() {
        let all = names(&zen_tools::builtin_tools());
        assert_eq!(names(&select_builtin_tools(None, zen_tools::builtin_tools()).unwrap()), all);
        assert_eq!(names(&select_builtin_tools(Some("default"), zen_tools::builtin_tools()).unwrap()), all);
        assert!(select_builtin_tools(Some(""), zen_tools::builtin_tools()).unwrap().is_empty());
        assert_eq!(names(&select_builtin_tools(Some("read, Bash"), zen_tools::builtin_tools()).unwrap()), vec!["Read", "Bash"]);
    }

    #[test]
    fn tools_flag_delta_form_removes_and_keeps_defaults() {
        let all = names(&zen_tools::builtin_tools());
        let got = names(&select_builtin_tools(Some("-Bash,-write"), zen_tools::builtin_tools()).unwrap());
        let want: Vec<String> = all.iter().filter(|n| *n != "Bash" && *n != "Write").cloned().collect();
        assert_eq!(got, want);
        // +name of a default tool is a no-op; a later entry wins over an earlier one.
        let got = names(&select_builtin_tools(Some("-Bash,+Bash"), zen_tools::builtin_tools()).unwrap());
        assert_eq!(got, all);
    }

    #[test]
    fn tools_flag_rejects_mixed_and_unknown_delta() {
        let mixed = select_builtin_tools(Some("Read,-Bash"), zen_tools::builtin_tools()).err().unwrap();
        assert!(mixed.to_string().contains("cannot mix"), "{mixed}");
        let unknown = select_builtin_tools(Some("-Bsh"), zen_tools::builtin_tools()).err().unwrap();
        assert!(unknown.to_string().contains("unknown built-in tool 'Bsh'"), "{unknown}");
    }
}
