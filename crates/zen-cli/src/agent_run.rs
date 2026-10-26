















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


pub struct AgentAssembly {
    pub registry: ToolRegistry,
    pub permissions: Arc<dyn PermissionChecker>,
    pub hooks: Arc<dyn HookRunner>,
    pub mcp_status: Vec<(String, String)>,

    pub mcp_errors: Vec<(String, String)>,
    pub slash_commands: Vec<String>,
    pub skills: Vec<zen_core::SkillInfo>,


    pub agent_system_prompt: Option<String>,



    pub subagent_sink: Arc<ForwardSink>,


    pub recorder: Option<Arc<crate::eventlog::Recorder>>,
}


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


pub fn core_mode(m: crate::cli::PermissionMode) -> PermissionMode {
    match m {
        crate::cli::PermissionMode::Default => PermissionMode::Default,
        crate::cli::PermissionMode::AcceptEdits => PermissionMode::AcceptEdits,
        crate::cli::PermissionMode::Plan => PermissionMode::Plan,
        crate::cli::PermissionMode::BypassPermissions => PermissionMode::BypassPermissions,
        crate::cli::PermissionMode::DontAsk => PermissionMode::DontAsk,
    }
}


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

                let rules = zen_tools::suggested_rules(tool_name, input);
                let rule = if rules.is_empty() { tool_name.to_string() } else { rules.join("\n") };
                zen_core::PromptAnswer::Always(rule)
            }
            _ => zen_core::PromptAnswer::No,
        }
    }
}











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

    let recorder = transcript_path.map(|p| crate::eventlog::Recorder::new(p.to_path_buf()));


    let mut registry = ToolRegistry::new();
    let all_builtins = zen_tools::builtin_tools();
    let selected = select_builtin_tools(cli.tools.as_deref(), all_builtins)?;
    for t in selected {
        registry.register(t);
    }

    if registry.get("Bash").is_some() {
        for t in zen_tools::job_tools() {
            registry.register(t);
        }
    }


    let mut mcp_status: Vec<(String, String)> = vec![];
    let mut mcp_errors: Vec<(String, String)> = vec![];
    let mut skills: Vec<zen_core::SkillInfo> = vec![];





    let skill_gate = Arc::new(crate::skills::SkillPermissionGate::new(
        build_permissions(cwd, cfg, cli),
    ));
    if !cli.bare {
        skills = zen_ext::discover_skills(cwd, home);
        if cli.tools.as_deref() != Some("") {


            let installed = home
                .map(crate::skills::load_enabled_skills)
                .unwrap_or_default();
            registry.register(Arc::new(crate::skills::GatedSkillTool::new(
                zen_ext::SkillTool::new(skills.clone()),
                skill_gate.clone(),
                installed,
            )));
        }
    }



    if !cli.bare || !cli.mcp_config.is_empty() {

        let mcp_configs = match zen_mcp::load_mcp_configs_scoped(
            cwd,
            home,
            &cli.mcp_config,
            cli.strict_mcp_config || cli.bare,
            cfg.folder_trusted,
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


    }




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




    let mut deny_rules = cfg.permissions.deny.clone();
    deny_rules.extend(split_rule_names(&cli.disallowed_tools));
    let tool_denied =
        |name: &str| deny_rules.iter().any(|r| zen_tools::rule_removes_tool(r, name));
    let kept: Vec<String> = registry.names().into_iter().filter(|n| !tool_denied(n)).collect();
    registry = registry.filtered(&kept);




    let mut agent_system_prompt: Option<String> = None;

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


    let mut assembly_permissions: Arc<dyn PermissionChecker> = build_permissions(cwd, cfg, cli);
    let hooks = build_hooks(cli, cfg, session_id, transcript_path, cwd);
    let _ = &assembly_permissions;

    let subagent_sink = Arc::new(ForwardSink::default());
    if !agents.is_empty() && cli.tools.as_deref() != Some("") && !tool_denied("Task") {

        let fwd: Arc<dyn EventSink> = subagent_sink.clone();
        let executor = zen_agent::SubAgentExecutor::new(
            crate::eventlog::wrap(cfg.gated_llm_client(fwd.clone(), home), recorder.as_ref()),
            registry.clone(),

            Arc::new(skill_gate.shared(assembly_permissions.clone())),
            hooks.clone(),
            fwd,
            ToolContext::new(cwd.to_path_buf(), session_id.to_string()),
            base_agent_config(cfg, cli, ""),
        );
        registry.register(Arc::new(zen_mcp::TaskTool::new(
            agents,
            Arc::new(executor),
        )));


        assembly_permissions = Arc::new(skill_gate.shared(build_permissions(cwd, cfg, cli)));
    }



    if cli.tools.as_deref() != Some("") {
        let lsp_manager = Arc::new(zen_mcp::LspManager::new());
        let lsp_tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(zen_mcp::GotoDefinitionTool::new(lsp_manager.clone())),
            Arc::new(zen_mcp::HoverTool::new(lsp_manager.clone())),
            Arc::new(zen_mcp::FindReferencesTool::new(lsp_manager.clone())),
        ];
        for tool in lsp_tools {


            let name = tool.name();
            let allowed = allowed_bare.is_empty()
                || allowed_bare.iter().any(|a| a.eq_ignore_ascii_case(name));
            if allowed && !tool_denied(name) && lsp_tool_allowed(cli.tools.as_deref(), name) {
                registry.register(tool);
            }
        }
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




fn lsp_tool_allowed(spec: Option<&str>, name: &str) -> bool {
    let list = match spec {
        None | Some("default") => return true,
        Some(list) => list,
    };
    let entries: Vec<&str> = list
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if entries.iter().any(|e| e.starts_with('+') || e.starts_with('-')) {
        return !entries
            .iter()
            .filter_map(|e| e.strip_prefix('-'))
            .any(|n| n.eq_ignore_ascii_case(name));
    }
    entries.iter().any(|e| e.eq_ignore_ascii_case(name))
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

        .with_env(cfg.env.clone())

        .with_api_key_env(cfg.api_key_env.clone()),
    )
}



