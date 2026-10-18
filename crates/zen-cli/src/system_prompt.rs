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
/// Default role prompt. Distilled 2026-10-17: every behavioral rule of the original is
/// kept, at ~80% of the tokens (the original was already lean; the research's 40-60% figure
/// applies to prompts with accumulated cruft). The pre-distillation text lives in the test
/// module below as a regression guard (`distilled_prompt_is_smaller_than_original`).
pub const DEFAULT_SYSTEM_PROMPT: &str = "\
You are zen, a CLI coding agent: reading and editing code, running commands, searching \
repositories, explaining findings.
Read before editing (Read/Grep/Glob, never guess file contents). Small focused edits, match \
project style. Use Bash for builds, tests and version control; run the tests you touch. \
Be concise: lead with the result, skip restating the task. If ambiguous, assume reasonably \
and state the assumption instead of stalling. Never print secrets (API keys, tokens). \
Do not commit unless asked. Report failures honestly, with the command output and error.

Change workflow (bug fix, feature, refactor):
1. Explore: read the relevant files and their callers before editing.
2. Reproduce: run the failing test or write a tiny script; confirm it fails for the reason described.
3. Fix the root cause with the smallest change that does it. Do not edit, delete or weaken \
tests unless the user asks you to.
4. Rerun your reproduction and the project's tests; read the output.
5. Check edge cases the task implies (empty input, boundaries, off-by-one, other call sites), \
then report what you changed.
";

/// Verbosity instruction appended by `--terse` / `/terse`. Saves output tokens by ruling out
/// preamble, restated context and unasked summaries (research: 40-65% fewer output tokens).
pub const TERSE_STYLE_BLOCK: &str = "\
Terse mode: be maximally concise. No preamble, no restated context, no summaries unless asked. \
Lead with the result, then only the details needed to act. One-line explanations; bullets over \
paragraphs.";

pub fn with_terse_style(prompt: &str) -> String {
    format!("{}\n\n{}", prompt.trim_end(), TERSE_STYLE_BLOCK)
}

pub fn without_terse_style(prompt: &str) -> String {
    let suffix = format!("\n\n{}", TERSE_STYLE_BLOCK);
    prompt.strip_suffix(&suffix).unwrap_or(prompt).to_string()
}

pub fn has_terse_style(prompt: &str) -> bool {
    prompt.ends_with(&TERSE_STYLE_BLOCK)
}

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

        // Repository map (top Rust symbols by cross-file references), same once-per-session
        // pattern; --bare and --repomap-budget 0 suppress it.
        if let Some(map) = crate::repomap::repo_map(cwd, cli.map_tokens) {
            parts.push(map.text().to_string());
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

    // Cache-aware ordering (#49): the stable content (role, snapshot, memory, skills)
    // comes first so it is byte-identical across turns and provider prompt
    // caches can hit; the volatile environment block (date, git branch/status)
    // goes after the marker, so it never poisons the cached prefix.
    let prompt = format!(
        "{}{}{}",
        parts.join("\n\n"),
        zen_core::CACHE_VOLATILE_MARKER,
        environment_block(cwd)
    );

    // Terse verbosity instruction (#62, output-side saving), applied last so it is always a
    // suffix: `/terse` toggles it by stripping/appending.
    if cli.terse {
        Ok(with_terse_style(&prompt))
    } else {
        Ok(prompt)
    }
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

    /// The pre-distillation wording (2026-10-17), kept as a regression guard: the distilled
    /// prompt must stay well under it while keeping every behavioral rule.
const ORIGINAL_SYSTEM_PROMPT: &str = "\
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

    #[test]
    fn distilled_prompt_is_smaller_than_original() {
        // chars/4 token estimate, same estimator the CLI uses elsewhere.
        // The original was already lean, so the honest bar is <85%, not the 40-60%
        // quoted for cruft-laden prompts; the rule-preservation test below is the
        // real quality gate.
        let new_toks = DEFAULT_SYSTEM_PROMPT.len() / 4;
        let old_toks = ORIGINAL_SYSTEM_PROMPT.len() / 4;
        assert!(
            new_toks * 100 < old_toks * 85,
            "distilled prompt did not shrink: {} vs {} tokens (est.)",
            new_toks,
            old_toks
        );
    }

    #[test]
    fn distilled_prompt_keeps_every_behavioral_rule() {
        let p = DEFAULT_SYSTEM_PROMPT;
        for rule in [
            "You are zen",
            "reading and editing code",
            "Read before editing",
            "Read/Grep/Glob",
            "never guess file contents",
            "match project style",
            "Use Bash for builds",
            "run the tests you touch",
            "Be concise",
            "assume reasonably",
            "Never print secrets",
            "Do not commit unless asked",
            "Report failures honestly",
            "Explore",
            "Reproduce",
            "root cause",
            "Rerun",
            "edge cases",
        ] {
            assert!(p.contains(rule), "distilled prompt lost rule {rule:?}");
        }
    }

    #[test]
    fn terse_flag_appends_style_block() {
        let c = cli(&["--bare", "--terse"]);
        let p = build_system_prompt(&c, Path::new("/tmp"), None).unwrap();
        assert!(p.contains(TERSE_STYLE_BLOCK), "terse block missing: {p}");
        assert!(has_terse_style(&p));
        let plain = build_system_prompt(&cli(&["--bare"]), Path::new("/tmp"), None).unwrap();
        assert!(!plain.contains(TERSE_STYLE_BLOCK));
        assert!(!has_terse_style(&plain));
    }

    #[test]
    fn terse_applies_on_custom_system_prompt_too() {
        let c = cli(&["--bare", "--terse", "--system-prompt", "CUSTOM ROLE"]);
        let p = build_system_prompt(&c, Path::new("/tmp"), None).unwrap();
        assert!(p.starts_with("CUSTOM ROLE"));
        assert!(has_terse_style(&p));
    }

    #[test]
    fn terse_style_roundtrip() {
        let p = with_terse_style("BASE PROMPT");
        assert!(has_terse_style(&p));
        assert_eq!(without_terse_style(&p), "BASE PROMPT");
        assert_eq!(without_terse_style("BASE PROMPT"), "BASE PROMPT");
    }

    #[test]
    fn repo_map_in_prompt_unless_bare_or_zero() {
        let dir = std::env::temp_dir().join(format!("zen_sp_map_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "pub fn entry() {}\nfn main() { entry(); }\n").unwrap();

        let p = build_system_prompt(&cli(&["--setting-sources", ""]), &dir, None).unwrap();
        assert!(p.contains("Repository map"), "map present by default");
        assert!(p.contains("entry"), "map lists the symbol");

        let bare = build_system_prompt(&cli(&["--bare"]), &dir, None).unwrap();
        assert!(!bare.contains("Repository map"), "--bare suppresses the map");

        let zero = build_system_prompt(&cli(&["--repomap-budget", "0"]), &dir, None).unwrap();
        assert!(!zero.contains("Repository map"), "--repomap-budget 0 suppresses the map");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
