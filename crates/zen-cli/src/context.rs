//! Context debugger: `zen context` — an observable, attributable, traceable view of
//! what the model actually saw.
//!
//! The session transcript (`~/.zen/projects/<encoded-cwd>/<session-id>.jsonl`) already
//! contains everything needed: `request` records (with `body_bytes` and delta-encoded
//! messages), `response` records (with the API's reported `usage.prompt_tokens`),
//! `tools` records (tool definitions by hash), and `summary` records (compaction history).
//!
//! Attribution strategy:
//! - The total comes from the API's reported `prompt_tokens` when available (most
//!   accurate); otherwise we fall back to a `chars / 4` estimate, and the output says so.
//! - Per-source breakdown replays the latest request's messages: system prompt, tool
//!   schemas (per-tool JSON size), then each history turn with tool results labeled by
//!   the tool call that produced them (e.g. `read src/main.rs`).
//! - A tool result is flagged PRUNABLE when it is large and its key identifier (file
//!   path) is not referenced by any later message — a heuristic, documented as such.

use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use zen_core::{ChatMessage, ToolDefinition};

/// chars/4 token estimate, consistent with the rest of the codebase.
fn est_tokens(s: &str) -> u64 {
    s.len() as u64 / 4
}

/// Tokens of one chat message (content + tool-call payloads).
fn message_tokens(m: &ChatMessage) -> u64 {
    let mut n = m.content.as_deref().map(est_tokens).unwrap_or(0);
    if let Some(calls) = &m.tool_calls {
        for c in calls {
            n += est_tokens(&c.function.name) + est_tokens(&c.function.arguments);
            n += 4; // per-call framing overhead
        }
    }
    if m.tool_call_id.is_some() {
        n += 4;
    }
    n
}

/// Tokens of one tool definition (serialized JSON size / 4).
fn tool_def_tokens(t: &ToolDefinition) -> u64 {
    serde_json::to_string(t).map(|s| est_tokens(&s)).unwrap_or(0)
}

/// Tool results at or above this size are candidates for pruning.
pub const PRUNE_CANDIDATE_TOKENS: u64 = 2000;

// ─── Parsed session data ─────────────────────────────────────────────────────

/// One fully-decoded request from the event log.
struct LoggedRequest {
    seq: u64,
    timestamp: String,
    model: String,
    tools_hash: Option<String>,
    body_bytes: u64,
    messages: Vec<ChatMessage>,
}

/// A compaction `summary` record from the transcript.
struct SummaryRecord {
    timestamp: String,
    text: String,
}

/// All parsed data from a session transcript.
struct ParsedSession {
    session_id: String,
    requests: Vec<LoggedRequest>,
    /// seq -> reported prompt_tokens from the matching response record.
    usage_by_seq: HashMap<u64, u64>,
    tools_by_hash: HashMap<String, Vec<ToolDefinition>>,
    summaries: Vec<SummaryRecord>,
}