pub fn resolve_tool_bundle(cli: &Cli, cfg: &ResolvedConfig) -> zen_agent::ToolBundle {
    if let Some(b) = cli.tool_bundle {
        return b;
    }
    if let Ok(s) = std::env::var("ZEN_TOOL_BUNDLE") {
        if let Ok(b) = s.parse() {
            return b;
        }
    }
    cfg.tool_bundle
}






fn resolve_auto_compact_threshold(
    flag: Option<&str>,
    setting: Option<&serde_json::Value>,
    context_window: u64,
) -> Option<u64> {

    let flag_value = flag.map(|v| match v {
        "auto" => serde_json::Value::Bool(true),
        "off" => serde_json::Value::Bool(false),
        n => serde_json::Value::from(n.parse::<u64>().unwrap_or(0)),
    });
    match flag_value.as_ref().or(setting) {
        Some(serde_json::Value::Bool(false)) => None,
        Some(serde_json::Value::Bool(true)) | None => Some(context_window * 80 / 100),
        Some(serde_json::Value::Number(n)) => n.as_u64(),
        _ => Some(context_window * 80 / 100),
    }
}

pub fn base_agent_config(cfg: &ResolvedConfig, cli: &Cli, system_prompt: &str) -> zen_agent::AgentConfig {
    let context_window = cfg.context_window.unwrap_or(128_000);
    let auto_compact_threshold =
        resolve_auto_compact_threshold(cli.autocompact.as_deref(), cfg.auto_compact.as_ref(), context_window);
    let mut config = zen_agent::AgentConfig {
        model: cfg.model.clone(),
        fallback_models: cfg.fallback_models.clone(),

        system_prompt: zen_agent::plan::system_prompt_for_mode(
            system_prompt,
            core_mode(cfg.permission_mode),
        ),
        max_turns: cfg.max_turns.max(1),
        max_output_tokens: cli.max_output_tokens,
        permission_mode: core_mode(cfg.permission_mode),
        context_window,
        auto_compact_threshold,
        keep_after_compact: 4,
        depth: 0,
        tool_timeouts: cfg.tool_timeouts.clone(),
        max_continuations: cfg.max_continuations,
        prune: zen_agent::prune::PruneConfig {


            enabled: !cli.no_prune,
            prune_after: cli
                .prune_after
                .unwrap_or(zen_agent::prune::DEFAULT_PRUNE_AFTER),
            max_result_tokens: cli
                .max_result_tokens
                .map(|n| n.max(1))
                .unwrap_or(zen_agent::prune::DEFAULT_MAX_RESULT_TOKENS),
            ..zen_agent::prune::PruneConfig::default()
        },
        tool_bundle: resolve_tool_bundle(cli, cfg),
        max_parallel_tool_calls: cfg.max_parallel_tool_calls,
        ..Default::default()
    };
    apply_verify_settings(&mut config, cfg, false);
    config.budget = crate::budget::resolve_budget(
        cli.max_budget_usd,
        cli.budget_warn_at,
        cfg.max_budget_usd,
        cfg.budget_warn_at,
    );
    config
}








