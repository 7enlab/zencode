//! Plan-mode behavior (SPEC §5, Appendix B4) and history-surgery helpers.
//!
//! Plan mode exists so the model can research a codebase and propose a plan without touching
//! anything. SPEC B4 records two things that make zen's plan mode distinct:
//!
//! 1. **Mutating tools are hidden from the tool list**, not merely denied. A model that never
//!    sees `Write` will not try to call it, so the run cannot waste turns on denied calls, and
//!    `permission_denials` stays empty as observed in the black-box run.
//! 2. **The result of a plan-mode turn is the plan text** with `subtype: "success"`. The model is
//!    told (via [`PLAN_MODE_INSTRUCTION`]) to present a plan and stop.
//!
//! There is no separate `ExitPlan` tool in zen's core tool set. Instead the model signals
//! completion by returning a message with no tool calls, and [`looks_like_plan_exit`] detects the
//! explicit markers (`/exitplan` or `ExitPlanMode`) so callers can tell "plan finished" from
//! "plan still in progress". The CLI owns the approval prompt, since that is a UI concern.
//!
//! This module also hosts [`safe_tail`], the history-slicing helper shared with compaction.

use zen_core::*;

/// Appended to the system prompt while plan mode is active (SPEC §5).
pub const PLAN_MODE_INSTRUCTION: &str =
    "\n\n# Plan mode (active)\nYou are in plan mode. Mutating tools are unavailable. Research the \
     codebase with the read-only tools you have, then present a concrete, step-by-step plan with \
     exact file paths and commands. Do not write or edit files. End your final message with the \
     line `ExitPlanMode` once the plan is ready for approval.";

/// System-prompt fragment appended when plan mode is *not* active, so the model knows it may act.
pub const PLAN_MODE_INACTIVE_NOTE: &str = "";

/// Tools hidden from the model in plan mode. Matched by exact name; MCP tools are handled
/// separately below.
const PLAN_MODE_DENIED_TOOLS: &[&str] = &[
    "Write",
    "Edit",
    "MultiEdit",
    "NotebookEdit",
    "Bash",
    "BashOutput",
    "KillShell",
    "JobOutput",
    "JobKill",
    "WebFetch",
    "Task",
];

/// Should `tool_name` be hidden from the tool list in plan mode?
///
/// Built-in mutating tools are listed explicitly; every MCP tool (`mcp__server__tool`) is treated
/// as mutating because zen cannot know what a remote server will do, matching SPEC §4's
/// classification of MCP tools as mutating.
pub fn plan_mode_hides(tool_name: &str) -> bool {
    tool_name.starts_with("mcp__") || PLAN_MODE_DENIED_TOOLS.contains(&tool_name)
}

/// Filter a tool list for plan mode: drop mutating tools, keep the read-only ones in order.
///
/// Kept pure (no registry access) so it is trivially testable and reusable by the CLI when it
/// renders the init event's `tools` array.
pub fn plan_mode_tools(defs: Vec<ToolDefinition>) -> Vec<ToolDefinition> {
    defs.into_iter()
        .filter(|d| !plan_mode_hides(&d.function.name))
        .collect()
}

/// Effective system prompt for a mode: the base prompt plus the plan instruction when active.
pub fn system_prompt_for_mode(base: &str, mode: PermissionMode) -> String {
    if mode == PermissionMode::Plan && !base.contains("# Plan mode (active)") {
        format!("{}{}", base, PLAN_MODE_INSTRUCTION)
    } else {
        base.to_string()
    }
}

/// Detect the model declaring its plan finished (SPEC §5's `ExitPlanMode` convention).
///
/// Case-insensitive; matches the standalone tool-like marker or the `/exitplan` slash form.
pub fn looks_like_plan_exit(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("exitplanmode") || lower.contains("/exitplan")
}

/// Return the longest suffix of `messages` that is safe to keep after compaction.
///
/// `keep` is the desired number of trailing messages (clamped to `messages.len()`).
/// OpenAI-compatible APIs reject both a `role:"tool"` message without its owning assistant
/// `tool_calls` message and an assistant `tool_calls` message without a `role:"tool"` answer for
/// each call, so a naive cut can produce an unsendable history. Repair happens in two steps:
///
/// 1. **Grow**: if the cut lands on tool results, the window is extended backwards to their owning
///    assistant message, so the most recent tool exchange (usually the freshest evidence the
///    model is working from) survives compaction intact. The window may therefore be *longer*
///    than `keep` by the size of one exchange.
/// 2. **Shrink**: if the window is still invalid (e.g. a dangling call that was never answered),
///    messages are dropped from the front until what remains is a valid exchange. In the
///    pathological case nothing is kept and compaction starts from the summary alone.
pub fn safe_tail(messages: &[ChatMessage], keep: usize) -> Vec<ChatMessage> {
    if messages.is_empty() {
        return vec![];
    }
    let mut start = messages.len().saturating_sub(keep);
    while start > 0 && start < messages.len() && messages[start].role == "tool" {
        start -= 1;
    }
    while start < messages.len() && !window_is_valid(&messages[start..]) {
        start += 1;
    }
    messages[start..].to_vec()
}

