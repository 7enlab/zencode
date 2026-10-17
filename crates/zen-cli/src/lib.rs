//! zen-cli library crate — public helpers reused by the binary and integration tests.
//! OWNER: worker C. Exposes the CLI surface, session/transcript helpers, output sinks and the
//! orchestration entry point `run`.

pub mod agent_run;
pub mod cli;
pub mod commands;
pub mod envsnap;
pub mod eventlog;
pub mod git;
pub mod output;
pub mod repl;
pub mod session;
pub mod settings;
pub mod system_prompt;

pub use cli::{Cli, Commands, InputFormat, McpArgs, McpSubcommand, OutputFormat, PermissionMode};
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
    // ── Subcommands ──────────────────────────────────────────────────────
    if let Some(sub) = cli.subcommand.clone() {
        return run_subcommand(&cli, cwd, home, &sub).await;
    }

    // ── Headless -p mode ─────────────────────────────────────────────────
    if cli.print {
        return run_headless(&cli, cwd, home).await;
    }

    // ── REPL ─────────────────────────────────────────────────────────────
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
            println!(
                "  api_key: {}",
                if cfg.api_key().is_some() {
                    "configured (hidden)"
                } else {
                    "not set"
                }
            );
            println!("  permission_mode: {}", cfg.permission_mode);
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
            Ok(0)
        }
        Commands::Mcp(mcp) => {
            commands::mcp_command(cwd, home, mcp).await?;
            Ok(0)
        }
    }
}

async fn run_headless(cli: &Cli, cwd: &Path, home: Option<&Path>) -> Result<i32> {
    use std::io::IsTerminal;

    let cfg = settings::resolve_config_with_home(cli, cwd, home)?;
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

/// Decide the session id, the transcript to write (None with `--no-session-persistence`) and
/// the transcript to replay history from (`--resume`/`--continue`; for `--fork-session` this is
/// the *source* session while the new id gets a fresh file).
fn resolve_session(
    cli: &Cli,
    cwd: &Path,
    home: Option<&Path>,
) -> Result<(String, Option<std::path::PathBuf>, Option<std::path::PathBuf>)> {
    let home_path = home.map(|p| p.to_path_buf());
    let persist = !cli.no_session_persistence;

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
