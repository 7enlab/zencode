//! The built-in system prompt (written from scratch) plus environment info, memory,
//! skills list and user-provided text (SPEC §3).
//!
//! Assembly order matters and mirrors what the model is expected to see: the role prompt first
//! (replaced entirely by `--system-prompt`), then the project snapshot (`envsnap.rs`),
//! then project memory (`ZEN.md`) and the available skills list, then any
//! `--append-system-prompt` text, and finally a short environment block (cwd, OS, date, git).
//! `--bare` suppresses the snapshot/memory/skills/id sections so a scripted
//! run is reproducible.
//!
//! Cache-aware ordering: everything before [`zen_core::CACHE_VOLATILE_MARKER`] is the
//! *stable* prefix and must be byte-identical across turns of a session so provider
//! prompt caches (Anthropic `cache_control`, OpenAI automatic caching) can hit.
//! The environment block is volatile (it carries the current date and git
//! branch/status) and rides last, after the marker, so it never breaks the cache.

use crate::cli::Cli;
use anyhow::Result;
use std::path::Path;

/// Default role prompt. Concise, tool-oriented, no vendor text.
pub const DEFAULT_SYSTEM_PROMPT: &str = "\
You are zen, a command-line coding agent. You help the user with software engineering tasks in \
the terminal: reading and editing code, running commands, searching repositories and explaining \
what you find.

Guidelines:
- Inspect the code before changing it. Use Read/Grep/Glob rather than guessing at file contents.
- Prefer small, focused edits. Keep existing style and conventions of the project.
- Use Bash for builds, tests and version control. Run the tests you touch.
- Be concise in your replies. Lead with the result; skip restating the task.
- When a task is ambiguous, make a reasonable assumption and state it, instead of stalling.
- Never print secrets (API keys, tokens). Do not commit unless asked.
- Report failures honestly: show the command output and the error.

Workflow for a change to code (bug fix, feature, refactor):
1. Explore: find and read the relevant files and their callers before editing.
2. Reproduce: run the failing test, or write a tiny script that shows the problem, and confirm \
it fails for the reason described.
3. Fix the root cause in the source with the smallest change that does it. Do not edit, delete \
or weaken tests unless the user asks you to.
4. Rerun your reproduction and the project's tests; read the output.
5. Think about edge cases the task implies (empty input, boundaries, off-by-one, other \
call sites) and check them too. Only then report what you changed.
";

/// Build the full system prompt for a session.
pub fn build_system_prompt(cli: &Cli, cwd: &Path, home: Option<&Path>) -> Result<String> {
    let mut parts: Vec<String> = Vec::new();

    // Base prompt (replace entirely when --system-prompt is given).
    match &cli.system_prompt {
        Some(s) => parts.push(s.clone()),
        None => parts.push(DEFAULT_SYSTEM_PROMPT.to_string()),
    }

    if !cli.bare {
        // Project snapshot (tree, build/test commands, toolchains), computed once per session
        // so the prompt prefix stays cache-stable (r7).
        if let Some(snap) = crate::envsnap::project_snapshot(cwd) {
            parts.push(snap);
        }

        // Memory (ZEN.md files).
        let mem = zen_ext::memory_prompt(cwd, home);
        if !mem.trim().is_empty() {
            parts.push(mem);
        }

        // Skills list (name + description only).
        let skills = zen_ext::discover_skills(cwd, home);
        let skills_txt = zen_ext::skills_prompt(&skills);
        if !skills_txt.trim().is_empty() {
            parts.push(skills_txt);
        }
    }

    // --append-system-prompt.
    if let Some(extra) = &cli.append_system_prompt {
        if !extra.trim().is_empty() {
            parts.push(extra.clone());
        }
    }

    // Cache-aware ordering: the stable content (role, snapshot, memory, skills)
    // comes first so it is byte-identical across turns and provider prompt
    // caches can hit; the volatile environment block (date, git branch/status)
    // goes last, after the marker, so it never poisons the cached prefix.
    Ok(format!(
        "{}{}{}",
        parts.join("\n\n"),
        zen_core::CACHE_VOLATILE_MARKER,
        environment_block(cwd)
    ))
}