/// Parse the transcript JSONL, delta-decoding request chains.
fn parse_transcript(path: &Path, session_id: &str) -> Result<ParsedSession> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow!("cannot read session transcript {}: {}", path.display(), e))?;
    let mut tools_by_hash: HashMap<String, Vec<ToolDefinition>> = HashMap::new();
    let mut chains: HashMap<u64, Vec<ChatMessage>> = HashMap::new();
    let mut requests: Vec<LoggedRequest> = vec![];
    let mut usage_by_seq: HashMap<u64, u64> = HashMap::new();
    let mut summaries: Vec<SummaryRecord> = vec![];

    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(line)
            .map_err(|e| anyhow!("{}:{}: invalid JSON: {}", path.display(), n + 1, e))?;
        match v.get("type").and_then(|t| t.as_str()) {
            Some("tools") => {
                let h = v["hash"].as_str().unwrap_or_default().to_string();
                if let Ok(defs) = serde_json::from_value::<Vec<ToolDefinition>>(v["tools"].clone()) {
                    tools_by_hash.insert(h, defs);
                }
            }
            Some("request") => {
                let seq = v["seq"].as_u64().unwrap_or(0);
                let mut messages: Vec<ChatMessage> = match v["base"].as_u64() {
                    Some(b) => {
                        let prev = chains.get(&b).cloned().unwrap_or_default();
                        let keep = v["keep"].as_u64().unwrap_or(0) as usize;
                        prev.into_iter().take(keep).collect()
                    }
                    None => vec![],
                };
                if let Ok(append) =
                    serde_json::from_value::<Vec<ChatMessage>>(v["append"].clone())
                {
                    messages.extend(append);
                }
                chains.insert(seq, messages.clone());
                requests.push(LoggedRequest {
                    seq,
                    timestamp: v["timestamp"].as_str().unwrap_or("").to_string(),
                    model: v["model"].as_str().unwrap_or("").to_string(),
                    tools_hash: v["tools_hash"].as_str().map(|s| s.to_string()),
                    body_bytes: v["body_bytes"].as_u64().unwrap_or(0),
                    messages,
                });
            }
            Some("response") => {
                if let Some(seq) = v["seq"].as_u64() {
                    let pt = v["response"]["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
                    if pt > 0 {
                        usage_by_seq.insert(seq, pt);
                    }
                }
            }
            Some("summary") => {
                let text = match &v["summary"] {
                    Value::String(s) => s.clone(),
                    Value::Object(m) => m
                        .get("summary")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string(),
                    _ => String::new(),
                };
                summaries.push(SummaryRecord {
                    timestamp: v["timestamp"].as_str().unwrap_or("").to_string(),
                    text,
                });
            }
            _ => {}
        }
    }
    // Ensure requests are in seq order (transcript order should already be).
    requests.sort_by_key(|r| r.seq);
    Ok(ParsedSession { session_id: session_id.to_string(), requests, usage_by_seq, tools_by_hash, summaries })
}

// ─── Attribution ─────────────────────────────────────────────────────────────

/// Where the headline total came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TotalSource {
    /// The API reported prompt_tokens for the latest request.
    Reported,
    /// No usage record; summed chars/4 estimate.
    Estimated,
}

/// One attributed turn/message in the history.
#[derive(Debug, Clone, Serialize)]
pub struct TurnAttribution {
    pub index: usize,
    pub role: String,
    pub label: String,
    pub tokens: u64,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub prunable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prune_reason: Option<String>,
}

/// One compaction event: what was summarized away.
#[derive(Debug, Clone, Serialize)]
pub struct CompactionInfo {
    pub timestamp: String,
    /// Tokens in context just before compaction (reported when available).
    pub tokens_before: u64,
    pub tokens_before_source: TotalSource,
    /// Tokens of the stored summary that replaced the history.
    pub summary_tokens: u64,
    pub dropped_tokens: u64,
}

/// A top token consumer with an actionable suggestion.
#[derive(Debug, Clone, Serialize)]
pub struct ConsumerHint {
    pub label: String,
    pub tokens: u64,
    pub suggestion: String,
}

/// Full context-debugger analysis of a session.
#[derive(Debug, Clone, Serialize)]
pub struct ContextAnalysis {
    pub session_id: String,
    pub model: String,
    pub total_tokens: u64,
    pub total_source: TotalSource,
    /// Exact request body bytes when the event log recorded them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_body_bytes: Option<u64>,
    pub system_prompt_tokens: u64,
    pub tool_schema_tokens: u64,
    pub tool_schemas: Vec<(String, u64)>,
    pub history_tokens: u64,
    pub turns: Vec<TurnAttribution>,
    pub compactions: Vec<CompactionInfo>,
    pub top_consumers: Vec<ConsumerHint>,
}

/// Extract a human label for a tool call, e.g. `read src/main.rs`.
fn tool_call_label(name: &str, arguments: &str) -> String {
    // Pull out a likely file path / target from common argument shapes.
    let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
    let target = ["path", "file", "pattern", "command", "query", "url"]
        .iter()
        .find_map(|k| args.get(k).and_then(|v| v.as_str()))
        .map(|s| {
            if s.len() > 60 {
                format!("{}…", &s[..57])
            } else {
                s.to_string()
            }
        });
    match target {
        Some(t) => format!("{} {}", name, t),
        None => name.to_string(),
    }
}

