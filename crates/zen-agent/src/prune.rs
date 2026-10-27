use std::collections::HashMap;

use serde_json::Value;
use zen_core::ChatMessage;

pub const DEFAULT_PRUNE_AFTER: usize = 5;

pub const DEFAULT_MIN_PRUNE_TOKENS: u64 = 2_000;

pub const DEFAULT_MAX_RESULT_TOKENS: u64 = 8_000;

pub const RECENT_TURN_FLOOR: usize = 2;

pub const UNIDENTIFIED_MULTIPLIER: u64 = 2;

pub const PRUNE_MARKER: &str = "[zen: pruned";

pub const TRUNCATE_MARKER: &str = "[zen: truncated";

#[derive(Debug, Clone)]
pub struct PruneConfig {

    pub enabled: bool,

    pub prune_after: usize,

    pub min_prune_tokens: u64,

    pub max_result_tokens: u64,
}

impl Default for PruneConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            prune_after: DEFAULT_PRUNE_AFTER,
            min_prune_tokens: DEFAULT_MIN_PRUNE_TOKENS,
            max_result_tokens: DEFAULT_MAX_RESULT_TOKENS,
        }
    }
}

impl PruneConfig {

    pub fn aggressive() -> Self {
        Self {
            enabled: true,
            prune_after: 3,
            min_prune_tokens: 1_000,
            max_result_tokens: 4_000,
        }
    }

    pub fn min_age(&self) -> usize {
        self.prune_after.max(RECENT_TURN_FLOOR)
    }
}

#[derive(Debug, Clone, Default)]
pub struct PruneStats {

    pub pruned: usize,

    pub pruned_tokens: u64,

    pub capped: usize,

    pub capped_tokens: u64,
}

impl PruneStats {

    pub fn total_saved(&self) -> u64 {
        self.pruned_tokens + self.capped_tokens
    }
}

pub fn est_tokens(s: &str) -> u64 {
    (s.len() as u64).div_ceil(4)
}

pub fn extract_identifier(tool_name: &str, arguments: &str) -> Option<String> {
    let _ = tool_name;
    let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
    ["path", "file", "file_path", "pattern", "command", "query", "url"]
        .iter()
        .find_map(|k| args.get(k).and_then(|v| v.as_str()))
        .map(|s| s.to_string())
}

pub fn tool_call_label(tool_name: &str, arguments: &str) -> String {
    match extract_identifier(tool_name, arguments) {
        Some(id) => format!("{} {}", tool_name, truncate_chars(&id, 60)),
        None => tool_name.to_string(),
    }
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let end = s
        .char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    format!("{}…", &s[..end])
}

fn first_line(s: &str, max_chars: usize) -> String {
    truncate_chars(s.lines().next().unwrap_or("").trim(), max_chars)
}

pub fn summarize_pruned(label: &str, identifier: Option<&str>, tokens: u64, age: usize) -> String {
    let id_note = match identifier {
        Some(id) => format!("identifier '{}' not referenced by later messages; ", id),
        None => "no identifiable target and no later references; ".to_string(),
    };
    format!(
        "{} {}-token tool result ({}) - {} turns old; {}first line preserved below. Full output in session journal; re-read with `zen context` if needed.]",
        PRUNE_MARKER, tokens, label, age, id_note,
    )
}

pub fn cap_content(content: &str, label: &str, max_tokens: u64) -> (String, u64) {
    let tokens = est_tokens(content);
    if tokens <= max_tokens || max_tokens == 0 {
        return (content.to_string(), 0);
    }

    let keep_chars = (max_tokens as usize) * 4;
    let mut end = keep_chars.min(content.len());
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    let kept = content[..end].trim_end().to_string();
    let capped = format!(
        "{} {} → {} tokens ({}); re-read with a narrower query or path#Lx-Ly]\n{}",
        TRUNCATE_MARKER, tokens, max_tokens, label, kept
    );
    {
        let saved = tokens.saturating_sub(est_tokens(&capped));
        (capped, saved)
    }
}

fn tool_call_args(messages: &[ChatMessage]) -> HashMap<String, (String, String)> {
    let mut out = HashMap::new();
    for m in messages {
        if let Some(calls) = &m.tool_calls {
            for c in calls {
                out.insert(
                    c.id.clone(),
                    (c.function.name.clone(), c.function.arguments.clone()),
                );
            }
        }
    }
    out
}

pub(crate) fn turn_of(turn_starts: &[usize], index: usize) -> usize {
    turn_starts.partition_point(|&s| s <= index).saturating_sub(1)
}

fn should_prune(
    tokens: u64,
    age: usize,
    identifier: Option<&str>,
    later_text: &str,
    protected: &str,
    cfg: &PruneConfig,
) -> Option<String> {
    if age < cfg.min_age() {
        return None;
    }
    if tokens < cfg.min_prune_tokens {
        return None;
    }
    match identifier {
        Some(id) => {
            if later_text.contains(id) {
                return None;
            }
            if !protected.is_empty() && protected.contains(id) {
                return None;
            }
            Some(format!(
                "{} tokens and '{}' is not referenced by any later message",
                tokens, id
            ))
        }
        None => {

            if tokens < cfg.min_prune_tokens * UNIDENTIFIED_MULTIPLIER {
                return None;
            }
            Some(format!(
                "{} tokens with no identifiable target and no later references",
                tokens
            ))
        }
    }
}

