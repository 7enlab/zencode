

















use zen_core::*;


pub const PLAN_MODE_INSTRUCTION: &str =
    "\n\n# Plan mode (active)\nYou are in plan mode. Mutating tools are unavailable. Research the \
     codebase with the read-only tools you have, then present a concrete, step-by-step plan with \
     exact file paths and commands. Do not write or edit files. End your final message with the \
     line `ExitPlanMode` once the plan is ready for approval.";


pub const PLAN_MODE_INACTIVE_NOTE: &str = "";



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






pub fn plan_mode_hides(tool_name: &str) -> bool {
    tool_name.starts_with("mcp__") || PLAN_MODE_DENIED_TOOLS.contains(&tool_name)
}





pub fn plan_mode_tools(defs: Vec<ToolDefinition>) -> Vec<ToolDefinition> {
    defs.into_iter()
        .filter(|d| !plan_mode_hides(&d.function.name))
        .collect()
}


pub fn system_prompt_for_mode(base: &str, mode: PermissionMode) -> String {
    if mode == PermissionMode::Plan && !base.contains("# Plan mode (active)") {
        format!("{}{}", base, PLAN_MODE_INSTRUCTION)
    } else {
        base.to_string()
    }
}




pub fn looks_like_plan_exit(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("exitplanmode") || lower.contains("/exitplan")
}


















pub fn safe_tail(messages: &[ChatMessage], keep: usize) -> Vec<ChatMessage> {
    if messages.is_empty() {
        return vec![];
    }
    let n = messages.len();
    let mut start = n.saturating_sub(keep);


    if start > 0 && start < n && messages[start].role == "tool" {
        let mut run_start = start;
        while run_start > 0 && messages[run_start - 1].role == "tool" {
            run_start -= 1;
        }
        let owns_everything = run_start > 0 && {
            let prev = &messages[run_start - 1];
            prev.role == "assistant"
                && prev
                    .tool_calls
                    .as_ref()
                    .map(|calls| {
                        messages[run_start..]
                            .iter()
                            .take_while(|m| m.role == "tool")
                            .filter_map(|m| m.tool_call_id.as_deref())
                            .all(|id| calls.iter().any(|c| c.id == id))
                    })
                    .unwrap_or(false)
        };
        if owns_everything {
            start = run_start - 1;
        }


    }

    while start < n && !window_is_valid(&messages[start..]) {
        start += 1;
    }
    if start < n {
        return messages[start..].to_vec();
    }

    let (es, ee) = most_recent_complete_exchange(messages);
    messages[es..ee].to_vec()
}




fn most_recent_complete_exchange(messages: &[ChatMessage]) -> (usize, usize) {
    let n = messages.len();
    let mut user_starts: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == "user")
        .map(|(i, _)| i)
        .collect();
    if !user_starts.contains(&0) {
        user_starts.push(0);
    }
    user_starts.sort_unstable();
    for &s in user_starts.iter().rev() {
        let end = user_starts
            .iter()
            .find(|&&i| i > s)
            .copied()
            .unwrap_or(n);
        for e in (s + 1..=end).rev() {
            if window_is_valid(&messages[s..e]) {
                return (s, e);
            }
        }
    }
    (0, 0)
}



fn window_is_valid(window: &[ChatMessage]) -> bool {

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
                None => return false,
            }
        }
    }
    open.is_empty()
}









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

        } else {
            out.push(m);
        }
    }

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

        let msgs = vec![
            ChatMessage::user("u"),
            tool_call_msg("c1"),
            ChatMessage::tool("c1", "result"),
            ChatMessage::assistant(Some("done".into()), None),
        ];


        let tail = safe_tail(&msgs, 2);
        assert_eq!(tail.len(), 3);
        assert_eq!(tail[0].role, "assistant");
        assert_eq!(tail[2].content.as_deref(), Some("done"));
    }




    #[test]
    fn safe_tail_pulls_in_owning_assistant_call() {

        let msgs = vec![ChatMessage::user("u"), tool_call_msg("c1"), ChatMessage::tool("c1", "r")];
        let tail = safe_tail(&msgs, 1);
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].role, "assistant");
        assert_eq!(tail[1].role, "tool");
        assert_eq!(tail[1].tool_call_id.as_deref(), Some("c1"));
    }

    #[test]
    fn safe_tail_includes_every_sibling_call() {

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

        assert_eq!(tail.len(), 3);
        assert_eq!(tail[0].role, "assistant");
        assert!(window_ok(&tail));
    }

    #[test]
    fn safe_tail_drops_unanswerable_prefix() {


        let msgs = vec![tool_call_msg("c1"), ChatMessage::user("later")];
        let tail = safe_tail(&msgs, 2);
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].content.as_deref(), Some("later"));
    }

    #[test]
    fn ensure_tool_responses_drops_orphans_and_fills_gaps() {


        let msgs = vec![
            tool_call_msg("c1"),
            ChatMessage::tool("c1", "r1"),
            ChatMessage::tool("cX", "orphan"),
            tool_call_msg("c2"),
        ];
        let out = ensure_tool_responses(msgs.clone());

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
    fn safe_tail_never_discards_freshest_complete_exchange() {


        let mut msgs = vec![
            ChatMessage::user("u"),
            tool_call_msg("c1"),
            ChatMessage::tool("c1", "fresh evidence"),
        ];

        if let Some(calls) = &mut msgs[1].tool_calls {
            calls.push(ToolCall {
                id: "c2".into(),
                call_type: "function".into(),
                function: FunctionCall { name: "Read".into(), arguments: "{}".into() },
            });
        }
        let tail = safe_tail(&msgs, 10);
        assert!(!tail.is_empty(), "tail must retain the complete exchange");
        assert_eq!(tail[0].role, "user");
        assert_eq!(tail[0].content.as_deref(), Some("u"));
    }

    #[test]
    fn safe_tail_grow_does_not_cross_exchange_boundaries() {



        let msgs = vec![
            ChatMessage::user("u"),
            tool_call_msg("c1"),
            ChatMessage::tool("c1", "r1"),
            ChatMessage::tool("cX", "orphan"),
        ];
        let tail = safe_tail(&msgs, 1);

        assert!(tail.iter().all(|m| m.tool_call_id.as_deref() != Some("cX")));
        assert!(window_ok(&tail));
        assert!(!tail.is_empty());
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