/// Build a map tool_call_id -> label from assistant messages.
fn tool_labels(messages: &[ChatMessage]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for m in messages {
        if let Some(calls) = &m.tool_calls {
            for c in calls {
                // Key by the tool call id so `tool` messages can find their label.
                out.insert(
                    c.id.clone(),
                    tool_call_label(&c.function.name, &c.function.arguments),
                );
            }
        }
    }
    out
}

fn analyze(parsed: &ParsedSession) -> ContextAnalysis {
    let latest = parsed.requests.last();
    let model = latest.map(|r| r.model.clone()).unwrap_or_default();

    // Headline total: API-reported usage for the latest request when we have it.
    let (total_tokens, total_source) = match latest {
        Some(r) => match parsed.usage_by_seq.get(&r.seq) {
            Some(&pt) => (pt, TotalSource::Reported),
            None => (r.messages.iter().map(message_tokens).sum(), TotalSource::Estimated),
        },
        None => (0, TotalSource::Estimated),
    };
    let request_body_bytes = latest.and_then(|r| if r.body_bytes > 0 { Some(r.body_bytes) } else { None });

    let mut system_prompt_tokens = 0u64;
    let mut tool_schema_tokens = 0u64;
    let mut tool_schemas: Vec<(String, u64)> = vec![];
    let mut history_tokens = 0u64;
    let mut turns: Vec<TurnAttribution> = vec![];

    if let Some(req) = latest {
        let labels = tool_labels(&req.messages);
        // Tool schemas from the tools_hash.
        if let Some(h) = &req.tools_hash {
            if let Some(defs) = parsed.tools_by_hash.get(h) {
                for d in defs {
                    let t = tool_def_tokens(d);
                    tool_schemas.push((d.function.name.clone(), t));
                    tool_schema_tokens += t;
                }
                tool_schemas.sort_by_key(|a| std::cmp::Reverse(a.1));
            }
        }
        let mut history_start = 0;
        if let Some(first) = req.messages.first() {
            if first.role == "system" {
                system_prompt_tokens = message_tokens(first);
                history_start = 1;
            }
        }
        // Collect identifiers mentioned later, for the prunable heuristic.
        let later_text: Vec<String> = req.messages.iter().filter_map(|m| m.content.clone()).collect();

        for (i, m) in req.messages.iter().enumerate().skip(history_start) {
            let tokens = message_tokens(m);
            history_tokens += tokens;
            let (label, mut prunable, mut prune_reason) = if m.role == "tool" {
                let key = m.tool_call_id.clone().unwrap_or_default();
                let base = labels.get(&key).cloned().unwrap_or_else(|| "tool_result".to_string());
                let label = format!("tool_result ({})", base);
                // Heuristic: large result whose identifier never reappears afterwards.
                let mut flag = false;
                let mut reason = None;
                if tokens >= PRUNE_CANDIDATE_TOKENS {
                    let id = base.split_whitespace().nth(1).unwrap_or("");
                    if !id.is_empty() {
                        let mentioned_later = req.messages.iter().skip(i + 1).any(|lm| {
                            lm.content.as_deref().map(|c| c.contains(id)).unwrap_or(false)
                        });
                        if !mentioned_later {
                            flag = true;
                            reason = Some(format!(
                                "{} tokens and '{}' is not referenced by any later message",
                                tokens, id
                            ));
                        }
                    } else if tokens >= PRUNE_CANDIDATE_TOKENS * 2 {
                        flag = true;
                        reason = Some(format!(
                            "{} tokens with no identifiable target and no later references",
                            tokens
                        ));
                    }
                }
                (label, flag, reason)
            } else if m.tool_calls.as_ref().map(|c| !c.is_empty()).unwrap_or(false) {
                let names: Vec<String> = m.tool_calls.as_ref().unwrap().iter()
                    .map(|c| tool_call_label(&c.function.name, &c.function.arguments))
                    .collect();
                (format!("assistant (calls: {})", names.join(", ")), false, None)
            } else {
                (m.role.clone(), false, None)
            };
            // Don't flag the most recent two turns: they are still "live".
            if prunable && i + 2 >= req.messages.len() {
                prunable = false;
                prune_reason = None;
            }
            let _ = later_text; // (kept for future cross-turn reference checks)
            turns.push(TurnAttribution { index: i, role: m.role.clone(), label, tokens, prunable, prune_reason });
        }
    }

    // Compaction history: match each summary to the last request before it.
    let mut compactions: Vec<CompactionInfo> = vec![];
    for s in &parsed.summaries {
        let before = parsed.requests.iter().rev().find(|r| r.timestamp <= s.timestamp);
        let (tokens_before, src) = match before {
            Some(r) => match parsed.usage_by_seq.get(&r.seq) {
                Some(&pt) => (pt, TotalSource::Reported),
                None => (r.messages.iter().map(message_tokens).sum(), TotalSource::Estimated),
            },
            None => (0, TotalSource::Estimated),
        };
        let summary_tokens = est_tokens(&s.text);
        compactions.push(CompactionInfo {
            timestamp: s.timestamp.clone(),
            tokens_before,
            tokens_before_source: src,
            summary_tokens,
            dropped_tokens: tokens_before.saturating_sub(summary_tokens),
        });
    }

    // Top consumers + suggestions.
    let mut consumers: Vec<ConsumerHint> = vec![];
    if system_prompt_tokens > 0 {
        consumers.push(ConsumerHint {
            label: "system prompt".to_string(),
            tokens: system_prompt_tokens,
            suggestion: "Check for verbose custom instructions or appended context that could be trimmed.".to_string(),
        });
    }
    if tool_schema_tokens > 0 {
        consumers.push(ConsumerHint {
            label: format!("tool schemas ({} tools)", tool_schemas.len()),
            tokens: tool_schema_tokens,
            suggestion: "Use --tools to limit the tool set to what this task needs; each unused tool is re-billed every turn.".to_string(),
        });
    }
    for t in &turns {
        if t.prunable {
            consumers.push(ConsumerHint {
                label: t.label.clone(),
                tokens: t.tokens,
                suggestion: "Large result no longer referenced — a future prune pass could summarize or drop it.".to_string(),
            });
        } else if t.tokens >= PRUNE_CANDIDATE_TOKENS * 2 {
            consumers.push(ConsumerHint {
                label: t.label.clone(),
                tokens: t.tokens,
                suggestion: "One of the largest single turns; check whether all of it is still needed.".to_string(),
            });
        }
    }
    consumers.sort_by_key(|a| std::cmp::Reverse(a.tokens));
    consumers.truncate(3);

    ContextAnalysis {
        session_id: parsed.session_id.clone(),
        model,
        total_tokens,
        total_source,
        request_body_bytes,
        system_prompt_tokens,
        tool_schema_tokens,
        tool_schemas,
        history_tokens,
        turns,
        compactions,
        top_consumers: consumers,
    }
}

