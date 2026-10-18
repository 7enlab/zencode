//! Interactive REPL (SPEC §2): line reading from stdin, slash commands, `!cmd` shell passthrough,
//! `#note` memory, permission prompts, Ctrl-D exit. No new TUI crates.

use crate::cli::Cli;
use crate::commands;
use anyhow::Result;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::path::Path;
use std::sync::Arc;
use zen_core::{EventSink, RunResult};

/// Outcome of handling one line of REPL input.
enum LineAction {
    Continue,
    Exit,
}

/// Run the interactive loop. Returns when the user exits.
pub async fn run_repl(cli: &Cli, cwd: &Path, home: Option<&Path>) -> Result<()> {
    let cfg = crate::settings::resolve_config_with_home(cli, cwd, home)?;
    if cfg.ollama {
        crate::ollama::ensure_ollama_ready(&cfg.base_url, &cfg.model).await?;
    }
    let mut session_id = cli
        .session_id
        .clone()
        .unwrap_or_else(crate::session::new_session_id);
    let proj_dir = crate::session::ensure_session_dir(
        home.unwrap_or(&std::env::temp_dir()),
        cwd,
        &session_id,
    )?;
    let transcript_path = crate::session::session_path(&proj_dir, &session_id);
    // Every turn (and every slash command that rewrites history, e.g. /compact) is persisted
    // through the parentUuid-chained transcript writer (SPEC C9).
    let mut writer = crate::session::TranscriptWriter::new(transcript_path.clone(), &session_id, cwd);

    println!(
        "zen {} — model {}, cwd {}",
        env!("CARGO_PKG_VERSION"),
        cfg.model,
        cwd.display()
    );
    println!("Type /help for commands, !cmd for shell, #note to remember, Ctrl-D to exit.");

    let assembly = crate::agent_run::assemble(
        cli,
        cwd,
        home,
        &cfg,
        &session_id,
        Some(&transcript_path),
    )
    .await?;

    let llm = crate::eventlog::wrap(cfg.gated_llm_client(Arc::new(zen_core::NullSink), home), assembly.recorder.as_ref());

    let mut config = build_repl_config(cli, &cfg, cwd);
    let _ = &mut config;

    let mut agent = zen_agent::Agent::new(
        llm,
        assembly.registry.clone(),
        assembly.permissions.clone(),
        Some(Arc::new(crate::agent_run::ReplPrompter)),
        assembly.hooks.clone(),
        Arc::new(zen_core::NullSink),
        {
            let mut ctx = crate::agent_run::tool_context_public(cwd, &cfg, cli, &session_id);
            ctx.session_dir = Some(crate::session::session_scratch_dir(&transcript_path));
            ctx
        },
        config,
    );
    agent.journal = assembly.recorder.clone().map(|r| r as Arc<dyn zen_core::ToolJournal>);

    // SessionStart hooks (C3.2/C3.11): their context goes in ahead of the first prompt.
    repl_session_start(&mut agent, &session_id, cwd, &cfg, "startup").await;

    // Ctrl-C ends the running turn; at the prompt, twice within 2 s exits (C8.9).
    let mut interrupts = ReplInterrupts::install();
    let mut repl = ReplState {
        session_id: &mut session_id,
        writer: &mut writer,
        mcp_status: assembly.mcp_status.clone(),
        interrupts: interrupts.as_mut(),
        recorder: assembly.recorder.clone(),
    };

    loop {
        print!("> ");
        std::io::stdout().flush().ok();
        // One read per line (no long-lived stdin lock), so the permission prompter can read
        // its answer from stdin too.
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) | Err(_) => break, // Ctrl-D / EOF
            Ok(_) => {}
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match handle_line(trimmed, cli, cwd, home, &cfg, &mut repl, &mut agent).await? {
            LineAction::Exit => break,
            LineAction::Continue => {}
        }
    }

    let session_id = repl.session_id.clone();
    crate::agent_run::run_session_end(&agent.hooks, &session_id, cwd, &cfg, "prompt_input_exit")
        .await;
    println!("bye");
    Ok(())
}

/// Run SessionStart hooks (`source` = startup | clear | compact) and add any additional context
/// they return to the conversation as a user message.
async fn repl_session_start(
    agent: &mut zen_agent::Agent,
    session_id: &str,
    cwd: &Path,
    cfg: &crate::settings::ResolvedConfig,
    source: &str,
) {
    let hooks = agent.hooks.clone();
    if let Some(ctx) =
        crate::agent_run::run_session_start(&hooks, session_id, cwd, cfg, source).await
    {
        agent
            .messages
            .push(zen_core::ChatMessage::user(format!("[SessionStart hook context]\n{}", ctx)));
    }
}

