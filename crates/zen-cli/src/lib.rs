//! zen-cli library crate — public helpers reused by the binary and integration tests.
//! OWNER: worker C. Exposes the CLI surface, session/transcript helpers, output sinks and the
//! orchestration entry point `run`.

pub mod acp;
pub mod agent_run;
pub mod budget;
pub mod btw;
pub mod cli;
pub mod commands;
pub mod cost;
pub mod context;
pub mod deep_research;
pub mod envsnap;
pub mod eval;
pub mod eventlog;
pub mod export;
pub mod git;
pub mod ollama;
pub mod models;
pub mod mentions;
pub mod output;
pub mod planmode;
pub mod prompt_budget;
pub mod repl;
pub mod safety;
pub mod repl_polish;
pub mod repomap;
pub mod review;
pub mod session;
pub mod session_mgmt;
pub mod settings;
pub mod skills;
pub mod skill_registry;
pub mod system_prompt;

pub use cli::{Cli, Commands, CostArgs, EvalArgs, ExportArgs, ImportArgs, InputFormat, McpArgs, McpSubcommand, ModelsArgs, OutputFormat, PermissionMode, ReviewArgs, SessionSubcommand, SessionsArgs};
pub use settings::{resolve_config, ResolvedConfig};
pub use system_prompt::build_system_prompt;

use anyhow::Result;
use std::path::Path;

/// Top-level orchestrator: resolves config, assembles the agent, and runs the turn(s).
pub async fn run(cli: Cli) -> Result<i32> {
    let cwd = std::env::current_dir()?;
    let home = dirs::home_dir();
    run_with_home(cli, &cwd, home.as_deref()).await
}

/// Injectable `home`/`cwd` variant (used by tests).
pub async fn run_with_home(cli: Cli, cwd: &Path, home: Option<&Path>) -> Result<i32> {
    // ── ACP server mode (JSON-RPC over stdio) ──────────────────────────────
    if cli.acp {
        return acp::run_acp_server(&cli).await;
    }

    // ── Subcommands ──────────────────────────────────────────────────────
    if let Some(sub) = cli.subcommand.clone() {
        return run_subcommand(&cli, cwd, home, &sub).await;
    }

    // ── Headless -p mode ─────────────────────────────────────────────────
    if cli.print {
        return run_headless(&cli, cwd, home).await;
    }

    // ── REPL ─────────────────────────────────────────────────────────────
    ask_folder_trust(&cli, cwd, home);
    let cwd_buf = cwd.to_path_buf();
    repl::run_repl(&cli, &cwd_buf, home).await?;
    Ok(0)
}

async fn run_subcommand(
    cli: &Cli,
    cwd: &Path,
    home: Option<&Path>,
    sub: &Commands,
) -> Result<i32> {
    match sub {
        Commands::Doctor => {
            let cfg = settings::resolve_config_with_home(cli, cwd, home)?;
            println!("zen doctor");
            println!("  model: {}", cfg.model);
            println!("  base_url: {}", cfg.base_url);
            if cfg.ollama {
                println!("  provider: ollama (local)");
            }
            println!(
                "  api_key: {}",
                if cfg.api_key().is_some() {
                    "configured (hidden)"
                } else {
                    "not set"
                }
            );
            println!("  permission_mode: {}", cfg.permission_mode);
            println!("  folder_trusted: {}", cfg.folder_trusted);
            for s in &cfg.trust_skipped {
                println!("    skipped: {}", s);
            }
            // RateGate quota (g3): per-provider daily usage for `:free` tiers.
            let store = home.map(zen_agent::RateGate::default_store_path);
            for line in zen_agent::rategate::quota_report_for(store.as_deref()).lines() {
                println!("  {line}");
            }
            if cfg.loaded_sources.is_empty() {
                println!("  settings_sources: (none)");
            } else {
                println!("  settings_sources:");
                for s in &cfg.loaded_sources {
                    println!("    - {}", s);
                }
            }
            if !cfg.warnings.is_empty() {
                println!("  warnings:");
                for w in &cfg.warnings {
                    println!("    - {}", w);
                }
            }
            print!(
                "{}",
                safety::doctor_safety_checklist(env!("CARGO_PKG_VERSION"))
            );
            Ok(0)
        }
        Commands::Mcp(mcp) => {
            commands::mcp_command(cwd, home, mcp).await?;
            Ok(0)
        }
        Commands::Models(args) => {
            models::models_command(cli, cwd, home, args).await?;
            Ok(0)
        }
        Commands::Cost(args) => {
            cost_subcommand(cli, cwd, home, args)?;
            Ok(0)
        }
        Commands::Context(args) => {
            context::run_context(args.session.as_deref(), &cli.output_format, cwd, home)?;
            Ok(0)
        }
        Commands::ToolHelp { name } => {
            println!("{}", commands::tool_help(name));
            Ok(0)
        }
        Commands::Export(e) => {
            export::run_export(
                cwd,
                home,
                e.format,
                e.session.as_deref(),
                e.output.as_deref(),
                e.redact,
            )?;
            Ok(0)
        }
        Commands::Import(i) => {
            export::run_import(cwd, home, &i.file)?;
            Ok(0)
        }
        Commands::Skill(skill) => {
            skills::skill_command(cwd, home, &skill.command).await?;
            Ok(0)
        }
        Commands::Review(args) => {
            return review::run_review(cli, cwd, home, args).await;
        }
    }
}