// ─── Text rendering ──────────────────────────────────────────────────────────

fn pct(part: u64, total: u64) -> f64 {
    if total == 0 { 0.0 } else { part as f64 * 100.0 / total as f64 }
}

/// Render the analysis as the text tree from the spec.
pub fn render_text(a: &ContextAnalysis) -> String {
    let mut out = String::new();
    let src = match a.total_source {
        TotalSource::Reported => "reported by API",
        TotalSource::Estimated => "estimated (chars/4)",
    };
    out.push_str(&format!(
        "Context: {} tokens ({}) — session {}\n",
        a.total_tokens, src, a.session_id
    ));
    if !a.model.is_empty() {
        out.push_str(&format!("Model: {}\n", a.model));
    }
    if let Some(b) = a.request_body_bytes {
        out.push_str(&format!("Request body: {} bytes on the wire\n", b));
    }
    let t = a.total_tokens;
    out.push_str(&format!(
        "├── system prompt: {} ({:.1}%)\n",
        a.system_prompt_tokens, pct(a.system_prompt_tokens, t)
    ));
    out.push_str(&format!(
        "├── tool schemas ({}): {} ({:.1}%)\n",
        a.tool_schemas.len(), a.tool_schema_tokens, pct(a.tool_schema_tokens, t)
    ));
    for (i, (name, tok)) in a.tool_schemas.iter().take(5).enumerate() {
        let last = i + 1 == a.tool_schemas.len().min(5);
        out.push_str(&format!(
            "│   {} {}: {}\n",
            if last { "└──" } else { "├──" }, name, tok
        ));
    }
    if a.tool_schemas.len() > 5 {
        out.push_str(&format!("│   └── … ({} more)\n", a.tool_schemas.len() - 5));
    }
    out.push_str(&format!(
        "└── history ({} turns): {} ({:.1}%)\n",
        a.turns.len(), a.history_tokens, pct(a.history_tokens, t)
    ));
    for (i, turn) in a.turns.iter().enumerate() {
        let last = i + 1 == a.turns.len();
        let flag = if turn.prunable { " [PRUNABLE?]" } else { "" };
        out.push_str(&format!(
            "    {} turn {} {} ({}): {}{}\n",
            if last { "└──" } else { "├──" },
            turn.index, turn.role, turn.label, turn.tokens, flag
        ));
        if let Some(reason) = &turn.prune_reason {
            out.push_str(&format!(
                "    {}     → {}\n",
                if last { " " } else { "│" }, reason
            ));
        }
    }

    if !a.compactions.is_empty() {
        out.push_str(&format!("\nCompactions ({}):\n", a.compactions.len()));
        for (i, c) in a.compactions.iter().enumerate() {
            out.push_str(&format!(
                "  {}. {} — {} tokens → {} summary tokens ({} dropped)\n",
                i + 1, c.timestamp, c.tokens_before, c.summary_tokens, c.dropped_tokens
            ));
        }
    }

    if !a.top_consumers.is_empty() {
        out.push_str("\nTop token consumers — where did your tokens go?\n");
        for (i, c) in a.top_consumers.iter().enumerate() {
            out.push_str(&format!(
                "  {}. {} — {} tokens\n     → {}\n",
                i + 1, c.label, c.tokens, c.suggestion
            ));
        }
    } else {
        out.push_str("\nNo dominant token consumers found.\n");
    }
    out
}