/// Mutable per-session REPL state shared by the line handlers.
struct ReplState<'a> {
    session_id: &'a mut String,
    writer: &'a mut crate::session::TranscriptWriter,
    mcp_status: Vec<(String, String)>,
    interrupts: Option<&'a mut ReplInterrupts>,
    /// Session event log (p0b); re-pointed when /clear or /resume switches the transcript.
    recorder: Option<Arc<crate::eventlog::Recorder>>,
}

/// SIGINT handling for the REPL. A background task owns the signal stream: during a turn it
/// forwards each Ctrl-C to the turn (which is then cancelled); at the prompt the first Ctrl-C
/// prints a hint and a second one within 2 s exits with 130.
struct ReplInterrupts {
    rx: tokio::sync::mpsc::UnboundedReceiver<()>,
    in_turn: Arc<AtomicBool>,
}

impl ReplInterrupts {
    fn install() -> Option<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sig = signal(SignalKind::interrupt()).ok()?;
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let in_turn = Arc::new(AtomicBool::new(false));
            let flag = in_turn.clone();
            tokio::spawn(async move {
                let mut last: Option<std::time::Instant> = None;
                while sig.recv().await.is_some() {
                    if flag.load(Ordering::SeqCst) {
                        let _ = tx.send(());
                        last = None;
                        continue;
                    }
                    if last.map(|t| t.elapsed() < std::time::Duration::from_secs(2)).unwrap_or(false) {
                        eprintln!();
                        std::process::exit(130);
                    }
                    last = Some(std::time::Instant::now());
                    eprintln!("\n(press Ctrl-C again to exit, or Ctrl-D)");
                }
            });
            Some(Self { rx, in_turn })
        }
        #[cfg(not(unix))]
        {
            None
        }
    }
}

/// Run one agent turn that Ctrl-C can cancel. `None` = interrupted: the history is restored to
/// what it was before the turn (no orphaned tool calls, C10.3) and processes the turn started
/// (e.g. a Bash child and its own children) are stopped.
async fn run_turn_interruptible(
    agent: &mut zen_agent::Agent,
    prompt: &str,
    interrupts: Option<&mut ReplInterrupts>,
) -> Option<RunResult> {
    let Some(intr) = interrupts else {
        return Some(agent.run_turn(prompt).await);
    };
    while intr.rx.try_recv().is_ok() {} // drop Ctrl-C presses from before this turn
    let before = agent.messages().to_vec();
    let pre = crate::agent_run::descendants_of(std::process::id());
    intr.in_turn.store(true, Ordering::SeqCst);
    let out = {
        let turn = agent.run_turn(prompt);
        tokio::pin!(turn);
        tokio::select! {
            r = &mut turn => Some(r),
            _ = intr.rx.recv() => {
                // Stop the turn's processes while it is still alive (see kill_descendants).
                crate::agent_run::kill_new_descendants(&pre);
                None
            }
        }
    };
    intr.in_turn.store(false, Ordering::SeqCst);
    if out.is_none() {
        agent.set_messages(before);
        eprintln!("Interrupted.");
    }
    out
}

/// Print a turn's outcome.
fn print_turn(result: Option<RunResult>) {
    match result {
        Some(r) if r.is_error => eprintln!("error: {}", r.result),
        Some(r) if !r.result.is_empty() => println!("{}", r.result),
        _ => {}
    }
}

async fn handle_line(
    line: &str,
    cli: &Cli,
    cwd: &Path,
    home: Option<&Path>,
    cfg: &crate::settings::ResolvedConfig,
    repl: &mut ReplState<'_>,
    agent: &mut zen_agent::Agent,
) -> Result<LineAction> {
    if let Some(rest) = line.strip_prefix('!') {
        run_shell(rest, cwd, cfg);
        return Ok(LineAction::Continue);
    }
    if let Some(note) = line.strip_prefix('#') {
        match zen_ext::append_memory_note(cwd, note) {
            Ok(p) => println!("remembered in {}", p.display()),
            Err(e) => eprintln!("could not append note: {}", e),
        }
        return Ok(LineAction::Continue);
    }
    // After /clear the history starts over in a new transcript.
    let clearing = line == "/clear" || line.starts_with("/clear ");
    let before = if clearing { vec![] } else { agent.messages().to_vec() };
    if line.starts_with('/') {
        let action = handle_slash(line, cli, cwd, home, cfg, repl, agent).await;
        persist(repl.writer, &before, agent);
        return action;
    }

    // Normal prompt → agent turn (prompt, tool calls, tool results and answer are persisted).
    let result = run_turn_interruptible(agent, line, repl.interrupts.as_deref_mut()).await;
    persist(repl.writer, &before, agent);
    print_turn(result);
    Ok(LineAction::Continue)
}