pub fn apply_verify_settings(config: &mut zen_agent::AgentConfig, cfg: &ResolvedConfig, interactive: bool) {
    let v = cfg.verify.as_ref().filter(|v| v.is_object());
    let enabled = v.and_then(|v| v.get("enabled")).and_then(|e| e.as_bool());
    if enabled == Some(false) {
        config.verify_reminder = false;
        config.verify_command = None;
        config.verify_after_edit = None;
        return;
    }

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
    ctx.api_key_env = cfg.api_key_env.clone();
    ctx.allow_private_network = cfg.allow_private_network;
    let mut extra: Vec<std::path::PathBuf> = cli.add_dir.clone();
    for d in &cfg.permissions.additional_directories {
        extra.push(std::path::PathBuf::from(d));
    }
    ctx.extra_dirs = extra;
    ctx
}


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
    let (code, _) = run_agent_with_result(
        cli,
        cwd,
        home,
        cfg,
        system_prompt,
        prompt,
        session_id,
        transcript_path,
        history_from,
    )
    .await?;
    Ok(code)
}



#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_agent_with_result(
    cli: &Cli,
    cwd: &Path,
    home: Option<&Path>,
    cfg: &ResolvedConfig,
    system_prompt: &str,
    prompt: &str,
    session_id: &str,
    transcript_path: Option<&Path>,
    history_from: Option<&Path>,
) -> Result<(i32, RunResult)> {
    let resume = history_from.is_some();


    let mut signals = SignalWatch::install();

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
            return Ok((1, result));
        }
    }


    let (prompt, slash_cmd) = match &builtin {
        Some((name, args)) if name == "review" => (commands::review_prompt(args), None),
        Some((name, _)) if name == "init" => {


            let candidates = zen_ext::detect_import_candidates(cwd);
            let prompt = if candidates.is_empty() {
                commands::init_prompt()
            } else {
                commands::init_prompt_with_imports(&candidates)
            };
            (prompt, None)
        }
        Some(_) => (prompt.to_string(), None),
        None => expand_slash_prompt_cmd(cli, cwd, home, prompt, cfg, session_id)?,
    };

    let effective_model = slash_cmd
        .as_ref()
        .and_then(|c| c.model.clone())
        .filter(|m| !m.is_empty() && m != "inherit")
        .unwrap_or_else(|| cfg.model.clone());

    let assembly = assemble(cli, cwd, home, cfg, session_id, transcript_path).await?;



    let buf = Arc::new(if cli.output_format == OutputFormat::StreamJson {
        Buffered::stdout()
    } else {
        Buffered::new()
    });
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
            s.set_folder_trusted(cfg.folder_trusted);
            s.init_now();
            stream_sink = Some(s.clone());
            s
        }
        _ => Arc::new(zen_core::NullSink),
    };
    assembly.subagent_sink.set(sink.clone());

    let llm = crate::eventlog::wrap(cfg.gated_llm_client(sink.clone(), home), assembly.recorder.as_ref());

    let mut config = base_agent_config(cfg, cli, system_prompt);
    config.model = effective_model.clone();



    let base_prompt = match &assembly.agent_system_prompt {
        Some(body) => format!("{}\n\n{}", body, crate::system_prompt::environment_block(cwd)),
        None => system_prompt.to_string(),
    };
    config.system_prompt =
        zen_agent::plan::system_prompt_for_mode(&base_prompt, config.permission_mode);

    let mut ctx = tool_context(cwd, cfg, cli, session_id);
    ctx.session_dir = transcript_path.map(crate::session::session_scratch_dir);


    let permissions = match slash_cmd.as_ref().filter(|c| !c.allowed_tools.is_empty()) {
        Some(c) => build_permissions_with(cwd, cfg, cli, &c.allowed_tools),
        None => assembly.permissions.clone(),
    };

    let mut agent = zen_agent::Agent::new(
        llm,
        assembly.registry.clone(),
        permissions,
        None,
        assembly.hooks.clone(),
        sink.clone(),
        ctx,
        config,
    );
    agent.journal = assembly.recorder.clone().map(|r| r as Arc<dyn zen_core::ToolJournal>);





    let mut writer = transcript_path
        .map(|p| crate::session::TranscriptWriter::new(p.to_path_buf(), session_id, cwd));
    if let Some(src) = history_from {

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




    let mut local_failed = false;
    if let Some((name, args)) = &builtin {
        let local = match name.as_str() {
            "model" if args.is_empty() => Some(format!("Current model: {}", agent.config.model)),
            "model" => {
                agent.config.model = args.clone();
                Some(format!("Set model to {}", args))
            }
            "cost" => Some(commands::cost_report(&agent.usage, &agent.config.model, &cfg.base_url)),
            "mcp" => Some(commands::mcp_report(&assembly.mcp_status)),
            "context" => Some(commands::context_report(
                &agent.config.system_prompt,
                &agent.tools_for_request(),
                agent.messages(),
                agent.config.context_window,
                agent.last_prompt_tokens,
            )),
            "done" => {
                let checks = crate::doneguard::run_done_checks(cwd, &[]).await;
                let changed = crate::doneguard::changed_files(cwd);
                let yagni = crate::doneguard::yagni_scan(cwd);
                if checks
                    .iter()
                    .any(|c| c.status == crate::doneguard::CheckStatus::Fail)
                {
                    local_failed = true;
                }
                Some(crate::doneguard::format_report(
                    cwd,
                    &crate::doneguard::DoneReport {
                        checks,
                        changed,
                        scope: vec![],
                        scope_violations: vec![],
                        yagni,
                    },
                ))
            }
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
            return Ok((if local_failed { 1 } else { 0 }, result));
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
        persist_usage(
            transcript_path,
            session_id,
            &usage_before,
            &agent.usage,
            home,
            &agent.config.model,
            &cfg.base_url,
        );
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
        return Ok((if is_error { 1 } else { 0 }, result));
    }


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


    if let Some(s) = &stream_sink {
        s.set_prompt_input(prompt.clone());
    }

    let verify = verify_config(cli);


    let mut usage_before = usage_before;
    let mut verify_attempt: usize = 0;

    let result: RunResult = loop {
        let before = agent.messages().to_vec();
        let started = std::time::Instant::now();
        let outcome = {
            let turn = agent.run_turn(&prompt);
            tokio::pin!(turn);
            match signals.as_mut() {
                Some(sig) => tokio::select! {
                    r = &mut turn => Ok(r),
                    i = sig.recv() => {


                        kill_descendants();
                        Err(i)
                    }
                },
                None => Ok(turn.await),
            }
        };
        let turn_result: RunResult = match outcome {
            Ok(r) => r,
            Err(interrupt) => {


                run_session_end(&assembly.hooks, session_id, cwd, cfg, "other").await;


                let total_cost_usd = zen_agent::budget::estimate_cost_usd(
                    &cfg.model,
                    agent.usage.input_tokens,
                    agent.usage.output_tokens,
                )
                .unwrap_or(0.0);
                let result = RunResult {
                    record_type: "result".into(),
                    subtype: "error_during_execution".into(),
                    is_error: true,
                    duration_ms: started.elapsed().as_millis() as u64,
                    duration_api_ms: 0,
                    num_turns: 0,
                    result: "Interrupted by user".into(),
                    session_id: session_id.into(),
                    total_cost_usd,
                    usage: agent.usage.clone(),
                    model: cfg.model.clone(),
                    permission_denials: vec![],
                };
                if interrupt != Interrupt::Term {

                    print_result(&result, &cli.output_format, &buf, false);
                    print!("{}", buf.text());
                    if cli.output_format == OutputFormat::Text {
                        println!();
                    }
                }
                return Ok((
                    if interrupt == Interrupt::Term { 143 } else { 130 },
                    result,
                ));
            }
        };


        if let Some(w) = writer.as_mut() {
            if let Err(e) = w.persist_turn(&before, agent.messages()) {
                eprintln!("warning: could not write transcript: {}", e);
            }
        }
        persist_usage(
            transcript_path,
            session_id,
            &usage_before,
            &agent.usage,
            home,
            &agent.config.model,
            &cfg.base_url,
        );
        usage_before = agent.usage.clone();




        let Some(v) = verify.as_ref() else {
            break turn_result;
        };
        if turn_result.is_error {
            break turn_result;
        }
        let vo = run_verify_command(&v.command, cwd, &agent.ctx.env).await;
        let attempt_no = verify_attempt + 1;
        let total = v.max_retries + 1;
        let status = if vo.success() {
            "passed".to_string()
        } else if vo.timed_out {
            "timed out".to_string()
        } else {
            format!("failed with exit {}", vo.exit_code)
        };
        sink.emit(AgentEvent::SystemMessage {
            event: "verify".into(),
            text: format!("verify: `{}` {status} (attempt {attempt_no}/{total})", v.command),
        });
        match decide_verify(&vo, &v.command, attempt_no, v.max_retries) {
            VerifyDecision::Done => break turn_result,
            VerifyDecision::Retry(feedback) => {
                verify_attempt = attempt_no;
                prompt = feedback;
            }
            VerifyDecision::Failed => {
                break verify_failed_result(&turn_result, &vo, &v.command, v.max_retries, session_id)
            }
        }
    };


    run_session_end(&assembly.hooks, session_id, cwd, cfg, "other").await;

    print_result(&result, &cli.output_format, &buf, false);

    print!("{}", buf.text());
    if cli.output_format == OutputFormat::Text && !result.result.ends_with('\n') {
        println!();
    }


    if let Some(schema_path) = &cli.json_schema {
        if cli.output_format != OutputFormat::Json {
            eprintln!("warning: --json-schema requires --output-format json");
        } else if let Err(msg) = crate::output::validate_json_schema(&result.result, schema_path) {
            eprintln!("JSON schema validation failed: {}", msg);
            return Ok((2, result));
        }
    }

    Ok((if result.is_error { 1 } else { 0 }, result))
}





#[allow(clippy::too_many_arguments)]
pub async fn run_plan_flow(
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


    let mut plan_cli = cli.clone();
    plan_cli.plan = false;
    plan_cli.until = None;
    plan_cli.permission_mode = Some(crate::cli::PermissionMode::Plan);
    let plan_cfg = crate::settings::resolve_config_with_home(&plan_cli, cwd, home)?;
    let (plan_code, plan_result) = run_agent_with_result(
        &plan_cli,
        cwd,
        home,
        &plan_cfg,
        system_prompt,
        prompt,
        session_id,
        transcript_path,
        history_from,
    )
    .await?;
    if plan_result.is_error {
        return Ok(plan_code);
    }

    let approved = if cli.yes {
        eprintln!("--yes: plan auto-approved.");
        true
    } else {
        ask_plan_approval()
    };
    if !approved {
        eprintln!("Plan rejected; nothing was executed.");
        return Ok(1);
    }

    let exec_prompt = format!("{prompt}\n\n## Approved plan\n\n{}", plan_result.result);
    run_agent_headless(
        cli,
        cwd,
        home,
        cfg,
        system_prompt,
        &exec_prompt,
        session_id,
        transcript_path,
        history_from,
    )
    .await
}


fn ask_plan_approval() -> bool {
    use std::io::{IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        eprintln!("--plan needs an interactive terminal to approve the plan (pass --yes to skip).");
        return false;
    }
    eprint!("\nExecute this plan? [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}



fn persist_usage(
    path: Option<&Path>,
    session_id: &str,
    before: &zen_core::UsageStats,
    after: &zen_core::UsageStats,
    home: Option<&Path>,
    model: &str,
    base_url: &str,
) {
    let delta = zen_core::UsageStats {
        input_tokens: after.input_tokens.saturating_sub(before.input_tokens),
        output_tokens: after.output_tokens.saturating_sub(before.output_tokens),
        cache_read_input_tokens: after
            .cache_read_input_tokens
            .saturating_sub(before.cache_read_input_tokens),
    };
    if delta == zen_core::UsageStats::default() {
        return;
    }
    if let Some(p) = path {
        if let Err(e) = crate::session::append_usage(p, session_id, &delta) {
            eprintln!("warning: could not write transcript: {}", e);
        }
    }
    crate::cost::record(
        home,
        session_id,
        model,
        base_url,
        delta.input_tokens,
        delta.output_tokens,
    );
}


fn local_result(
    session_id: &str,
    model: &str,
    usage: zen_core::UsageStats,
    text: String,
    is_error: bool,
    duration_ms: u64,
) -> RunResult {



    let total_cost_usd = zen_agent::budget::estimate_cost_usd(
        model,
        usage.input_tokens,
        usage.output_tokens,
    )
    .unwrap_or(0.0);
    RunResult {
        record_type: "result".into(),
        subtype: if is_error { "error_during_execution" } else { "success" }.into(),
        is_error,
        duration_ms,
        duration_api_ms: 0,
        num_turns: 0,
        result: text,
        session_id: session_id.into(),
        total_cost_usd,
        usage,
        model: model.into(),
        permission_denials: vec![],
    }
}


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interrupt {

    Term,

    Int,
}


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




pub fn kill_descendants() {
    kill_new_descendants(&[]);
}



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


pub fn descendants_of(root: u32) -> Vec<u32> {
    let mut parent_of: Vec<(u32, u32)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for e in entries.flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(e.path().join("stat")) else { continue };

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


    hooks
        .run(HookEvent::SessionStart, source, payload)
        .await
        .feedback
        .filter(|s| !s.trim().is_empty())
}


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


fn compact_focus(prompt: &str) -> Option<Option<&str>> {
    let rest = prompt.trim().strip_prefix("/compact")?;
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let focus = rest.trim();
    Some(if focus.is_empty() { None } else { Some(focus) })
}



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
        Some((mut cmd, text)) => {

            if !cfg.folder_trusted && cmd.path.starts_with(cwd.join(".zen")) {
                cmd.allowed_tools.clear();
            }
            (text, Some(cmd))
        }
        None => (prompt.to_string(), None),
    })
}


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


