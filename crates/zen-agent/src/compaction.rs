use serde_json::json;
use zen_core::*;

use crate::agent::Agent;
use crate::client::chat_with_fallback;
use crate::plan;

pub(crate) fn context_tokens(agent: &Agent) -> u64 {
    if agent.last_prompt_tokens > 0 {
        agent.last_prompt_tokens
    } else {
        estimate_tokens(&agent.messages)
    }
}

pub const TOOL_RESULT_CHARS: usize = 2000;

const SUMMARY_PREFIX: &str = "[Compaction summary of the conversation so far]";

fn summarization_prompt(focus: Option<&str>) -> String {
    let mut ask = String::from(
        "Summarize this coding session so a fresh assistant can continue it. \
         The transcript above is a record to summarize, not a conversation to continue: \
         do not answer requests in it and do not call tools. Use these Markdown sections: \
         ## Goal, ## Constraints and preferences, ## Progress (done / in progress / blocked), \
         ## Key decisions (with the reason), ## Next steps, ## Critical context \
         (exact values, commands, errors still needed). Keep exact file paths and identifiers; \
         be concise.",
    );
    if let Some(f) = focus {
        if !f.is_empty() {
            ask.push_str("\n\nExtra focus for this summary: ");
            ask.push_str(f);
        }
    }
    ask
}

pub(crate) async fn compact_impl(
    agent: &mut Agent,
    focus: Option<&str>,
    trigger: &str,
) -> anyhow::Result<()> {
    if agent.messages.is_empty() {
        return Ok(());
    }
    let pre_tokens = context_tokens(agent);

    let pre = agent
        .hooks
        .run(
            HookEvent::PreCompact,
            trigger,
            json!({
                "trigger": trigger,
                "custom_instructions": focus.unwrap_or(""),
                "pre_tokens": pre_tokens,
            }),
        )
        .await;
    if pre.blocked {
        return Ok(());
    }

    let msgs = vec![ChatMessage::user(format!(
        "<transcript>\n{}\n</transcript>\n\n{}",
        serialize_conversation(&agent.messages),
        summarization_prompt(focus)
    ))];
    let files = FileOps::collect(&agent.messages);

    let (resp, _ms, _model) = chat_with_fallback(
        &agent.llm,
        &agent.config.model,
        &agent.config.fallback_models,
        msgs,
        vec![],
        None,
    )
    .await?;

    let model_summary = resp.message.content.clone().unwrap_or_default();
    if model_summary.trim().is_empty() {
        anyhow::bail!("compaction produced an empty summary");
    }
    let mut summary = format!("{}{}", model_summary.trim_end(), files.render());

    let todos = agent.ctx.todos.lock().unwrap().clone();
    if let Some(note) = crate::guard::todo_reminder(&todos) {
        summary.push_str("\n\n");
        summary.push_str(&note);
        agent.model_calls_since_todo = 0;
    }

    agent.usage.input_tokens += resp.usage.prompt_tokens;
    agent.usage.output_tokens += resp.usage.completion_tokens;

    let tail = plan::safe_tail(&agent.messages, agent.config.keep_after_compact);
    let mut new_msgs = Vec::with_capacity(tail.len() + 1);
    new_msgs.push(ChatMessage::user(format!("{SUMMARY_PREFIX}\n{summary}")));
    new_msgs.extend(tail);
    agent.messages = new_msgs;

    agent.last_prompt_tokens = 0;

    agent.prune_turn_starts = vec![0];

    agent.sink.emit(AgentEvent::CompactBoundary {
        trigger: trigger.to_string(),
        pre_tokens,
    });

    let post = agent
        .hooks
        .run(
            HookEvent::PostCompact,
            trigger,
            json!({ "trigger": trigger, "pre_tokens": pre_tokens, "summary": summary }),
        )
        .await;
    if let Some(text) = post.system_message {
        agent.sink.emit(AgentEvent::SystemMessage { event: "PostCompact".into(), text });
    }
    Ok(())
}

fn truncate_chars(s: &str, max: usize) -> String {
    let total = s.chars().count();
    if total <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max).collect();
    format!("{kept}\n[… {} more characters truncated]", total - max)
}

pub fn serialize_conversation(msgs: &[ChatMessage]) -> String {
    let mut out: Vec<String> = Vec::new();
    for m in msgs {
        let text = m.content.as_deref().unwrap_or("").trim();
        match m.role.as_str() {
            "user" if !text.is_empty() => out.push(format!("[User]: {text}")),
            "assistant" => {
                if !text.is_empty() {
                    out.push(format!("[Assistant]: {text}"));
                }
                if let Some(calls) = &m.tool_calls {
                    let list: Vec<String> = calls
                        .iter()
                        .map(|c| format!("{}({})", c.function.name, c.function.arguments.trim()))
                        .collect();
                    if !list.is_empty() {
                        out.push(format!("[Assistant tool calls]: {}", list.join("; ")));
                    }
                }
            }
            "tool" => out.push(format!("[Tool result]: {}", truncate_chars(text, TOOL_RESULT_CHARS))),
            _ => {}
        }
    }
    out.join("\n")
}