async fn handle_slash(
    line: &str,
    cli: &Cli,
    cwd: &Path,
    home: Option<&Path>,
    cfg: &crate::settings::ResolvedConfig,
    repl: &mut ReplState<'_>,
    agent: &mut zen_agent::Agent,
) -> Result<LineAction> {
    let session_id = repl.session_id.clone();
    let session_id = session_id.as_str();
    let mut it = line.splitn(2, char::is_whitespace);
    let cmd = it.next().unwrap_or("").to_string();
    let arg = it.next().unwrap_or("").trim().to_string();

    match cmd.as_str() {
        "/exit" | "/quit" => return Ok(LineAction::Exit),
        "/help" => {
            println!("Built-in commands:");
            for c in commands::BUILTIN_COMMANDS {
                println!("  {}", c);
            }
            // Same discovery as expansion (zen-ext): commands + user-invocable skills (C5.2/C5.3).
            let custom = zen_ext::discover_slash_commands(cwd, home);
            if !custom.is_empty() {
                println!("Custom commands and skills:");
                for c in &custom {
                    match &c.description {
                        Some(d) => println!("  /{}  {}", c.name, d),
                        None => println!("  /{}", c.name),
                    }
                }
            }
        }
        "/clear" => {
            crate::agent_run::run_session_end(&agent.hooks, session_id, cwd, cfg, "clear").await;
            agent.set_messages(vec![]);
            // A new session with its own transcript (SPEC §6: /clear starts a new session).
            let new_id = crate::session::new_session_id();
            let path = repl.writer.path.with_file_name(format!("{}.jsonl", new_id));
            agent.ctx.session_dir = Some(crate::session::session_scratch_dir(&path));
            if let Some(r) = &repl.recorder {
                r.set_path(path.clone());
            }
            *repl.writer = crate::session::TranscriptWriter::new(path, &new_id, cwd);
            *repl.session_id = new_id.clone();
            agent.ctx.session_id = new_id.clone();
            repl_session_start(agent, &new_id, cwd, cfg, "clear").await;
            println!("context cleared, new session: {}", new_id);
        }
        "/compact" => {
            let focus = if arg.is_empty() { None } else { Some(arg.as_str()) };
            match agent.compact(focus, "manual").await {
                Ok(()) => {
                    println!("context compacted");
                    repl_session_start(agent, session_id, cwd, cfg, "compact").await;
                }
                Err(e) => eprintln!("compact failed: {}", e),
            }
        }
        "/model" => {
            if arg.is_empty() {
                println!("{}", cfg.model);
            } else {
                agent.config.model = arg.clone();
                println!("model set to {}", arg);
            }
        }
        "/cost" => println!("{}", commands::cost_report(&agent.usage)),
        "/quota" => {
            let store = home.map(zen_agent::RateGate::default_store_path);
            println!("{}", zen_agent::rategate::quota_report_for(store.as_deref()));
        }
        "/context" => println!(
            "{}",
            commands::context_report(
                &agent.config.system_prompt,
                &agent.tools.definitions(),
                agent.messages(),
                agent.config.context_window,
                agent.last_prompt_tokens,
            )
        ),
        "/add-dir" => {
            if arg.is_empty() {
                eprintln!("usage: /add-dir <path>");
            } else {
                let p = cwd.join(&arg);
                match p.canonicalize() {
                    Ok(dir) if dir.is_dir() => {
                        if !agent.ctx.extra_dirs.contains(&dir) {
                            agent.ctx.extra_dirs.push(dir.clone());
                        }
                        println!("added working directory {}", dir.display());
                    }
                    _ => eprintln!("not a directory: {}", p.display()),
                }
            }
        }
        "/export" => {
            let file = if arg.is_empty() { format!("zen-session-{}.md", session_id) } else { arg.clone() };
            let path = cwd.join(&file);
            match std::fs::write(&path, commands::transcript_markdown(session_id, agent.messages())) {
                Ok(()) => println!("conversation exported to {}", path.display()),
                Err(e) => eprintln!("export failed: {}", e),
            }
        }
        "/rename" => {
            if arg.is_empty() {
                eprintln!("usage: /rename <name>");
            } else {
                match crate::session::append_title(&repl.writer.path, session_id, &arg) {
                    Ok(()) => println!("session renamed to {}", arg),
                    Err(e) => eprintln!("rename failed: {}", e),
                }
            }
        }
        "/review" | "/init" => {
            let prompt = if cmd == "/review" {
                commands::review_prompt(&arg)
            } else {
                commands::init_prompt()
            };
            print_turn(run_turn_interruptible(agent, &prompt, repl.interrupts.as_deref_mut()).await);
        }
        "/status" => {
            println!("model: {}", agent.config.model);
            println!("base_url: {}", cfg.base_url);
            println!("permission_mode: {}", cfg.permission_mode);
            println!("session_id: {}", session_id);
            if cfg.loaded_sources.is_empty() {
                println!("settings: (none)");
            } else {
                for s in &cfg.loaded_sources {
                    println!("settings: {}", s);
                }
            }
        }
        "/permissions" => {
            println!("allow: {:?}", cfg.permissions.allow);
            println!("ask:   {:?}", cfg.permissions.ask);
            println!("deny:  {:?}", cfg.permissions.deny);
            println!("mode:  {}", cfg.permission_mode);
        }
        "/config" => {
            if arg.is_empty() {
                println!("model: {}", agent.config.model);
                println!("permission_mode: {}", cfg.permission_mode);
                println!("base_url: {}", cfg.base_url);
                println!(
                    "autoCompact: {}",
                    cfg.auto_compact.as_ref().map(|v| v.to_string()).unwrap_or_else(|| "true".into())
                );
                println!("set a value with /config key=value (saved to .zen/settings.local.json)");
            } else if let Some((k, v)) = arg.split_once('=') {
                match save_local_setting(cwd, k.trim(), v.trim()) {
                    Ok(p) => println!(
                        "saved {} = {} to {} (applies to new sessions)",
                        k.trim(),
                        v.trim(),
                        p.display()
                    ),
                    Err(e) => eprintln!("could not save setting: {}", e),
                }
            } else {
                eprintln!("usage: /config key=value");
            }
        }
        "/memory" => {
            for (p, _) in zen_ext::memory_files(cwd, home) {
                println!("{}", p.display());
            }
        }
        "/todos" => {
            let todos = agent.ctx.todos.lock().unwrap().clone();
            if todos.is_empty() {
                println!("(no todos)");
            } else {
                for t in todos {
                    println!("[{}] {}", t.status, t.content);
                }
            }
        }
        "/agents" => {
            let agents = zen_mcp::load_agents(cwd, home, cli.agents.as_deref());
            for a in agents {
                println!("{}: {}", a.name, a.description);
            }
        }
        "/mcp" => println!("{}", commands::mcp_report(&repl.mcp_status)),
        "/hooks" => {
            if cfg.hooks.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                println!("(no hooks)");
            } else {
                println!("{}", serde_json::to_string_pretty(&cfg.hooks)?);
            }
        }
        "/skills" => {
            let skills = zen_ext::discover_skills(cwd, home);
            if skills.is_empty() {
                println!("(no skills)");
            } else {
                for s in skills {
                    println!("{}: {}", s.name, s.description);
                }
            }
        }
        "/resume" | "/continue" => {
            let home_dir = home.map(Path::to_path_buf).unwrap_or_else(std::env::temp_dir);
            let sessions = crate::session::list_sessions_for_cwd(&home_dir, cwd);
            if arg.is_empty() {
                if sessions.is_empty() {
                    println!("(no previous sessions)");
                } else {
                    println!("Available sessions (/resume <id|name>):");
                    for (id, path) in sessions {
                        match crate::session::session_title(&path) {
                            Some(t) => println!("  {}  {}", id, t),
                            None => println!("  {}", id),
                        }
                    }
                }
            } else {
                // By id/path (any project), else by `/rename` title in this directory.
                let found = crate::session::find_session_path(&home_dir, &arg).or_else(|| {
                    sessions
                        .into_iter()
                        .find(|(_, p)| crate::session::session_title(p).as_deref() == Some(arg.as_str()))
                        .map(|(_, p)| p)
                });
                match found {
                    Some(path) => {
                        let id = path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or_default()
                            .to_string();
                        let (committed, history) = crate::eventlog::resume_history(&path)?;
                        agent.ctx.session_dir = Some(crate::session::session_scratch_dir(&path));
                        if let Some(r) = &repl.recorder {
                            r.set_path(path.clone());
                        }
                        *repl.writer = crate::session::TranscriptWriter::new(path, &id, cwd);
                        if history != committed {
                            if let Err(e) = repl.writer.persist_turn(&committed, &history) {
                                eprintln!("warning: could not write transcript: {}", e);
                            }
                        }
                        agent.set_messages(history);
                        *repl.session_id = id.clone();
                        agent.ctx.session_id = id.clone();
                        println!("resumed session {} ({} messages)", id, agent.messages().len());
                    }
                    None => eprintln!("no session found: {}", arg),
                }
            }
        }
        other => {
            // Custom command?
            let name = other.trim_start_matches('/');
            // Custom commands and user-invocable skills (skill wins), SPEC C5.2/C5.3.
            if zen_ext::find_slash_command(cwd, home, name).is_some() {
                let (expanded, slash_cmd) = crate::agent_run::expand_slash_prompt_cmd(
                    cli, cwd, home, line, cfg, session_id,
                )?;
                // `model` / `allowed-tools` frontmatter apply to this turn only (C5.2).
                let saved_model = agent.config.model.clone();
                let saved_perms = agent.permissions.clone();
                if let Some(c) = &slash_cmd {
                    if let Some(m) = c.model.as_ref().filter(|m| !m.is_empty() && *m != "inherit") {
                        agent.config.model = m.clone();
                    }
                    if !c.allowed_tools.is_empty() {
                        agent.permissions =
                            crate::agent_run::build_permissions_with(cwd, cfg, cli, &c.allowed_tools);
                    }
                }
                let result =
                    run_turn_interruptible(agent, &expanded, repl.interrupts.as_deref_mut()).await;
                agent.config.model = saved_model;
                agent.permissions = saved_perms;
                print_turn(result);
            } else {
                eprintln!("unknown command: {}", other);
            }
        }
    }
    Ok(LineAction::Continue)
}