pub fn emit_notice(_ev: AgentEvent) {}


const VERIFY_TIMEOUT_SECS: u64 = 300;

const VERIFY_OUTPUT_CAP: usize = 6000;


#[derive(Debug, Clone)]
pub struct VerifyConfig {
    pub command: String,
    pub max_retries: usize,
}


pub fn verify_config(cli: &Cli) -> Option<VerifyConfig> {
    cli.until
        .as_ref()
        .map(|c| c.trim())
        .filter(|c| !c.is_empty())
        .map(|c| VerifyConfig {
            command: c.to_string(),
            max_retries: cli.max_verify_retries,
        })
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyOutcome {

    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl VerifyOutcome {
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }


    pub fn combined_output(&self) -> String {
        let mut out = String::new();
        if !self.stdout.trim().is_empty() {
            out.push_str(self.stdout.trim_end());
        }
        if !self.stderr.trim().is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(self.stderr.trim_end());
        }
        truncate_head_tail(&out, VERIFY_OUTPUT_CAP)
    }
}


fn truncate_head_tail(s: &str, max_chars: usize) -> String {
    let len = s.chars().count();
    if len <= max_chars {
        return s.to_string();
    }
    let head = max_chars * 2 / 3;
    let tail = max_chars - head;
    let h: String = s.chars().take(head).collect();
    let t: String = s.chars().rev().take(tail).collect::<String>().chars().rev().collect();
    format!("{h}\n…[truncated {} chars]…\n{t}", len - max_chars)
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyDecision {

    Done,

    Retry(String),

    Failed,
}


pub fn decide_verify(
    outcome: &VerifyOutcome,
    command: &str,
    attempt: usize,
    max_retries: usize,
) -> VerifyDecision {
    if outcome.success() {
        return VerifyDecision::Done;
    }
    if attempt > max_retries {
        return VerifyDecision::Failed;
    }
    VerifyDecision::Retry(verify_feedback_prompt(command, outcome, attempt, max_retries))
}


pub fn verify_feedback_prompt(
    command: &str,
    outcome: &VerifyOutcome,
    attempt: usize,
    max_retries: usize,
) -> String {
    let reason = if outcome.timed_out {
        "timed out".to_string()
    } else {
        format!("exited with code {}", outcome.exit_code)
    };
    format!(
        "The verification command `{command}` {reason} (verify attempt {attempt} of {}).\n\n\
         Command output:\n```\n{output}\n```\n\n\
         Fix whatever failed and complete the original task. Do not change unrelated files.",
        max_retries + 1,
        output = outcome.combined_output(),
    )
}



pub async fn run_verify_command(
    command: &str,
    cwd: &Path,
    env: &std::collections::HashMap<String, String>,
) -> VerifyOutcome {
    let shell = zen_core::pick_shell(
        "sh",
        cfg!(windows),
        std::env::var("ZEN_SHELL").ok().as_deref(),
        &zen_core::program_on_path,
    );
    let mut cmd = tokio::process::Command::new(&shell.program);
    cmd.args(shell.args(command))
        .current_dir(cwd)
        .envs(env.iter())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let timeout_secs: u64 = std::env::var("VERIFY_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(VERIFY_TIMEOUT_SECS)
        .max(1);
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return VerifyOutcome {
                exit_code: -1,
                stdout: String::new(),
                stderr: format!("failed to spawn verify command: {e}"),
                timed_out: false,
            }
        }
    };
    match tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        child.wait_with_output(),
    )
    .await
    {
        Ok(Ok(out)) => VerifyOutcome {
            exit_code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            timed_out: false,
        },
        _ => VerifyOutcome {
            exit_code: -1,
            stdout: String::new(),
            stderr: format!("verify command timed out after {timeout_secs}s"),
            timed_out: true,
        },
    }
}