/// `zen cost [--session <id>]`: render the session's cost ledger as a dashboard.
fn cost_subcommand(
    cli: &Cli,
    cwd: &Path,
    home: Option<&std::path::Path>,
    args: &CostArgs,
) -> anyhow::Result<()> {
    let Some(home) = home else {
        anyhow::bail!("cannot determine home directory");
    };
    // Resolve the session: explicit id/path, else the most recent session.
    let session_id = match &args.session {
        Some(s) => {
            if let Some(p) = session::find_session_path(home, s) {
                p.file_stem()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| s.clone())
            } else {
                s.clone()
            }
        }
        None => cost::latest_session_id(home)
            .ok_or_else(|| anyhow::anyhow!("no sessions found under ~/.zen/projects"))?,
    };
    let Some(ledger) = cost::load(home, &session_id) else {
        println!("No cost data recorded for session {}.", session_id);
        println!("Cost tracking starts with this version; run a session first.");
        return Ok(());
    };
    let summary = cost::summarize(&ledger.session_id, &ledger.entries);
    // Budget comes from the same precedence chain as the agent loop
    // (flag > env > config).
    let cfg = settings::resolve_config_with_home(cli, cwd, Some(home)).ok();
    let budget = crate::budget::resolve_budget(
        cli.max_budget_usd,
        cli.budget_warn_at,
        cfg.as_ref().and_then(|c| c.max_budget_usd),
        cfg.as_ref().and_then(|c| c.budget_warn_at),
    );
    match args.output_format {
        OutputFormat::Text => {
            print!("{}", cost::render_text(&summary));
            if let Some(policy) = &budget {
                println!(
                    "{}",
                    crate::budget::budget_line(policy, summary.total_cost_usd)
                );
            }
        }
        OutputFormat::Json | OutputFormat::StreamJson => {
            let mut v = cost::to_json(&summary);
            if let Some(policy) = &budget {
                v["budget_usd"] = serde_json::json!(policy.max_budget_usd);
                v["budget_warn_at"] = serde_json::json!(policy.warn_at);
            }
            println!("{}", serde_json::to_string(&v)?)
        }
        Commands::Sessions(s) => {
            commands::sessions_command(cli, cwd, home, s).await?;
            Ok(0)
        }
        Commands::Eval(args) => {
            let cfg = settings::resolve_config_with_home(cli, cwd, home)?;
            let model = args.model.clone().unwrap_or_else(|| cfg.model.clone());
            let opts = eval::EvalOptions {
                model,
                task_filter: args.task.clone(),
                max_turns: args.max_turns,
                base_url: cfg.base_url.clone(),
                api_key: cfg.api_key().map(|s| s.to_string()),
            };
            let report = eval::run_eval(&opts).await?;
            if args.json {
                println!("{}", report.to_json()?);
            } else {
                println!("{}", report.to_markdown());
            }
            // Exit 0 when the v0.3.0 bar (>=5/10) is met, so CI can gate on it.
            Ok(if report.passed_count * 2 >= report.task_count {
                0
            } else {
                1
            })
        }
    }
    Ok(())
}

/// Interactive start: in an untrusted folder that has project config able to run commands, ask
/// once whether to trust it; yes is saved to `~/.zen/trusted-folders.json` (folder trust).
fn ask_folder_trust(cli: &Cli, cwd: &Path, home: Option<&Path>) {
    use std::io::{BufRead, IsTerminal, Write};
    if cli.bare || settings::folder_trusted(cli, cwd, home) {
        return;
    }
    let gated = zen_ext::gated_project_config(cwd);
    if gated.is_empty() {
        return;
    }
    if std::io::stdin().is_terminal() {
        eprintln!("This folder has project config that can run commands: {}", gated.join("; "));
        eprint!("Trust this folder? [y/N] ");
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        let _ = std::io::stdin().lock().read_line(&mut answer);
        if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            match zen_ext::trust_folder(cwd, home) {
                Ok(()) => return,
                Err(e) => eprintln!("warning: could not save folder trust: {}", e),
            }
        }
    }
    eprintln!("{}", untrusted_notice(&gated));
}