/// A window is valid when every `tool` message's id was opened by an assistant `tool_calls`
/// message earlier in the window, and every opened id was answered (no dangling calls).
fn window_is_valid(window: &[ChatMessage]) -> bool {
    // Ids opened by tool_calls but not yet answered. A tool result removes its id.
    let mut open: Vec<&str> = Vec::new();
    for m in window {
        if m.role == "assistant" {
            if let Some(calls) = &m.tool_calls {
                open.extend(calls.iter().map(|c| c.id.as_str()));
            }
        } else if m.role == "tool" {
            let Some(id) = m.tool_call_id.as_deref() else { return false };
            match open.iter().rposition(|o| *o == id) {
                Some(pos) => {
                    open.remove(pos);
                }
                None => return false, // orphan tool result
            }
        }
    }
    open.is_empty()
}

/// Return a copy of `messages` made safe to send to an OpenAI-compatible API:
///
/// - orphan `tool` messages (no preceding assistant `tool_calls` for that id) are dropped, and
/// - any assistant `tool_calls` without a following `tool` message are followed by a synthetic
///   "[No response was recorded for this tool call.]" result so the pair count always balances.
///
/// This is a *request-time* transform; the stored history is untouched. It lets a resumed session
/// start even if a prior crash left an incomplete exchange.
pub fn ensure_tool_responses(messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    let mut out: Vec<ChatMessage> = Vec::with_capacity(messages.len());
    let mut open_ids: Vec<String> = Vec::new();
    for m in messages {
        if m.role == "assistant" {
            out.push(m.clone());
            if let Some(calls) = &m.tool_calls {
                open_ids.extend(calls.iter().map(|c| c.id.clone()));
            }
        } else if m.role == "tool" {
            let owns = m.tool_call_id.as_ref().and_then(|id| {
                open_ids
                    .iter()
                    .position(|o| o == id)
                    .map(|pos| {
                        open_ids.remove(pos);
                        true
                    })
            });
            if owns.unwrap_or(false) {
                out.push(m);
            }
            // else: orphan tool message -> silently dropped
        } else {
            out.push(m);
        }
    }
    // Close any dangling tool calls still open.
    for id in open_ids {
        out.push(ChatMessage::tool(
            id,
            "[No response was recorded for this tool call.]",
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_call_msg(id: &str) -> ChatMessage {
        ChatMessage::assistant(
            None,
            Some(vec![ToolCall {
                id: id.into(),
                call_type: "function".into(),
                function: FunctionCall { name: "Read".into(), arguments: "{}".into() },
            }]),
        )
    }

    #[test]
    fn safe_tail_keeps_plain_tail() {
        let msgs: Vec<ChatMessage> = (0..6).map(|i| ChatMessage::user(format!("m{}", i))).collect();
        let tail = safe_tail(&msgs, 2);
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].content.as_deref(), Some("m4"));
    }

    #[test]
    fn safe_tail_drops_orphan_tool_message() {
        // [user, assistant(tool_call c1), tool c1, assistant text]
        let msgs = vec![
            ChatMessage::user("u"),
            tool_call_msg("c1"),
            ChatMessage::tool("c1", "result"),
            ChatMessage::assistant(Some("done".into()), None),
        ];
        // keep=2 would start at the tool message; the owning call is pulled in instead of
        // dropping the result.
        let tail = safe_tail(&msgs, 2);
        assert_eq!(tail.len(), 3);
        assert_eq!(tail[0].role, "assistant");
        assert_eq!(tail[2].content.as_deref(), Some("done"));
    }

    /// A tool result whose owning assistant message sits just *before* the window is the ideal
    /// case: the assistant message must be pulled in so the pair stays together, and the window
    /// grows by one. Here keep=1 starts at the tool result, so the owner comes along.
    #[test]
    fn safe_tail_pulls_in_owning_assistant_call() {
        // [user, assistant(tool_call c1), tool c1] with keep=1 -> keep the assistant + tool pair.
        let msgs = vec![ChatMessage::user("u"), tool_call_msg("c1"), ChatMessage::tool("c1", "r")];
        let tail = safe_tail(&msgs, 1);
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].role, "assistant");
        assert_eq!(tail[1].role, "tool");
        assert_eq!(tail[1].tool_call_id.as_deref(), Some("c1"));
    }

    #[test]
    fn safe_tail_includes_every_sibling_call() {
        // An assistant message with two tool calls needs both results kept.
        let assistant = ChatMessage::assistant(
            None,
            Some(vec![
                ToolCall {
                    id: "c1".into(),
                    call_type: "function".into(),
                    function: FunctionCall { name: "Read".into(), arguments: "{}".into() },
                },
                ToolCall {
                    id: "c2".into(),
                    call_type: "function".into(),
                    function: FunctionCall { name: "Read".into(), arguments: "{}".into() },
                },
            ]),
        );
        let msgs = vec![ChatMessage::user("u"), assistant, ChatMessage::tool("c1", "a"), ChatMessage::tool("c2", "b")];
        let tail = safe_tail(&msgs, 1);
        // The very last message is a tool result, so the whole exchange must be pulled in.
        assert_eq!(tail.len(), 3);
        assert_eq!(tail[0].role, "assistant");
        assert!(window_ok(&tail));
    }

    #[test]
    fn safe_tail_drops_unanswerable_prefix() {
        // A dangling assistant tool_calls with no result anywhere in the history can never be
        // made valid, so every window containing it must be dropped.
        let msgs = vec![tool_call_msg("c1"), ChatMessage::user("later")];
        let tail = safe_tail(&msgs, 2);
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].content.as_deref(), Some("later"));
    }

    #[test]
    fn ensure_tool_responses_drops_orphans_and_fills_gaps() {
        // [assistant(c1), tool c1, orphan tool cX, assistant(c2)]
        // c2 has no result -> synthesized; cX is orphan -> dropped.
        let msgs = vec![
            tool_call_msg("c1"),
            ChatMessage::tool("c1", "r1"),
            ChatMessage::tool("cX", "orphan"),
            tool_call_msg("c2"),
        ];
        let out = ensure_tool_responses(msgs.clone());
        // orphan dropped, c2 gets a synthetic result appended.
        assert!(!out.iter().any(|m| m.tool_call_id.as_deref() == Some("cX")));
        assert!(out
            .iter()
            .any(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some("c2")));
        assert_eq!(out.iter().filter(|m| m.role == "tool").count(), 2);
    }

    #[test]
    fn ensure_tool_responses_leaves_clean_history_alone() {
        let msgs: Vec<ChatMessage> = (0..4).map(|i| ChatMessage::user(format!("m{}", i))).collect();
        assert_eq!(ensure_tool_responses(msgs.clone()), msgs);
    }

    /// Test-local mirror of the validity rule (kept independent of the implementation).
    fn window_ok(window: &[ChatMessage]) -> bool {
        let mut open: Vec<&str> = Vec::new();
        for m in window {
            if let Some(calls) = &m.tool_calls {
                open.extend(calls.iter().map(|c| c.id.as_str()));
            } else if m.role == "tool" {
                let Some(id) = m.tool_call_id.as_deref() else { return false };
                match open.iter().rposition(|o| *o == id) {
                    Some(p) => {
                        open.remove(p);
                    }
                    None => return false,
                }
            }
        }
        open.is_empty()
    }

    #[test]
    fn safe_tail_handles_empty_and_oversized() {
        assert!(safe_tail(&[], 4).is_empty());
        let msgs = vec![ChatMessage::user("a"), ChatMessage::user("b")];
        assert_eq!(safe_tail(&msgs, 99).len(), 2);
    }

    #[test]
    fn plan_mode_hides_mutating_and_mcp_tools() {
        assert!(plan_mode_hides("Write"));
        assert!(plan_mode_hides("Edit"));
        assert!(plan_mode_hides("Bash"));
        assert!(plan_mode_hides("Task"));
        assert!(plan_mode_hides("mcp__server__tool"));
        assert!(!plan_mode_hides("Read"));
        assert!(!plan_mode_hides("Grep"));
        assert!(!plan_mode_hides("TodoWrite"));
    }

    #[test]
    fn plan_mode_tools_filters_definitions() {
        let mk = |name: &str| ToolDefinition {
            tool_type: "function".into(),
            function: FunctionDefinition {
                name: name.into(),
                description: String::new(),
                parameters: serde_json::json!({"type":"object"}),
            },
        };
        let defs = vec![mk("Read"), mk("Write"), mk("Bash"), mk("Grep")];
        let kept: Vec<String> = plan_mode_tools(defs)
            .into_iter()
            .map(|d| d.function.name)
            .collect();
        assert_eq!(kept, vec!["Read", "Grep"]);
    }

    #[test]
    fn plan_prompt_is_added_once() {
        let p = system_prompt_for_mode("base", PermissionMode::Plan);
        assert!(p.contains("# Plan mode (active)"));
        // Idempotent.
        assert_eq!(system_prompt_for_mode(&p, PermissionMode::Plan), p);
        assert_eq!(system_prompt_for_mode("base", PermissionMode::Default), "base");
    }

    #[test]
    fn plan_exit_detection() {
        assert!(looks_like_plan_exit("Here is the plan.\nExitPlanMode"));
        assert!(looks_like_plan_exit("/exitplan"));
        assert!(!looks_like_plan_exit("still researching"));
    }
}