fn verify_failed_result(
    base: &RunResult,
    outcome: &VerifyOutcome,
    command: &str,
    max_retries: usize,
    session_id: &str,
) -> RunResult {
    let mut r = base.clone();
    r.is_error = true;
    r.subtype = "error_during_execution".into();
    r.result = format!(
        "{}\n\nVerify failed: command `{command}` {} after {} attempt(s) \
         (1 initial run + {max_retries} retries).\nOutput:\n```\n{output}\n```",
        base.result,
        if outcome.timed_out {
            "timed out".to_string()
        } else {
            format!("exited with code {}", outcome.exit_code)
        },
        max_retries + 1,
        output = outcome.combined_output(),
    );
    r.session_id = session_id.to_string();
    r
}

#[cfg(test)]
mod verify_tests {
    use super::*;
    use clap::Parser;

    fn fail(exit_code: i32) -> VerifyOutcome {
        VerifyOutcome {
            exit_code,
            stdout: "some stdout".into(),
            stderr: "some stderr".into(),
            timed_out: false,
        }
    }

    fn pass() -> VerifyOutcome {
        VerifyOutcome {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
        }
    }

    #[test]
    fn decide_verify_success_is_done() {
        assert_eq!(decide_verify(&pass(), "true", 1, 3), VerifyDecision::Done);

        assert_eq!(decide_verify(&pass(), "true", 4, 3), VerifyDecision::Done);
    }

