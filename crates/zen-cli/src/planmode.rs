use zen_agent::AgentConfig;
use zen_core::PermissionMode;

pub const PLAN_BADGE: &str = "[PLAN]";

pub fn is_approve_word(s: &str) -> bool {
    matches!(s.trim().to_ascii_lowercase().as_str(), "approve" | "yes")
}

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

pub fn set_permission_mode(
    config: &mut AgentConfig,
    mode: PermissionMode,
    base_system_prompt: &str,
) {
    config.permission_mode = mode;
    config.system_prompt = zen_agent::plan::system_prompt_for_mode(base_system_prompt, mode);
}

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

        set_permission_mode(&mut config, PermissionMode::Plan, base);
        assert_eq!(config.permission_mode, PermissionMode::Plan);
        assert!(config.system_prompt.contains("# Plan mode (active)"));
        assert_eq!(
            config.system_prompt.matches("# Plan mode (active)").count(),
            1
        );

        set_permission_mode(&mut config, PermissionMode::Plan, base);
        assert_eq!(
            config.system_prompt.matches("# Plan mode (active)").count(),
            1
        );

        set_permission_mode(&mut config, PermissionMode::AcceptEdits, base);
        assert_eq!(config.permission_mode, PermissionMode::AcceptEdits);
        assert_eq!(config.system_prompt, base);
        assert!(!config.system_prompt.contains("# Plan mode (active)"));
    }
}