#[derive(Debug, Default, PartialEq)]
pub struct FileOps {
    pub read: std::collections::BTreeSet<String>,
    pub modified: std::collections::BTreeSet<String>,
}

impl FileOps {

    pub fn collect(msgs: &[ChatMessage]) -> Self {
        let mut ops = FileOps::default();
        for m in msgs {
            let text = m.content.as_deref().unwrap_or("");
            if m.role == "user" && text.starts_with(SUMMARY_PREFIX) {
                ops.read.extend(block_lines(text, "read-files"));
                ops.modified.extend(block_lines(text, "modified-files"));
            }
            for c in m.tool_calls.iter().flatten() {
                let Ok(args) = serde_json::from_str::<serde_json::Value>(&c.function.arguments) else {
                    continue;
                };
                let Some(fp) = args.get("file_path").and_then(|v| v.as_str()) else { continue };
                match c.function.name.as_str() {
                    "Read" => {
                        ops.read.insert(fp.to_string());
                    }
                    "Write" | "Edit" | "MultiEdit" => {
                        ops.modified.insert(fp.to_string());
                    }
                    _ => {}
                }
            }
        }
        let modified = ops.modified.clone();
        ops.read.retain(|p| !modified.contains(p));
        ops
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for (tag, set) in [("read-files", &self.read), ("modified-files", &self.modified)] {
            if !set.is_empty() {
                let body: Vec<&str> = set.iter().map(String::as_str).collect();
                out.push_str(&format!("\n\n<{tag}>\n{}\n</{tag}>", body.join("\n")));
            }
        }
        out
    }
}

fn block_lines(text: &str, tag: &str) -> Vec<String> {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let Some(start) = text.rfind(&open) else { return vec![] };
    let rest = &text[start + open.len()..];
    let Some(end) = rest.find(&close) else { return vec![] };
    rest[..end].lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: format!("id-{name}"),
            call_type: "function".into(),
            function: FunctionCall { name: name.into(), arguments: args.into() },
        }
    }

    #[test]
    fn serialize_conversation_flattens_roles_and_caps_tool_results() {
        let big = "x".repeat(TOOL_RESULT_CHARS + 50);
        let msgs = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("fix the bug"),
            ChatMessage::assistant(Some("looking".into()), Some(vec![call("Read", r#"{"file_path":"a.rs"}"#)])),
            ChatMessage::tool("id-Read", big),
            ChatMessage::assistant(Some("done".into()), None),
        ];
        let t = serialize_conversation(&msgs);
        assert!(!t.contains("sys"), "system prompt is not part of the transcript");
        assert!(t.starts_with("[User]: fix the bug\n[Assistant]: looking\n"));
        assert!(t.contains(r#"[Assistant tool calls]: Read({"file_path":"a.rs"})"#));
        assert!(t.contains("[… 50 more characters truncated]"));
        assert!(t.ends_with("[Assistant]: done"));
        assert!(t.len() < TOOL_RESULT_CHARS + 300);
    }

    #[test]
    fn truncation_is_char_safe() {
        let s = "é".repeat(5);
        assert_eq!(truncate_chars(&s, 3), "ééé\n[… 2 more characters truncated]");
        assert_eq!(truncate_chars("ab", 3), "ab");
    }

    #[test]
    fn file_ops_collect_and_carry_over_previous_summary() {
        let prev = format!(
            "{SUMMARY_PREFIX}\nold\n\n<read-files>\nold.rs\nb.rs\n</read-files>\n\n<modified-files>\nlib.rs\n</modified-files>"
        );
        let msgs = vec![
            ChatMessage::user(prev),
            ChatMessage::assistant(None, Some(vec![
                call("Read", r#"{"file_path":"a.rs"}"#),
                call("Edit", r#"{"file_path":"b.rs","old_string":"x","new_string":"y"}"#),
                call("Bash", r#"{"command":"ls"}"#),
                call("Write", "not json"),
            ])),
        ];
        let ops = FileOps::collect(&msgs);
        let read: Vec<&str> = ops.read.iter().map(String::as_str).collect();
        let modified: Vec<&str> = ops.modified.iter().map(String::as_str).collect();
        assert_eq!(read, vec!["a.rs", "old.rs"], "b.rs was modified, so it is not listed as read");
        assert_eq!(modified, vec!["b.rs", "lib.rs"]);
        assert_eq!(
            ops.render(),
            "\n\n<read-files>\na.rs\nold.rs\n</read-files>\n\n<modified-files>\nb.rs\nlib.rs\n</modified-files>"
        );
        assert_eq!(FileOps::default().render(), "");
    }

    #[test]
    fn focus_text_is_included_only_when_present() {
        let plain = summarization_prompt(None);
        assert!(plain.contains("Summarize this coding session"));
        assert!(!plain.contains("Extra focus"));
        let focused = summarization_prompt(Some("the parser bug"));
        assert!(focused.contains("Extra focus for this summary: the parser bug"));

        assert_eq!(summarization_prompt(Some("")), plain);
    }
}