    #[test]
    fn decide_verify_retries_until_max_then_fails() {
        let f = fail(2);
        match decide_verify(&f, "make check", 1, 3) {
            VerifyDecision::Retry(prompt) => {
                assert!(prompt.contains("make check"), "{prompt}");
                assert!(prompt.contains("exited with code 2"), "{prompt}");
                assert!(prompt.contains("some stdout"), "{prompt}");
                assert!(prompt.contains("attempt 1 of 4"), "{prompt}");
            }
            other => panic!("expected Retry, got {other:?}"),
        }

        assert!(matches!(decide_verify(&f, "c", 3, 3), VerifyDecision::Retry(_)));
        assert_eq!(decide_verify(&f, "c", 4, 3), VerifyDecision::Failed);
    }

    #[test]
    fn decide_verify_zero_retries_fails_immediately() {
        assert_eq!(decide_verify(&fail(1), "c", 1, 0), VerifyDecision::Failed);
    }

    #[test]
    fn decide_verify_timeout_counts_as_failure() {
        let to = VerifyOutcome {
            exit_code: -1,
            stdout: String::new(),
            stderr: "timed out".into(),
            timed_out: true,
        };
        match decide_verify(&to, "sleep 999", 1, 1) {
            VerifyDecision::Retry(p) => assert!(p.contains("timed out"), "{p}"),
            other => panic!("expected Retry, got {other:?}"),
        }
    }