fn untrusted_notice(skipped: &[String]) -> String {
    format!(
        "zen: folder not trusted; skipped project {} (use --trust-folder or ZEN_TRUST_FOLDER=1 to load)",
        skipped.join("; ")
    )
}

async fn run_headless(cli: &Cli, cwd: &Path, home: Option<&Path>) -> Result<i32> {
    use std::io::IsTerminal;

    let cfg = settings::resolve_config_with_home(cli, cwd, home)?;
    if cfg.ollama {
        crate::ollama::ensure_ollama_ready(&cfg.base_url, &cfg.model).await?;
    }
    if !cfg.trust_skipped.is_empty() {
        eprintln!("{}", untrusted_notice(&cfg.trust_skipped));
    }
    if cli.ephemeral {
        eprintln!("EPHEMERAL MODE — nothing will be saved");
    }
    let system_prompt = build_system_prompt(cli, cwd, home)?;

    // stdin (only for -p with text input format)
    let mut prompt_parts: Vec<String> = Vec::new();
    if cli.input_format == InputFormat::Text && !std::io::stdin().is_terminal() {
        let s = read_stdin()?;
        if !s.is_empty() {
            prompt_parts.push(s);
        }
    }
    let arg_prompt = cli.prompt.join(" ").trim().to_string();
    if !arg_prompt.is_empty() {
        prompt_parts.push(arg_prompt);
    }
    let full_prompt = prompt_parts.join("\n\n");
    if full_prompt.is_empty() {
        anyhow::bail!("No prompt provided. Pass a prompt argument or pipe stdin with -p.");
    }

    // Session id + transcript path.
    let (session_id, transcript_path, history_from) = resolve_session(cli, cwd, home)?;

    // --plan: plan turn, approval, then execution.
    if cli.plan {
        return agent_run::run_plan_flow(
            cli,
            cwd,
            home,
            &cfg,
            &system_prompt,
            &full_prompt,
            &session_id,
            transcript_path.as_deref(),
            history_from.as_deref(),
        )
        .await;
    }

    let code = agent_run::run_agent_headless(
        cli,
        cwd,
        home,
        &cfg,
        &system_prompt,
        &full_prompt,
        &session_id,
        transcript_path.as_deref(),
        history_from.as_deref(),
    )
    .await?;
    Ok(code)
}

/// Decide the session id, the transcript to write (None with `--no-session-persistence` or
/// `--ephemeral`) and the transcript to replay history from (`--resume`/`--continue`;
/// for `--fork-session` this is the *source* session while the new id gets a fresh file).
fn resolve_session(
    cli: &Cli,
    cwd: &Path,
    home: Option<&Path>,
) -> Result<(String, Option<std::path::PathBuf>, Option<std::path::PathBuf>)> {
    let home_path = home.map(|p| p.to_path_buf());
    // `--ephemeral` implies no persistence: nothing is written under ~/.zen.
    let persist = !cli.no_session_persistence && !cli.ephemeral;

    // --resume <id|path> / --continue
    let resumed: Option<std::path::PathBuf> = if let Some(h) = &home_path {
        if let Some(r) = &cli.resume {
            if r.is_empty() {
                None
            } else {
                Some(session::find_session_path(h, r).ok_or_else(|| {
                    anyhow::anyhow!("No conversation found with session ID or path: {}", r)
                })?)
            }
        } else if cli.continue_session {
            session::latest_session_for_cwd(h, &session::encode_cwd(cwd))
        } else {
            None
        }
    } else {
        // Session history lives under the home directory; silently starting a fresh
        // session here would make the user believe they resumed when they didn't.
        if cli.resume.as_deref().is_some_and(|r| !r.is_empty()) {
            anyhow::bail!("--resume needs a home directory to locate the session");
        }
        if cli.continue_session {
            anyhow::bail!("--continue needs a home directory to locate the session");
        }
        None
    };

    let session_id = if let Some(existing) = &resumed {
        if cli.fork_session {
            session::new_session_id()
        } else {
            existing
                .file_stem()
                .and_then(|s| s.to_str())
                .map(String::from)
                .unwrap_or_else(session::new_session_id)
        }
    } else {
        cli.session_id
            .clone()
            .unwrap_or_else(session::new_session_id)
    };

    if !persist {
        return Ok((session_id, None, resumed));
    }
    if let (Some(existing), false) = (&resumed, cli.fork_session) {
        return Ok((session_id, Some(existing.clone()), resumed.clone()));
    }
    let h = home_path.unwrap_or_else(std::env::temp_dir);
    let proj = session::ensure_session_dir(&h, cwd, &session_id)?;
    let path = session::session_path(&proj, &session_id);
    Ok((session_id, Some(path), resumed))
}