/// The `Environment:` block appended to every system prompt. Public so callers that replace
/// the prompt entirely (e.g. `--agent`) can still include it.
pub fn environment_block(cwd: &Path) -> String {
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let mut s = String::from("Environment:\n");
    s.push_str(&format!("- working directory: {}\n", cwd.display()));
    s.push_str(&format!("- platform: {}\n", crate::git::os_info()));
    s.push_str(&format!("- date: {}\n", date));
    // Both git probes run concurrently under one 2 s budget (perf p0).
    let (branch, status) = crate::git::git_info(cwd);
    if let Some(branch) = branch {
        s.push_str(&format!("- git branch: {}\n", branch));
    }
    if let Some(status) = status {
        s.push_str(&format!("- git status: {}\n", status));
    }
    s.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn cli(args: &[&str]) -> Cli {
        let mut v = vec!["zen"];
        v.extend_from_slice(args);
        Cli::parse_from(v)
    }

    #[test]
    fn default_prompt_contains_role_and_env() {
        let c = cli(&["--bare"]);
        let p = build_system_prompt(&c, Path::new("/tmp"), None).unwrap();
        assert!(p.contains("zen"));
        assert!(p.contains("working directory: /tmp"));
        assert!(p.contains("date:"));
    }

    /// Anti-pattern guard: the stable (cacheable) system-prompt prefix must never
    /// contain volatile values (today's date, request ids, timestamps). A single
    /// changing byte in the prefix silently invalidates the provider prompt cache
    /// for the whole session.
    #[test]
    fn stable_prefix_contains_no_volatile_values() {
        let c = cli(&["--bare"]);
        let p = build_system_prompt(&c, Path::new("/tmp"), None).unwrap();
        let (stable, volatile) = zen_core::split_stable_volatile(&p);
        assert!(p.contains(zen_core::CACHE_VOLATILE_MARKER), "prompt must carry the volatile marker");
        assert!(!volatile.is_empty(), "environment block must be present after the marker");
        // Today's date lives in the volatile tail, never in the stable prefix.
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        assert!(!stable.contains(&today), "stable prefix leaks today's date — cache killer");
        assert!(volatile.contains(&today), "volatile tail should carry the date");
        // Stable prefix is time-invariant: two builds must agree byte-for-byte.
        let p2 = build_system_prompt(&c, Path::new("/tmp"), None).unwrap();
        let (stable2, _) = zen_core::split_stable_volatile(&p2);
        assert_eq!(stable, stable2, "stable prefix must be deterministic across builds");
    }

    #[test]
    fn volatile_block_comes_last() {
        // Cache-aware ordering: the model still sees the environment, but only
        // after the stable content, so the cacheable prefix is maximal.
        let c = cli(&["--bare"]);
        let p = build_system_prompt(&c, Path::new("/tmp"), None).unwrap();
        let env_pos = p.find("Environment:").expect("env block present");
        let marker_pos = p.find(zen_core::CACHE_VOLATILE_MARKER).expect("marker present");
        assert!(marker_pos < env_pos, "environment block must come after the volatile marker");
    }

    #[test]
    fn system_prompt_flag_replaces_default() {
        let c = cli(&["--system-prompt", "CUSTOM ROLE", "--bare"]);
        let p = build_system_prompt(&c, Path::new("/tmp"), None).unwrap();
        assert!(p.starts_with("CUSTOM ROLE"));
        assert!(!p.contains("You are zen, a command-line coding agent"));
    }

    #[test]
    fn append_system_prompt_appended() {
        let c = cli(&["--append-system-prompt", "EXTRA RULE", "--bare"]);
        let p = build_system_prompt(&c, Path::new("/tmp"), None).unwrap();
        assert!(p.contains("EXTRA RULE"));
    }

    #[test]
    fn bare_skips_memory_and_skills() {
        let dir = std::env::temp_dir().join(format!("zen_sp_bare_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ZEN.md"), "PROJECT MEMORY NOTE").unwrap();

        let c = cli(&["--bare"]);
        let p = build_system_prompt(&c, &dir, None).unwrap();
        assert!(!p.contains("PROJECT MEMORY NOTE"));
    }

    #[test]
    fn non_bare_includes_memory() {
        let dir = std::env::temp_dir().join(format!("zen_sp_mem_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ZEN.md"), "PROJECT MEMORY NOTE").unwrap();

        let c = cli(&["--setting-sources", ""]);
        let p = build_system_prompt(&c, &dir, None).unwrap();
        assert!(p.contains("PROJECT MEMORY NOTE"), "prompt was: {}", p);
    }

    #[test]
    fn r7_project_snapshot_included_unless_bare() {
        let dir = std::env::temp_dir().join(format!("zen_sp_snap_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]").unwrap();
        let p = build_system_prompt(&cli(&["--setting-sources", ""]), &dir, None).unwrap();
        assert!(p.contains("Project snapshot") && p.contains("`cargo test`"), "{}", p);
        let again = build_system_prompt(&cli(&["--setting-sources", ""]), &dir, None).unwrap();
        assert_eq!(p, again, "same inputs give a byte-identical prompt (cache-stable)");
        let bare = build_system_prompt(&cli(&["--bare"]), &dir, None).unwrap();
        assert!(!bare.contains("Project snapshot"), "{}", bare);
    }

    #[test]
    fn c2_default_prompt_has_change_workflow() {
        // r7 bench C2: explore → reproduce → fix → rerun → edge cases, tests left alone
        // (https://www.anthropic.com/engineering/swe-bench-sonnet).
        let p = build_system_prompt(&cli(&["-p", "x"]), Path::new("/tmp"), None).unwrap();
        for step in ["Explore", "Reproduce", "root cause", "Rerun", "edge cases", "Do not edit, delete"] {
            assert!(p.contains(step), "missing {step:?} in {p}");
        }
        let custom = build_system_prompt(&cli(&["-p", "x", "--system-prompt", "R"]), Path::new("/tmp"), None).unwrap();
        assert!(!custom.contains("Workflow for a change"), "--system-prompt replaces it");
    }
}