    #[test]
    fn combined_output_truncates_middle() {
        let big = "x".repeat(10_000);
        let o = VerifyOutcome {
            exit_code: 1,
            stdout: big,
            stderr: String::new(),
            timed_out: false,
        };
        let c = o.combined_output();
        assert!(c.len() < 10_000, "len {}", c.len());
        assert!(c.contains("truncated"), "{c}");
        assert!(c.starts_with("xxx") && c.ends_with("xxx"));

        let o2 = VerifyOutcome {
            exit_code: 1,
            stdout: "a\n".into(),
            stderr: "b\n".into(),
            timed_out: false,
        };
        assert_eq!(o2.combined_output(), "a\nb");
    }

    #[test]
    fn verify_config_none_when_flag_absent_or_blank() {
        let cli = crate::cli::Cli::try_parse_from(["zen", "-p", "hi"]).unwrap();
        assert!(verify_config(&cli).is_none());
        let cli = crate::cli::Cli::try_parse_from(["zen", "-p", "--until", "   ", "hi"]).unwrap();
        assert!(verify_config(&cli).is_none());
        let cli = crate::cli::Cli::try_parse_from(["zen", "-p", "--until", "cargo test -q", "--max-verify-retries", "5", "hi"]).unwrap();
        let v = verify_config(&cli).unwrap();
        assert_eq!(v.command, "cargo test -q");
        assert_eq!(v.max_retries, 5);
    }