pub fn prune_messages(
    messages: &mut [ChatMessage],
    turn_starts: &[usize],
    current_turn: usize,
    cfg: &PruneConfig,
    protected: &str,
) -> PruneStats {
    let mut stats = PruneStats::default();
    if !cfg.enabled || messages.is_empty() {
        return stats;
    }
    let args_by_id = tool_call_args(messages);

    let mut candidates: Vec<(usize, String, Option<String>, u64, usize)> = Vec::new();
    for (i, m) in messages.iter().enumerate() {
        if m.role != "tool" {
            continue;
        }
        let content = m.content.as_deref().unwrap_or("");
        let tokens = est_tokens(content);
        let age = current_turn.saturating_sub(turn_of(turn_starts, i));
        let (label, identifier) = match m
            .tool_call_id
            .as_deref()
            .and_then(|id| args_by_id.get(id))
        {
            Some((name, args)) => (
                tool_call_label(name, args),
                extract_identifier(name, args),
            ),
            None => ("tool_result".to_string(), None),
        };
        candidates.push((i, label, identifier, tokens, age));
    }

    for (i, label, identifier, tokens, age) in candidates {

        let later_text: String = messages
            .iter()
            .skip(i + 1)
            .filter_map(|m| m.content.as_deref())
            .collect::<Vec<_>>()
            .join("\n");
        if should_prune(
            tokens,
            age,
            identifier.as_deref(),
            &later_text,
            protected,
            cfg,
        )
        .is_some()
        {
            let original = messages[i].content.clone().unwrap_or_default();
            let summary = format!(
                "{}\nOriginal first line: {}",
                summarize_pruned(&label, identifier.as_deref(), tokens, age),
                first_line(&original, 200)
            );
            messages[i].content = Some(summary);
            stats.pruned += 1;
            stats.pruned_tokens += tokens;
        }
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use zen_core::ChatMessage;

    fn cfg() -> PruneConfig {
        PruneConfig::default()
    }

    fn tool_msg(id: &str, content: &str) -> ChatMessage {
        ChatMessage::tool(id, content)
    }

    fn assistant_with_call(id: &str, name: &str, args: &str) -> ChatMessage {
        ChatMessage::assistant(
            None,
            Some(vec![zen_core::ToolCall {
                id: id.to_string(),
                call_type: "function".to_string(),
                function: zen_core::FunctionCall {
                    name: name.to_string(),
                    arguments: args.to_string(),
                },
            }]),
        )
    }

    #[test]
    fn est_tokens_divides_by_four() {
        assert_eq!(est_tokens(""), 0);
        assert_eq!(est_tokens("abcd"), 1);
        assert_eq!(est_tokens("abcde"), 2);
    }

    #[test]
    fn extract_identifier_common_shapes() {
        assert_eq!(
            extract_identifier("read", r#"{"path": "src/main.rs"}"#).as_deref(),
            Some("src/main.rs")
        );
        assert_eq!(
            extract_identifier("edit", r#"{"file_path": "a/b.rs"}"#).as_deref(),
            Some("a/b.rs")
        );
        assert_eq!(
            extract_identifier("bash", r#"{"command": "cargo test"}"#).as_deref(),
            Some("cargo test")
        );
        assert_eq!(extract_identifier("bash", r#"{"foo": 1}"#), None);
        assert_eq!(extract_identifier("bash", "not json"), None);
    }

    #[test]
    fn should_prune_positive_case() {
        let reason = should_prune(5000, 6, Some("src/old.rs"), "unrelated text", "", &cfg());
        assert!(reason.is_some(), "large, old, unreferenced → prunable");
    }

    #[test]
    fn should_prune_negative_referenced_later() {
        let reason = should_prune(
            5000,
            6,
            Some("src/old.rs"),
            "as seen in src/old.rs the function…",
            "",
            &cfg(),
        );
        assert!(reason.is_none(), "referenced later → not prunable");
    }

    #[test]
    fn should_prune_negative_protected_identifier() {

        let reason = should_prune(
            5000,
            6,
            Some("src/auth.rs"),
            "unrelated",
            "Plan: refactor src/auth.rs login flow",
            &cfg(),
        );
        assert!(reason.is_none(), "identifier in plan → not prunable");
    }

    #[test]
    fn should_prune_negative_too_recent() {
        let reason = should_prune(9000, 1, Some("src/old.rs"), "unrelated", "", &cfg());
        assert!(reason.is_none(), "last-2-turns floor → not prunable");
    }

    #[test]
    fn should_prune_negative_too_small() {
        let reason = should_prune(500, 10, Some("src/old.rs"), "unrelated", "", &cfg());
        assert!(reason.is_none(), "under min_prune_tokens → not prunable");
    }

    #[test]
    fn should_prune_unidentified_needs_double_size() {
        let c = cfg();
        assert!(should_prune(3000, 10, None, "unrelated", "", &c).is_none());
        assert!(should_prune(5000, 10, None, "unrelated", "", &c).is_some());
    }

    #[test]
    fn prune_after_config_is_honored() {
        let mut c = cfg();
        c.prune_after = 10;
        assert!(should_prune(5000, 6, Some("x.rs"), "unrelated", "", &c).is_none());
        assert!(should_prune(5000, 11, Some("x.rs"), "unrelated", "", &c).is_some());
    }

    #[test]
    fn cap_content_truncates_with_marker() {
        let content = "x".repeat(40_000);
        let (capped, saved) = cap_content(&content, "read big.rs", 8_000);
        assert!(capped.starts_with(TRUNCATE_MARKER));
        assert!(capped.contains("re-read with a narrower query"));
        assert!(saved > 0, "saved tokens must be positive");
        assert!(est_tokens(&capped) <= 8_000 + 50, "capped content near the cap");
    }

    #[test]
    fn cap_content_passthrough_under_cap() {
        let content = "small result";
        let (out, saved) = cap_content(content, "read small.rs", 8_000);
        assert_eq!(out, content);
        assert_eq!(saved, 0);
    }

    fn fixture_15_turns() -> (Vec<ChatMessage>, Vec<usize>) {
        let mut messages = vec![ChatMessage::user("do the thing")];
        let mut turn_starts = vec![0usize];
        for turn in 1..=15usize {
            turn_starts.push(messages.len());
            let call_id = format!("call-{}", turn);
            messages.push(assistant_with_call(
                &call_id,
                "read",
                &format!(r#"{{"path": "src/file{}.rs"}}"#, turn),
            ));

            let body = format!("// file{} contents\n", turn) + &"x".repeat(20_000);
            messages.push(tool_msg(&call_id, &body));

            if turn > 1 {
                messages.push(ChatMessage::assistant(
                    Some(format!("done with step {}", turn)),
                    None,
                ));
            }
        }
        (messages, turn_starts)
    }

    #[test]
    fn prune_messages_kills_the_rebilling_multiplier() {
        let (mut messages, turn_starts) = fixture_15_turns();
        let current_turn = turn_starts.len() - 1;
        let before: u64 = messages
            .iter()
            .filter_map(|m| m.content.as_deref())
            .map(est_tokens)
            .sum();

        let stats = prune_messages(&mut messages, &turn_starts, current_turn, &cfg(), "");

        assert_eq!(stats.pruned, 10, "turns 1-10 pruned, got {:?}", stats);
        assert!(stats.pruned_tokens > 45_000);

        let after: u64 = messages
            .iter()
            .filter_map(|m| m.content.as_deref())
            .map(est_tokens)
            .sum();
        assert!(
            after < before / 2,
            "pruning must more than halve the transcript: {} → {}",
            before,
            after
        );

        let pruned_msg = &messages[2];
        let content = pruned_msg.content.as_deref().unwrap();
        assert!(content.starts_with(PRUNE_MARKER), "marker present");
        assert!(content.contains("src/file1.rs"), "identifier preserved");
        assert_eq!(pruned_msg.role, "tool");
        assert_eq!(pruned_msg.tool_call_id.as_deref(), Some("call-1"));
    }

    #[test]
    fn prune_messages_respects_protected_plan() {
        let (mut messages, turn_starts) = fixture_15_turns();
        let current_turn = turn_starts.len() - 1;

        let stats = prune_messages(
            &mut messages,
            &turn_starts,
            current_turn,
            &cfg(),
            "Plan: still need to update src/file3.rs",
        );
        assert_eq!(stats.pruned, 9, "all but the protected turn pruned");

        let kept = &messages[7];
        assert!(
            !kept.content.as_deref().unwrap().starts_with(PRUNE_MARKER),
            "protected identifier must not be pruned"
        );
    }

    #[test]
    fn prune_messages_disabled_is_noop() {
        let (mut messages, turn_starts) = fixture_15_turns();
        let mut c = cfg();
        c.enabled = false;
        let current_turn = turn_starts.len() - 1;
        let stats = prune_messages(&mut messages, &turn_starts, current_turn, &c, "");
        assert_eq!(stats.pruned, 0);
        assert_eq!(stats.pruned_tokens, 0);
    }

    #[test]
    fn turn_of_maps_indices_to_turns() {

        let ts = vec![0usize, 1, 3];
        assert_eq!(turn_of(&ts, 0), 0);
        assert_eq!(turn_of(&ts, 1), 1);
        assert_eq!(turn_of(&ts, 2), 1);
        assert_eq!(turn_of(&ts, 3), 2);
        assert_eq!(turn_of(&[], 5), 0);
    }

    #[test]
    fn aggressive_defaults_are_stricter() {
        let a = PruneConfig::aggressive();
        let d = PruneConfig::default();
        assert!(a.prune_after < d.prune_after);
        assert!(a.min_prune_tokens < d.min_prune_tokens);
        assert!(a.max_result_tokens < d.max_result_tokens);
        assert!(a.enabled && d.enabled);
    }
}
