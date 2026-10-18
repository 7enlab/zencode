//! Mechanical plan mode for the REPL (parity P4, matrix C2).
//!
//! Claude Code's plan mode is *mechanically* enforced read-only: the harness withholds
//! mutating tools from the model and rejects any mutating call that slips through, instead
//! of trusting the model to behave. ZenCode matches that contract in two layers:
//!
//! 1. [`zen_agent::plan::plan_mode_tools`] hides `Write`/`Edit`/state-changing `Bash`/…
//!    from the `tools` array of every request while plan mode is active.
//! 2. `prepare_call` in zen-agent rejects a hallucinated mutating call with the tool error
//!    `"Tool '<name>' is not available in plan mode"` — the tool never executes and no
//!    permission denial is recorded, so the turn stays a clean plan.
//!
//! This module owns the REPL side of the contract: the `/plan` toggle, the `[PLAN]` prompt
//! badge, the approve-to-exit flow, and storing the approved plan in the session transcript.

use zen_agent::AgentConfig;
use zen_core::PermissionMode;

/// Badge printed in the REPL prompt while plan mode is active.
pub const PLAN_BADGE: &str = "[PLAN]";

/// True for the exact words that approve a pending plan (case-insensitive,
/// surrounding whitespace ignored). Anything else is treated as a follow-up prompt.
pub fn is_approve_word(s: &str) -> bool {
    matches!(s.trim().to_ascii_lowercase().as_str(), "approve" | "yes")
}

/// Remove trailing plan-exit markers (`ExitPlanMode`, `/exitplan`, backticked variants)
/// from plan text before it is stored or shown back to the user. Markers in the middle of
/// the text are kept — only the trailing signal lines are stripped.
pub fn strip_plan_exit_markers(text: &str) -> String {
    let mut lines: Vec<&str> = text.lines().collect();
    while let Some(last) = lines.last() {
        let t = last.trim().trim_matches('`').to_ascii_lowercase();
        if t == "exitplanmode" || t == "/exitplan" {
            lines.pop();
        } else {
            break;
        }
    }
    lines.join("\n").trim_end().to_string()
}

/// Switch a live agent to `mode`, recomputing the system prompt from the *base* prompt so
/// the plan-mode instruction is added exactly once when entering plan mode and fully
/// removed when leaving it.
///
/// Takes `&mut AgentConfig` (not `&mut Agent`) so the toggle is unit-testable without a
/// full agent. The mechanical enforcement itself lives in zen-agent and reads
/// `config.permission_mode`; this only flips the flag the enforcement reads.
pub fn set_permission_mode(
    config: &mut AgentConfig,
    mode: PermissionMode,
    base_system_prompt: &str,
) {
    config.permission_mode = mode;
    config.system_prompt = zen_agent::plan::system_prompt_for_mode(base_system_prompt, mode);
}

/// The REPL prompt: `[PLAN] > ` while plan mode is active, `> ` otherwise.
pub fn prompt_for(mode: PermissionMode) -> &'static str {
    if mode == PermissionMode::Plan {
        "[PLAN] > "
    } else {
        "> "
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approve_words_match_exactly() {
        assert!(is_approve_word("approve"));
        assert!(is_approve_word("YES"));
        assert!(is_approve_word("  Approve  "));
        assert!(!is_approve_word("yes please"));
        assert!(!is_approve_word("approved"));
        assert!(!is_approve_word("approve the plan"));
        assert!(!is_approve_word(""));
        assert!(!is_approve_word("no"));
    }

    #[test]
    fn strip_markers_only_trailing() {
        assert_eq!(strip_plan_exit_markers("1. do X\nExitPlanMode"), "1. do X");
        assert_eq!(strip_plan_exit_markers("1. do X\n`ExitPlanMode`"), "1. do X");
        assert_eq!(strip_plan_exit_markers("plan\n/exitplan\n"), "plan");
        assert_eq!(
            strip_plan_exit_markers("1. do X\nExitPlanMode\n/exitplan"),
            "1. do X"
        );
        // Markers in the middle are content, not the exit signal.
        assert_eq!(
            strip_plan_exit_markers("ExitPlanMode\n1. do X"),
            "ExitPlanMode\n1. do X"
        );
        assert_eq!(strip_plan_exit_markers("no markers"), "no markers");
        assert_eq!(strip_plan_exit_markers(""), "");
    }

    #[test]
    fn prompt_badge_only_in_plan_mode() {
        assert_eq!(prompt_for(PermissionMode::Plan), "[PLAN] > ");
        assert_eq!(prompt_for(PermissionMode::Default), "> ");
        assert_eq!(prompt_for(PermissionMode::AcceptEdits), "> ");
        assert_eq!(prompt_for(PermissionMode::BypassPermissions), "> ");
        assert_eq!(prompt_for(PermissionMode::DontAsk), "> ");
        assert!(prompt_for(PermissionMode::Plan).contains(PLAN_BADGE));
    }

    #[test]
    fn toggle_on_off_is_clean_and_idempotent() {
        let base = "you are a test";
        let mut config = AgentConfig {
            permission_mode: PermissionMode::Default,
            system_prompt: base.to_string(),
            ..Default::default()
        };
        // Enter plan mode: flag flips and the instruction is added exactly once.
        set_permission_mode(&mut config, PermissionMode::Plan, base);
        assert_eq!(config.permission_mode, PermissionMode::Plan);
        assert!(config.system_prompt.contains("# Plan mode (active)"));
        assert_eq!(
            config.system_prompt.matches("# Plan mode (active)").count(),
            1
        );
        // Enabling again does not duplicate the instruction.
        set_permission_mode(&mut config, PermissionMode::Plan, base);
        assert_eq!(
            config.system_prompt.matches("# Plan mode (active)").count(),
            1
        );
        // Leaving plan mode restores the pristine base prompt.
        set_permission_mode(&mut config, PermissionMode::AcceptEdits, base);
        assert_eq!(config.permission_mode, PermissionMode::AcceptEdits);
        assert_eq!(config.system_prompt, base);
        assert!(!config.system_prompt.contains("# Plan mode (active)"));
    }
}