    #[test]
    fn verify_failed_result_marks_error() {
        let base = RunResult {
            record_type: "result".into(),
            subtype: "success".into(),
            is_error: false,
            duration_ms: 1,
            duration_api_ms: 0,
            num_turns: 2,
            result: "did the thing".into(),
            session_id: "s1".into(),
            total_cost_usd: 0.0,
            usage: Default::default(),
            model: "m".into(),
            permission_denials: vec![],
        };
        let r = verify_failed_result(&base, &fail(3), "make check", 2, "s1");
        assert!(r.is_error);
        assert_eq!(r.subtype, "error_during_execution");
        assert!(r.result.contains("did the thing"), "{}", r.result);
        assert!(r.result.contains("make check"), "{}", r.result);
        assert!(r.result.contains("exited with code 3"), "{}", r.result);
        assert!(r.result.contains("3 attempt(s)"), "{}", r.result);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_verify_command_captures_exit_and_streams() {
        let env = std::collections::HashMap::new();
        let dir = std::env::temp_dir();
        let ok = run_verify_command("exit 0", &dir, &env).await;
        assert!(ok.success());
        let f = run_verify_command("echo hello-out; echo hello-err >&2; exit 3", &dir, &env).await;
        assert_eq!(f.exit_code, 3);
        assert!(!f.success());
        assert!(f.stdout.contains("hello-out"), "{:?}", f.stdout);
        assert!(f.stderr.contains("hello-err"), "{:?}", f.stderr);
        assert!(f.combined_output().contains("hello-out"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_verify_command_runs_in_cwd() {
        let env = std::collections::HashMap::new();
        let dir = std::env::temp_dir().join("zen-verify-cwd-test");
        std::fs::create_dir_all(&dir).unwrap();
        let o = run_verify_command("pwd", &dir, &env).await;
        assert!(o.success());
        assert!(o.stdout.trim_end().ends_with("zen-verify-cwd-test"), "{:?}", o.stdout);
        let _ = std::fs::remove_dir(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_verify_command_timeout() {
        let env = std::collections::HashMap::new();


        std::env::set_var("VERIFY_TIMEOUT_SECS", "1");
        let o = run_verify_command("sleep 30", &std::env::temp_dir(), &env).await;
        std::env::remove_var("VERIFY_TIMEOUT_SECS");
        assert!(o.timed_out);
        assert!(!o.success());
    }
}

#[cfg(test)]
mod tools_flag_tests {
    use super::*;

    #[test]
    fn auto_compact_threshold_defaults_to_80_percent_of_context_window() {
        assert_eq!(resolve_auto_compact_threshold(None, None, 128_000), Some(102_400));
        assert_eq!(resolve_auto_compact_threshold(None, None, 200_000), Some(160_000));
    }

    #[test]
    fn auto_compact_flag_auto_and_true_setting_use_80_percent() {
        use serde_json::json;
        assert_eq!(resolve_auto_compact_threshold(Some("auto"), None, 100_000), Some(80_000));
        assert_eq!(resolve_auto_compact_threshold(None, Some(&json!(true)), 100_000), Some(80_000));

        assert_eq!(
            resolve_auto_compact_threshold(Some("auto"), Some(&json!(false)), 100_000),
            Some(80_000)
        );
    }

    #[test]
    fn auto_compact_flag_off_and_false_setting_disable() {
        use serde_json::json;
        assert_eq!(resolve_auto_compact_threshold(Some("off"), None, 100_000), None);
        assert_eq!(resolve_auto_compact_threshold(None, Some(&json!(false)), 100_000), None);

        assert_eq!(
            resolve_auto_compact_threshold(Some("off"), Some(&json!(5_000)), 100_000),
            None
        );
    }

    #[test]
    fn auto_compact_numeric_flag_and_setting_set_absolute_threshold() {
        use serde_json::json;
        assert_eq!(resolve_auto_compact_threshold(Some("5000"), None, 100_000), Some(5_000));
        assert_eq!(resolve_auto_compact_threshold(None, Some(&json!(3000)), 100_000), Some(3_000));

        assert_eq!(
            resolve_auto_compact_threshold(Some("7000"), Some(&json!(3000)), 100_000),
            Some(7_000)
        );
    }

    #[test]
    fn auto_compact_unexpected_setting_shape_falls_back_to_80_percent() {
        use serde_json::json;
        assert_eq!(resolve_auto_compact_threshold(None, Some(&json!("weird")), 100_000), Some(80_000));
        assert_eq!(resolve_auto_compact_threshold(None, Some(&json!(null)), 100_000), Some(80_000));
    }

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