// ─── Entry point ─────────────────────────────────────────────────────────────

/// Resolve the transcript path: explicit --session id/path, else latest for cwd.
pub fn resolve_session_path(
    home: Option<&Path>,
    cwd: &Path,
    session: Option<&str>,
) -> Result<PathBuf> {
    let home = home
        .map(|p| p.to_path_buf())
        .or_else(dirs::home_dir)
        .ok_or_else(|| anyhow!("cannot determine home directory"))?;
    if let Some(s) = session {
        if let Some(p) = crate::session::find_session_path(&home, s) {
            return Ok(p);
        }
        return Err(anyhow!("no session found for '{}'", s));
    }
    let encoded = crate::session::encode_cwd(cwd);
    crate::session::latest_session_for_cwd(&home, &encoded)
        .ok_or_else(|| anyhow!("no sessions found for this directory"))
}

/// Run `zen context`: analyze a session transcript and print the attribution.
pub fn run_context(
    session: Option<&str>,
    output_format: &crate::cli::OutputFormat,
    cwd: &Path,
    home: Option<&Path>,
) -> Result<()> {
    let path = resolve_session_path(home, cwd, session)?;
    let session_id = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parsed = parse_transcript(&path, &session_id)?;
    if parsed.requests.is_empty() {
        println!("No model requests recorded in this session yet.");
        return Ok(());
    }
    let analysis = analyze(&parsed);
    match output_format {
        crate::cli::OutputFormat::Text => {
            print!("{}", render_text(&analysis));
        }
        crate::cli::OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&analysis)?);
        }
        crate::cli::OutputFormat::StreamJson => {
            let mut v = serde_json::to_value(&analysis)?;
            if let Value::Object(ref mut m) = v {
                m.insert("type".to_string(), Value::String("context".to_string()));
            }
            println!("{}", serde_json::to_string(&v)?);
        }
    }
    Ok(())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_fixture(dir: &Path, name: &str, lines: &[Value]) -> PathBuf {
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        for l in lines {
            writeln!(f, "{}", serde_json::to_string(l).unwrap()).unwrap();
        }
        p
    }

    fn msg(role: &str, content: &str) -> Value {
        serde_json::json!({"role": role, "content": content})
    }

    /// Build a small session: tools + 2 requests (delta-encoded) + 1 response + 1 summary.
    fn fixture(path: &Path) {
        let tools = serde_json::json!([
            {"type": "function", "function": {"name": "read", "description": "read a file", "parameters": {"type": "object"}}},
            {"type": "function", "function": {"name": "bash", "description": "run a shell command", "parameters": {"type": "object"}}}
        ]);
        let th = "abc123";
        let sys = "x".repeat(400); // 100 tokens
        let r1 = serde_json::json!({
            "type": "request", "v": 1, "seq": 1, "timestamp": "2026-10-17T10:00:00Z",
            "model": "test-model", "tools_hash": th, "base": null, "keep": 0,
            "append": [msg("system", &sys), msg("user", "hello")],
            "body_hash": "h1", "body_bytes": 5000
        });
        // Second request keeps both messages and appends a big tool result.
        let big = "y".repeat(12000); // 3000 tokens
        let r2 = serde_json::json!({
            "type": "request", "v": 1, "seq": 2, "timestamp": "2026-10-17T10:01:00Z",
            "model": "test-model", "tools_hash": th, "base": 1, "keep": 2,
            "append": [
                {"role": "assistant", "content": "reading", "tool_calls": [
                    {"id": "c1", "type": "function", "function": {"name": "read", "arguments": "{\"path\": \"src/main.rs\"}"}}
                ]},
                {"role": "tool", "tool_call_id": "c1", "content": big}
            ],
            "body_hash": "h2", "body_bytes": 20000
        });
        let resp = serde_json::json!({
            "type": "response", "v": 1, "seq": 2, "timestamp": "2026-10-17T10:01:05Z",
            "response": {"message": {"role": "assistant"}, "usage": {"prompt_tokens": 4500, "completion_tokens": 50, "total_tokens": 4550}, "finish_reason": "stop"}
        });
        let summary = serde_json::json!({
            "type": "summary", "uuid": "u1", "sessionId": "s1",
            "timestamp": "2026-10-17T10:02:00Z", "cwd": "/tmp",
            "summary": "z".repeat(400)
        });
        write_fixture(
            path.parent().unwrap(),
            path.file_name().unwrap().to_str().unwrap(),
            &[
                serde_json::json!({"type": "tools", "v": 1, "hash": th, "tools": tools}),
                r1, r2, resp, summary,
            ],
        );
    }

    #[test]
    fn parses_delta_encoded_requests() {
        let d = std::env::temp_dir().join(format!("ctx_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("s.jsonl");
        fixture(&p);
        let parsed = parse_transcript(&p, "s").unwrap();
        assert_eq!(parsed.requests.len(), 2);
        // Second request rebuilt via base/keep: 2 + 2 = 4 messages.
        assert_eq!(parsed.requests[1].messages.len(), 4);
        assert_eq!(parsed.requests[1].body_bytes, 20000);
        assert_eq!(parsed.usage_by_seq.get(&2), Some(&4500));
        assert_eq!(parsed.summaries.len(), 1);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn prefers_reported_total_over_estimate() {
        let d = std::env::temp_dir().join(format!("ctx_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("s.jsonl");
        fixture(&p);
        let parsed = parse_transcript(&p, "s").unwrap();
        let a = analyze(&parsed);
        assert_eq!(a.total_tokens, 4500);
        assert_eq!(a.total_source, TotalSource::Reported);
        assert_eq!(a.model, "test-model");
        assert_eq!(a.request_body_bytes, Some(20000));
        // system prompt was 400 chars = 100 tokens.
        assert_eq!(a.system_prompt_tokens, 100);
        // 2 tool schemas attributed.
        assert_eq!(a.tool_schemas.len(), 2);
        assert!(a.tool_schema_tokens > 0);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn flags_large_unreferenced_tool_result() {
        let d = std::env::temp_dir().join(format!("ctx_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("s.jsonl");
        fixture(&p);
        // The 3000-token tool result is the last turn -> not flagged (still live).
        // Add a follow-up request so it becomes old.
        let r3 = serde_json::json!({
            "type": "request", "v": 1, "seq": 3, "timestamp": "2026-10-17T10:03:00Z",
            "model": "test-model", "tools_hash": "abc123", "base": 2, "keep": 4,
            "append": [msg("user", "thanks"), msg("assistant", "done")],
            "body_hash": "h3", "body_bytes": 21000
        });
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        writeln!(f, "{}", serde_json::to_string(&r3).unwrap()).unwrap();
        let parsed = parse_transcript(&p, "s").unwrap();
        let a = analyze(&parsed);
        let prunable: Vec<_> = a.turns.iter().filter(|t| t.prunable).collect();
        assert_eq!(prunable.len(), 1);
        assert!(prunable[0].label.contains("read src/main.rs"));
        assert!(prunable[0].prune_reason.as_ref().unwrap().contains("not referenced"));
        // And it shows up in top consumers.
        assert!(a.top_consumers.iter().any(|c| c.label.contains("read src/main.rs")));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn compaction_history_shows_dropped_tokens() {
        let d = std::env::temp_dir().join(format!("ctx_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("s.jsonl");
        fixture(&p);
        let parsed = parse_transcript(&p, "s").unwrap();
        let a = analyze(&parsed);
        assert_eq!(a.compactions.len(), 1);
        let c = &a.compactions[0];
        // Last request before the summary is seq 2 with reported 4500 tokens.
        assert_eq!(c.tokens_before, 4500);
        assert_eq!(c.summary_tokens, 100); // 400 chars
        assert_eq!(c.dropped_tokens, 4400);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn text_tree_renders_expected_sections() {
        let d = std::env::temp_dir().join(format!("ctx_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("s.jsonl");
        fixture(&p);
        let parsed = parse_transcript(&p, "s").unwrap();
        let a = analyze(&parsed);
        let text = render_text(&a);
        assert!(text.contains("Context: 4500 tokens (reported by API)"));
        assert!(text.contains("system prompt: 100"));
        assert!(text.contains("tool schemas (2)"));
        assert!(text.contains("history (3 turns)"));
        assert!(text.contains("Compactions (1)"));
        assert!(text.contains("Top token consumers"));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn json_shape_has_type_field_for_stream_json() {
        let d = std::env::temp_dir().join(format!("ctx_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("s.jsonl");
        fixture(&p);
        let parsed = parse_transcript(&p, "s").unwrap();
        let a = analyze(&parsed);
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["total_tokens"], 4500);
        assert_eq!(v["total_source"], "reported");
        assert!(v["turns"].is_array());
        assert!(v["compactions"].is_array());
        assert!(v["top_consumers"].is_array());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn empty_session_reports_gracefully() {
        let d = std::env::temp_dir().join(format!("ctx_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("s.jsonl");
        std::fs::write(&p, "").unwrap();
        let parsed = parse_transcript(&p, "s").unwrap();
        assert!(parsed.requests.is_empty());
        let a = analyze(&parsed);
        assert_eq!(a.total_tokens, 0);
        assert!(a.top_consumers.is_empty());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn tool_call_label_extracts_path() {
        assert_eq!(
            tool_call_label("read", "{\"path\": \"src/main.rs\"}"),
            "read src/main.rs"
        );
        assert_eq!(
            tool_call_label("bash", "{\"command\": \"cargo test --workspace\"}"),
            "bash cargo test --workspace"
        );
        assert_eq!(tool_call_label("read", "not json"), "read");
    }
}