/// `/config key=value`: set a top-level key in `.zen/settings.local.json`. The value is parsed as
/// JSON when it is valid JSON (`true`, `5000`, `["a"]`), otherwise stored as a string.
fn save_local_setting(cwd: &Path, key: &str, value: &str) -> Result<std::path::PathBuf> {
    if key.is_empty() {
        anyhow::bail!("empty key");
    }
    let path = cwd.join(".zen").join("settings.local.json");
    let mut obj = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str::<serde_json::Value>(&text)
            .map_err(|e| anyhow::anyhow!("{} is not valid JSON: {}", path.display(), e))?,
        Err(_) => serde_json::json!({}),
    };
    let map = obj
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("{} is not a JSON object", path.display()))?;
    let parsed = serde_json::from_str(value).unwrap_or_else(|_| serde_json::Value::String(value.into()));
    map.insert(key.to_string(), parsed);
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, serde_json::to_string_pretty(&obj)? + "\n")?;
    Ok(path)
}

fn build_repl_config(
    cli: &Cli,
    cfg: &crate::settings::ResolvedConfig,
    cwd: &Path,
) -> zen_agent::AgentConfig {
    let mut config = zen_agent::AgentConfig {
        model: cfg.model.clone(),
        fallback_models: cfg.fallback_models.clone(),
        system_prompt: crate::build_system_prompt(cli, cwd, None).unwrap_or_default(),
        max_turns: cfg.max_turns.max(1),
        permission_mode: crate::agent_run::core_mode(cfg.permission_mode),
        tool_timeouts: cfg.tool_timeouts.clone(),
        max_continuations: cfg.max_continuations,
        ..Default::default()
    };
    if let Some(w) = cfg.context_window {
        config.context_window = w;
    }
    crate::agent_run::apply_verify_settings(&mut config, cfg, true);
    config
}

fn run_shell(cmd: &str, cwd: &Path, cfg: &crate::settings::ResolvedConfig) {
    let sh = zen_core::resolve_shell("bash");
    let mut command = std::process::Command::new(&sh.program);
    command.args(sh.args(cmd)).current_dir(cwd);
    for (k, v) in &cfg.env {
        command.env(k, v);
    }
    match command.output() {
        Ok(out) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            print!("{}", text);
            if !out.status.success() {
                eprintln!("(exit {})", out.status.code().unwrap_or(-1));
            }
        }
        Err(e) => eprintln!("failed to run command: {}", e),
    }
}

/// Append what changed in the agent history since `before` to the transcript.
fn persist(
    writer: &mut crate::session::TranscriptWriter,
    before: &[zen_core::ChatMessage],
    agent: &zen_agent::Agent,
) {
    if let Err(e) = writer.persist_turn(before, agent.messages()) {
        eprintln!("warning: could not write transcript: {}", e);
    }
}

/// Unused sink alias kept for symmetry with headless mode.
pub type ReplSink = Arc<dyn EventSink>;