fn read_stdin() -> Result<String> {
    use std::io::Read;
    let mut buf = Vec::new();
    let mut handle = std::io::stdin().take(10 * 1024 * 1024 + 1);
    handle.read_to_end(&mut buf)?;
    if buf.len() > 10 * 1024 * 1024 {
        anyhow::bail!("stdin input exceeds 10 MB limit");
    }
    Ok(String::from_utf8_lossy(&buf).trim().to_string())
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn resolve_session_generates_id_and_path() {
        let dir = std::env::temp_dir().join(format!("zen_lib_sess_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cli = Cli::parse_from(["zen", "-p", "--session-id", "00000000-0000-4000-8000-0000000000aa", "hi"]);
        let (id, path, _) = resolve_session(&cli, &dir, Some(&dir)).unwrap();
        assert_eq!(id, "00000000-0000-4000-8000-0000000000aa");
        assert!(path.unwrap().ends_with("00000000-0000-4000-8000-0000000000aa.jsonl"));
    }

    #[test]
    fn resolve_session_no_persistence_returns_none_path() {
        let dir = std::env::temp_dir().join(format!("zen_lib_np_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cli = Cli::parse_from(["zen", "-p", "--no-session-persistence", "hi"]);
        let (_, path, _) = resolve_session(&cli, &dir, Some(&dir)).unwrap();
        assert!(path.is_none());
    }

    #[test]
    fn resolve_session_ephemeral_writes_nothing() {
        let dir = std::env::temp_dir().join(format!("zen_lib_eph_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cli = Cli::parse_from(["zen", "-p", "--ephemeral", "hi"]);
        let (id, path, _) = resolve_session(&cli, &dir, Some(&dir)).unwrap();
        assert!(!id.is_empty());
        assert!(path.is_none(), "ephemeral must not produce a transcript path");
        // Nothing session-related may be created under home.
        assert!(
            !dir.join(".zen").exists(),
            "ephemeral must not create ~/.zen entries"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_session_ephemeral_allows_readonly_resume() {
        let dir = std::env::temp_dir().join(format!("zen_lib_eph_rs_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A prior (non-ephemeral) session exists on disk.
        let proj = session::ensure_session_dir(&dir, &dir, "orig").unwrap();
        let p = session::session_path(&proj, "orig");
        session::append_typed(&p, "user", "orig", "cwd", "u", None, None, None).unwrap();

        let cli = Cli::parse_from(["zen", "-p", "--ephemeral", "--resume", "orig", "hi"]);
        let (id, path, src) = resolve_session(&cli, &dir, Some(&dir)).unwrap();
        assert_eq!(id, "orig");
        assert_eq!(src, Some(p), "ephemeral resume replays history read-only");
        assert!(path.is_none(), "ephemeral resume must not open a transcript for writing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_session_fork_gives_new_id() {
        let dir = std::env::temp_dir().join(format!("zen_lib_fork_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Create a prior session.
        let proj = session::ensure_session_dir(&dir, &dir, "orig").unwrap();
        let p = session::session_path(&proj, "orig");
        session::append_typed(&p, "user", "orig", "cwd", "u", None, None, None).unwrap();

        let cli = Cli::parse_from(["zen", "-p", "--resume", "orig", "--fork-session", "hi"]);
        let (id, path, src) = resolve_session(&cli, &dir, Some(&dir)).unwrap();
        assert_ne!(id, "orig");
        assert!(path.unwrap().ends_with(format!("{}.jsonl", id)));
        assert_eq!(src.unwrap(), p, "fork replays the original transcript");
    }

    #[test]
    fn resolve_session_continue_picks_latest() {
        let dir = std::env::temp_dir().join(format!("zen_lib_cont_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let proj = session::ensure_session_dir(&dir, &dir, "first").unwrap();
        let p1 = session::session_path(&proj, "first");
        session::append_typed(&p1, "user", "first", "cwd", "u", None, None, None).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(25));
        let proj2 = session::ensure_session_dir(&dir, &dir, "second").unwrap();
        let p2 = session::session_path(&proj2, "second");
        session::append_typed(&p2, "user", "second", "cwd", "u", None, None, None).unwrap();

        let cli = Cli::parse_from(["zen", "-p", "--continue", "hi"]);
        let (id, path, _) = resolve_session(&cli, &dir, Some(&dir)).unwrap();
        assert_eq!(id, "second");
        assert_eq!(path.unwrap(), p2);
    }
}
