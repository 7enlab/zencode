











use anyhow::{anyhow, Context, Result};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};



pub const EXPORT_FORMAT_VERSION: u32 = 1;


pub fn exporter_version() -> String {
    format!("zen {}", env!("CARGO_PKG_VERSION"))
}


#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ExportFormat {

    Json,

    Markdown,

    Html,
}




#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExportedMessage {

    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,



    pub payload: Value,
}


#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ExportedUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}



#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SessionExport {
    pub format_version: u32,
    pub exported_by: String,
    pub exported_at: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub usage: ExportedUsage,


    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,

    pub redacted: bool,
    pub messages: Vec<ExportedMessage>,
}




#[derive(Debug)]
pub struct LoadedSession {
    pub session_id: String,
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub usage: ExportedUsage,
    pub messages: Vec<ExportedMessage>,
}

fn record_str(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(String::from)
}







pub fn load_session_for_export(path: &Path) -> Result<LoadedSession> {
    let session_id = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_string());
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read session file {}", path.display()))?;

    let mut out = LoadedSession {
        session_id,
        cwd: None,
        model: None,
        usage: ExportedUsage::default(),
        messages: vec![],
    };

    for (n, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(line).with_context(|| {
            format!("{}:{}: invalid transcript line", path.display(), n + 1)
        })?;
        let kind = record_str(&v, "type").unwrap_or_default();
        let timestamp = record_str(&v, "timestamp");
        let uuid = record_str(&v, "uuid");
        if out.cwd.is_none() {
            out.cwd = record_str(&v, "cwd");
        }
        match kind.as_str() {
            "user" | "assistant" | "tool" => {
                if let Some(msg) = v.get("message").cloned() {
                    out.messages.push(ExportedMessage {
                        kind,
                        timestamp,
                        uuid,
                        payload: msg,
                    });
                }
            }

            "tool_result" => {
                let payload = serde_json::json!({
                    "role": "tool",
                    "tool_use_id": v.get("tool_use_id"),
                    "content": v.get("content"),
                    "is_error": v.get("is_error"),
                });
                out.messages.push(ExportedMessage {
                    kind: "tool".to_string(),
                    timestamp,
                    uuid,
                    payload,
                });
            }
            "summary" => {
                if let Some(sum) = v.get("summary").cloned() {
                    out.messages.push(ExportedMessage {
                        kind: "summary".to_string(),
                        timestamp,
                        uuid,
                        payload: sum,
                    });
                }
            }
            "system" => match v.get("subtype").and_then(|s| s.as_str()) {
                Some("usage") => {
                    if let Some(u) = v.get("usage") {
                        out.usage.input_tokens +=
                            u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                        out.usage.output_tokens +=
                            u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                    }
                }
                _ => {
                    if let Some(msg) = v.get("message").cloned() {
                        out.messages.push(ExportedMessage {
                            kind: "system".to_string(),
                            timestamp,
                            uuid,
                            payload: msg,
                        });
                    }
                }
            },
            "request" => {

                out.model = out.model.or_else(|| record_str(&v, "model"));
            }

            _ => {}
        }
    }
    Ok(out)
}






const SECRET_PREFIXES: &[&str] = &[
    "sk-ant-",
    "sk-",
    "AIza",
    "xoxb-", "xoxp-", "xoxa-", "xoxr-",
    "ghp_", "gho_", "ghu_", "ghs_", "ghr_",
    "glpat-",
    "hf_",
    "AKIA",
];


const KEY_WORDS: &[&str] = &[
    "api_key", "apikey", "api-key", "token", "secret", "password", "passwd", "pwd", "auth",
];

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '~' | '+' | '/' | '=')
}


pub fn redact_str(s: &str) -> String {
    let mut out = s.to_string();



    let mut search_from = 0;
    while let Some(rel) = out[search_from..].find("-----BEGIN") {
        let start = search_from + rel;
        let after_begin = start + "-----BEGIN".len();
        let header_end = out[after_begin..]
            .find("-----")
            .map(|i| after_begin + i + "-----".len())
            .unwrap_or(out.len());
        let header = &out[start..header_end.min(out.len())];
        if !header.to_ascii_uppercase().contains("PRIVATE KEY") {
            search_from = after_begin;
            continue;
        }
        let end = out[start..].find("-----END").and_then(|i| {
            let after_end = start + i + "-----END".len();
            out[after_end..]
                .find("-----")
                .map(|j| after_end + j + "-----".len())
        });
        match end {
            Some(e) => {
                out.replace_range(start..e.min(out.len()), "[REDACTED PRIVATE KEY]");
                search_from = start + "[REDACTED PRIVATE KEY]".len();
            }
            None => {
                out.replace_range(start.., "[REDACTED PRIVATE KEY]");
                break;
            }
        }
    }


    out = redact_after_marker(&out, "Bearer ", "[REDACTED]");


    for prefix in SECRET_PREFIXES {
        let mut search_from = 0;
        loop {
            let hay = &out[search_from..];
            let Some(rel) = hay.find(prefix) else { break };
            let abs = search_from + rel;

            let min_tail = if *prefix == "sk-" { 20 } else { 8 };
            let tail: String = out[abs + prefix.len()..].chars().take_while(|c| is_token_char(*c)).collect();
            if tail.len() >= min_tail {
                let end = abs + prefix.len() + tail.len();
                out.replace_range(abs..end, "[REDACTED]");
                search_from = abs + "[REDACTED]".len();
            } else {
                search_from = abs + prefix.len();
            }
        }
    }




    redact_assignments_second_pass(&out)
}


fn redact_after_marker(s: &str, marker: &str, replacement: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(marker) {
        let token_start = i + marker.len();
        let token_len: usize = rest[token_start..]
            .chars()
            .take_while(|c| is_token_char(*c))
            .map(|c| c.len_utf8())
            .sum();
        out.push_str(&rest[..token_start]);
        if token_len >= 8 {
            out.push_str(replacement);
        } else {

            out.push_str(&rest[token_start..token_start + token_len]);
        }
        rest = &rest[token_start + token_len..];
    }
    out.push_str(rest);
    out
}





fn redact_assignments_second_pass(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    let mut spans: Vec<(usize, usize)> = vec![];
    for word in KEY_WORDS {
        let mut from = 0;
        while let Some(rel) = lower[from..].find(word) {
            let abs = from + rel;
            let left_ok = abs == 0
                || {
                    let c = s[..abs].chars().next_back().unwrap();
                    !c.is_ascii_alphanumeric() && c != '_'
                };
            if !left_ok {
                from = abs + word.len();
                continue;
            }
            let rest = &s[abs + word.len()..];
            let trimmed = rest.trim_start();
            let mut chars = trimmed.chars();
            match chars.next() {
                Some('=') | Some(':') => {
                    let sep_len = 1;
                    let after = &trimmed[sep_len..];
                    let aval = after.trim_start();
                    let val_start = abs + word.len() + (rest.len() - trimmed.len()) + sep_len
                        + (after.len() - aval.len());
                    let (val_start, quoted) = match aval.chars().next() {
                        Some(q @ ('"' | '\'')) => (val_start + q.len_utf8(), true),
                        _ => (val_start, false),
                    };
                    let vlen: usize = s[val_start..]
                        .chars()
                        .take_while(|c| {
                            if quoted {
                                *c != '"' && *c != '\''
                            } else {
                                !c.is_whitespace() && *c != ',' && *c != ';'
                            }
                        })
                        .map(|c| c.len_utf8())
                        .sum();
                    if vlen >= 4 {
                        spans.push((val_start, val_start + vlen));
                    }
                    from = abs + word.len();
                }
                _ => from = abs + word.len(),
            }
        }
    }
    spans.sort();
    spans.dedup();
    let mut out = String::with_capacity(s.len());
    let mut pos = 0;
    for (a, b) in spans {
        if a < pos {
            continue;
        }
        out.push_str(&s[pos..a]);
        out.push_str("[REDACTED]");
        pos = b;
    }
    out.push_str(&s[pos..]);
    out
}


pub fn redact_value(v: &mut Value) {
    match v {
        Value::String(s) => {
            let r = redact_str(s);
            *s = r;
        }
        Value::Array(a) => {
            for x in a {
                redact_value(x);
            }
        }
        Value::Object(o) => {

            let secret_key = |k: &str| {
                let kl = k.to_ascii_lowercase();
                KEY_WORDS.iter().any(|w| kl == *w || kl.ends_with(&format!("_{}", w)) || kl.ends_with(&format!("-{}", w)))
                    || kl == "authorization"
            };
            for (k, val) in o.iter_mut() {
                if secret_key(k) {
                    if val.is_string() {
                        *val = Value::String("[REDACTED]".to_string());
                    } else {
                        redact_value(val);
                    }
                } else {
                    redact_value(val);
                }
            }
        }
        _ => {}
    }
}







fn payload_text(payload: &Value) -> String {
    match payload {
        Value::String(s) => s.clone(),
        Value::Object(o) => {
            if let Some(s) = o.get("summary").and_then(|v| v.as_str()) {
                return s.to_string();
            }
            match o.get("content") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(blocks)) => blocks
                    .iter()
                    .filter_map(|b| {
                        if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                            b.get("text").and_then(|t| t.as_str())
                        } else {
                            b.get("text").and_then(|t| t.as_str())
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => {

                    serde_json::to_string(payload).unwrap_or_default()
                }
            }
        }
        _ => serde_json::to_string(payload).unwrap_or_default(),
    }
}


fn tool_calls_note(payload: &Value) -> Option<String> {
    let calls = payload.get("tool_calls")?.as_array()?;
    if calls.is_empty() {
        return None;
    }
    let names: Vec<String> = calls
        .iter()
        .filter_map(|c| {
            c.get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .map(String::from)
        })
        .collect();
    if names.is_empty() {
        None
    } else {
        Some(format!("tool calls: {}", names.join(", ")))
    }
}

const TOOL_OUTPUT_LIMIT: usize = 8000;

fn truncate(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_string();
    }

    let mut end = limit;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}…\n[truncated: showing first {} of {} chars]",
        &s[..end],
        end,
        s.len()
    )
}


pub fn render_markdown(exp: &SessionExport) -> String {
    let mut out = format!("# zen session {}\n\n", exp.session_id);
    out.push_str(&format!("- exported by: {}\n", exp.exported_by));
    out.push_str(&format!("- exported at: {}\n", exp.exported_at));
    if let Some(cwd) = &exp.cwd {
        out.push_str(&format!("- cwd: `{}`\n", cwd));
    }
    if let Some(model) = &exp.model {
        out.push_str(&format!("- model: `{}`\n", model));
    }
    out.push_str(&format!(
        "- usage: {} input / {} output tokens\n",
        exp.usage.input_tokens, exp.usage.output_tokens
    ));
    match exp.cost_usd {
        Some(c) => out.push_str(&format!("- cost: ${:.4}\n", c)),
        None => out.push_str("- cost: unknown (no pricing data on this build; free-tier default is $0.00)\n"),
    }
    if exp.redacted {
        out.push_str("- secrets: redacted (`--redact`)\n");
    }
    out.push('\n');

    for m in &exp.messages {
        match m.kind.as_str() {
            "user" => {
                out.push_str("## User\n\n");
                out.push_str(&payload_text(&m.payload));
                out.push_str("\n\n");
            }
            "assistant" => {
                out.push_str("## Assistant\n\n");
                let text = payload_text(&m.payload);
                if !text.trim().is_empty() {
                    out.push_str(&text);
                    out.push_str("\n\n");
                }
                if let Some(note) = tool_calls_note(&m.payload) {
                    out.push_str(&format!("*{note}*\n\n"));
                }
            }
            "tool" => {
                let name = m
                    .payload
                    .get("name")
                    .and_then(|n| n.as_str())
                    .or_else(|| {
                        m.payload
                            .get("tool_use_id")
                            .and_then(|n| n.as_str())
                    })
                    .unwrap_or("tool");
                out.push_str(&format!("### Tool: `{}`\n\n", name));
                out.push_str("```\n");
                out.push_str(&truncate(&payload_text(&m.payload), TOOL_OUTPUT_LIMIT));
                out.push_str("\n```\n\n");
            }
            "summary" => {
                out.push_str("## Summary (context compaction)\n\n");
                out.push_str(&payload_text(&m.payload));
                out.push_str("\n\n");
            }
            "system" => {
                out.push_str(&format!(
                    "> *system:* {}\n\n",
                    truncate(&payload_text(&m.payload), 500).replace('\n', " ")
                ));
            }
            _ => {}
        }
    }
    out
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}


pub fn render_html(exp: &SessionExport) -> String {
    let mut body = String::new();
    body.push_str(&format!(
        "<p class=\"meta\">exported by {} at {}",
        escape_html(&exp.exported_by),
        escape_html(&exp.exported_at)
    ));
    if let Some(cwd) = &exp.cwd {
        body.push_str(&format!(" · cwd <code>{}</code>", escape_html(cwd)));
    }
    if let Some(model) = &exp.model {
        body.push_str(&format!(" · model <code>{}</code>", escape_html(model)));
    }
    body.push_str(&format!(
        " · {} in / {} out tokens",
        exp.usage.input_tokens, exp.usage.output_tokens
    ));
    if exp.redacted {
        body.push_str(" · secrets redacted");
    }
    body.push_str("</p>\n");

    for m in &exp.messages {
        let (cls, label) = match m.kind.as_str() {
            "user" => ("user", "User"),
            "assistant" => ("assistant", "Assistant"),
            "tool" => ("tool", "Tool"),
            "summary" => ("summary", "Summary (compaction)"),
            _ => ("system", "System"),
        };
        body.push_str(&format!("<section class=\"{cls}\"><h2>{label}</h2>"));
        if m.kind == "tool" {
            body.push_str(&format!(
                "<pre>{}</pre>",
                escape_html(&truncate(&payload_text(&m.payload), TOOL_OUTPUT_LIMIT))
            ));
        } else {
            body.push_str(&format!("<pre>{}</pre>", escape_html(&payload_text(&m.payload))));
        }
        body.push_str("</section>\n");
    }

    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>zen session {sid}</title>\n<style>\n\
         body{{font-family:system-ui,sans-serif;max-width:72ch;margin:2em auto;padding:0 1em;color:#1a1a1a}}\n\
         .meta{{color:#666;font-size:.9em}}\n\
         section{{border:1px solid #ddd;border-radius:8px;padding:1em;margin:1em 0}}\n\
         section.user{{border-left:4px solid #2563eb}}\n\
         section.assistant{{border-left:4px solid #16a34a}}\n\
         section.tool{{border-left:4px solid #d97706}}\n\
         section.summary{{border-left:4px solid #9333ea}}\n\
         pre{{white-space:pre-wrap;word-break:break-word;background:#f6f6f6;padding:.75em;border-radius:6px}}\n\
         h2{{margin-top:0;font-size:1em;text-transform:uppercase;letter-spacing:.05em;color:#555}}\n\
         </style>\n</head>\n<body>\n<h1>zen session {sid}</h1>\n{body}</body>\n</html>\n",
        sid = escape_html(&exp.session_id),
        body = body,
    )
}


pub fn render_json(exp: &SessionExport) -> Result<String> {
    serde_json::to_string_pretty(exp).context("cannot serialize session export")
}




fn resolve_session_path(
    cwd: &Path,
    home: Option<&Path>,
    session: Option<&str>,
) -> Result<PathBuf> {
    let home = home.ok_or_else(|| anyhow!("cannot determine home directory"))?;
    match session {
        Some(id) => crate::session::find_session_path(home, id).ok_or_else(|| {
            anyhow!(
                "session not found: {id} (searched ~/.zen/projects for <id>.jsonl)"
            )
        }),
        None => {
            let encoded = crate::session::encode_cwd(cwd);
            crate::session::latest_session_for_cwd(home, &encoded).ok_or_else(|| {
                anyhow!("no sessions found for the current directory")
            })
        }
    }
}


pub fn export_session(path: &Path, redact: bool) -> Result<SessionExport> {
    let loaded = load_session_for_export(path)?;
    let mut exp = SessionExport {
        format_version: EXPORT_FORMAT_VERSION,
        exported_by: exporter_version(),
        exported_at: chrono::Utc::now().to_rfc3339(),
        session_id: loaded.session_id,
        cwd: loaded.cwd,
        model: loaded.model,
        usage: loaded.usage,
        cost_usd: None,
        redacted: false,
        messages: loaded.messages,
    };
    if redact {
        for m in &mut exp.messages {
            redact_value(&mut m.payload);
        }
        exp.redacted = true;
    }
    Ok(exp)
}


pub fn run_export(
    cwd: &Path,
    home: Option<&Path>,
    format: ExportFormat,
    session: Option<&str>,
    output: Option<&Path>,
    redact: bool,
) -> Result<()> {
    let path = resolve_session_path(cwd, home, session)?;
    let exp = export_session(&path, redact)?;
    let rendered = match format {
        ExportFormat::Json => render_json(&exp)?,
        ExportFormat::Markdown => render_markdown(&exp),
        ExportFormat::Html => render_html(&exp),
    };
    match output {
        Some(p) => {
            std::fs::write(p, rendered)
                .with_context(|| format!("cannot write {}", p.display()))?;
            println!("session exported to {}", p.display());
        }
        None => print!("{}", rendered),
    }
    Ok(())
}


pub fn run_import(cwd: &Path, home: Option<&Path>, file: &Path) -> Result<String> {
    let home = home.ok_or_else(|| anyhow!("cannot determine home directory"))?;
    let raw = std::fs::read_to_string(file)
        .with_context(|| format!("cannot read {}", file.display()))?;
    let exp: SessionExport = serde_json::from_str(&raw).with_context(|| {
        format!(
            "cannot parse {} as a zen session export (only `--format json` exports can be imported)",
            file.display()
        )
    })?;
    if exp.format_version > EXPORT_FORMAT_VERSION {
        return Err(anyhow!(
            "export format v{} is newer than this zen (v{}); upgrade zen to import it",
            exp.format_version,
            EXPORT_FORMAT_VERSION
        ));
    }
    if exp.format_version < EXPORT_FORMAT_VERSION {
        return Err(anyhow!(
            "export format v{} is no longer supported by this zen (v{})",
            exp.format_version,
            EXPORT_FORMAT_VERSION
        ));
    }

    let new_id = crate::session::new_session_id();
    let path = crate::session::ensure_session_path(home, cwd, &new_id)?;
    let cwd_s = cwd.to_string_lossy().into_owned();
    crate::session::append_typed(
        &path,
        "system",
        &new_id,
        &cwd_s,
        &uuid::Uuid::new_v4().to_string(),
        None,
        Some(serde_json::json!({
            "role": "system",
            "content": format!(
                "imported from zen session export {} (original session {})",
                file.display(),
                exp.session_id
            ),
        })),
        None,
    )?;
    for m in &exp.messages {
        let (message, summary) = if m.kind == "summary" {
            (None, Some(m.payload.clone()))
        } else {
            (Some(m.payload.clone()), None)
        };
        crate::session::append_typed(
            &path,
            &m.kind,
            &new_id,
            &cwd_s,
            &m.uuid.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            None,
            message,
            summary,
        )?;
    }
    println!("imported as session {}", new_id);
    Ok(new_id)
}



#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_home(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("zen_export_{}_{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }


    fn sample_transcript(home: &Path, cwd: &Path, sid: &str) -> PathBuf {
        let path = crate::session::ensure_session_path(home, cwd, sid).unwrap();
        let c = cwd.to_string_lossy().into_owned();
        let u = || uuid::Uuid::new_v4().to_string();
        crate::session::append_typed(
            &path, "user", sid, &c, &u(), None,
            Some(serde_json::json!({"role": "user", "content": "fix the login bug"})),
            None,
        )
        .unwrap();
        crate::session::append_typed(
            &path, "assistant", sid, &c, &u(), None,
            Some(serde_json::json!({
                "role": "assistant",
                "content": "On it.",
                "tool_calls": [{"id": "t1", "function": {"name": "Read", "arguments": "{}"}}],
            })),
            None,
        )
        .unwrap();
        crate::session::append_typed(
            &path, "tool", sid, &c, &u(), None,
            Some(serde_json::json!({"role": "tool", "name": "Read", "content": "fn login() {}"})),
            None,
        )
        .unwrap();

        crate::session::append_record(
            &path,
            &serde_json::json!({"type": "request", "v": 1, "model": "test-model", "messages": []}),
        )
        .unwrap();
        crate::session::append_typed(
            &path, "summary", sid, &c, &u(), None, None,
            Some(serde_json::json!({"summary": "compacted earlier work"})),
        )
        .unwrap();
        let usage = zen_core::UsageStats { input_tokens: 100, output_tokens: 50, ..Default::default() };
        crate::session::append_usage(&path, sid, &usage).unwrap();
        path
    }

    #[test]
    fn export_json_has_format_version_and_all_messages() {
        let home = tmp_home("json");
        let cwd = tmp_home("jsoncwd");
        let path = sample_transcript(&home, &cwd, "sess-json-1");
        let exp = export_session(&path, false).unwrap();
        assert_eq!(exp.format_version, EXPORT_FORMAT_VERSION);
        assert_eq!(exp.session_id, "sess-json-1");
        assert_eq!(exp.model.as_deref(), Some("test-model"));
        assert_eq!(exp.usage.input_tokens, 100);
        assert_eq!(exp.usage.output_tokens, 50);

        assert_eq!(exp.messages.len(), 4);
        assert!(exp.exported_by.starts_with("zen "));
        let json = render_json(&exp).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["format_version"], 1);
    }

    #[test]
    fn export_markdown_renders_sections() {
        let home = tmp_home("md");
        let cwd = tmp_home("mdcwd");
        let path = sample_transcript(&home, &cwd, "sess-md-1");
        let exp = export_session(&path, false).unwrap();
        let md = render_markdown(&exp);
        assert!(md.contains("# zen session sess-md-1"));
        assert!(md.contains("## User"));
        assert!(md.contains("fix the login bug"));
        assert!(md.contains("## Assistant"));
        assert!(md.contains("tool calls: Read"));
        assert!(md.contains("### Tool:"));
        assert!(md.contains("## Summary (context compaction)"));
        assert!(md.contains("compacted earlier work"));
    }

    #[test]
    fn export_html_escapes_content() {
        let home = tmp_home("html");
        let cwd = tmp_home("htmlcwd");
        let path = crate::session::ensure_session_path(&home, &cwd, "sess-html-1").unwrap();
        let c = cwd.to_string_lossy().into_owned();
        crate::session::append_typed(
            &path, "user", "sess-html-1", &c, &uuid::Uuid::new_v4().to_string(), None,
            Some(serde_json::json!({"role": "user", "content": "<script>alert(1)</script>"})),
            None,
        )
        .unwrap();
        let exp = export_session(&path, false).unwrap();
        let html = render_html(&exp);
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<script>alert"));
        assert!(html.contains("<!DOCTYPE html>"));
    }

    #[test]
    fn redaction_catches_common_secrets() {
        let cases = [
            ("anthropic", "key is sk-ant-abcdef0123456789XYZ ok", "sk-ant-"),
            ("openai", "sk-abcdefghijklmnopqrstuvwxyz0123456789ABCD", "sk-abc"),
            ("google", "AIzaSyD-abcdefghijklmnopqrstuvwxyz0123", "AIza"),
            ("github", "token ghp_0123456789abcdefghijKLMNOP", "ghp_"),
            ("aws", "AKIAIOSFODNN7EXAMPLE here", "AKIA"),
            ("bearer", "Authorization: Bearer abcdef0123456789XYZ", "Bearer abcdef"),
            ("assign-eq", "api_key = supersecretvalue123", "supersecretvalue"),
            ("assign-colon", "password: hunter2hunter2", "hunter2"),
            (
                "pem",
                "-----BEGIN RSA PRIVATE KEY-----\nMIIBPAIBAA==\n-----END RSA PRIVATE KEY-----",
                "MIIBPAIBAA",
            ),
        ];
        for (name, input, secret) in cases {
            let redacted = redact_str(input);
            assert!(
                !redacted.contains(secret),
                "{name}: secret survived redaction: {redacted}"
            );
            assert!(
                redacted.contains("[REDACTED"),
                "{name}: no redaction marker: {redacted}"
            );
        }

        let innocent = "the task is to fix login";
        assert_eq!(redact_str(innocent), innocent);
    }

    #[test]
    fn redact_value_recurses_and_redacts_secret_keys() {
        let mut v = serde_json::json!({
            "role": "user",
            "content": "my key sk-ant-abcdef0123456789XYZ done",
            "api_key": "should-be-gone-entirely",
            "nested": {"token": "tok_abcdef0123456789"},
        });
        redact_value(&mut v);
        let s = serde_json::to_string(&v).unwrap();
        assert!(!s.contains("sk-ant-"));
        assert!(!s.contains("should-be-gone-entirely"));
        assert!(!s.contains("tok_abcdef"));
    }

    #[test]
    fn export_with_redact_flag_marks_document() {
        let home = tmp_home("redact");
        let cwd = tmp_home("redactcwd");
        let path = crate::session::ensure_session_path(&home, &cwd, "sess-r-1").unwrap();
        let c = cwd.to_string_lossy().into_owned();
        crate::session::append_typed(
            &path, "user", "sess-r-1", &c, &uuid::Uuid::new_v4().to_string(), None,
            Some(serde_json::json!({"role": "user", "content": "key sk-ant-abcdef0123456789XYZ"})),
            None,
        )
        .unwrap();
        let plain = export_session(&path, false).unwrap();
        assert!(!plain.redacted);
        let red = export_session(&path, true).unwrap();
        assert!(red.redacted);
        let s = serde_json::to_string(&red).unwrap();
        assert!(!s.contains("sk-ant-"));
    }

    #[test]
    fn import_roundtrip_preserves_messages() {
        let home = tmp_home("rt");
        let cwd = tmp_home("rtcwd");
        let path = sample_transcript(&home, &cwd, "sess-rt-1");
        let exp = export_session(&path, false).unwrap();
        let n_messages = exp.messages.len();

        let file = home.join("export.json");
        fs::write(&file, render_json(&exp).unwrap()).unwrap();
        let new_id = run_import(&cwd, Some(home.as_path()), &file).unwrap();
        assert_ne!(new_id, "sess-rt-1");


        let new_path = crate::session::find_session_path(&home, &new_id).unwrap();
        let exp2 = export_session(&new_path, false).unwrap();

        assert_eq!(exp2.messages.len(), n_messages + 1);
        let kinds: Vec<&str> = exp2.messages.iter().map(|m| m.kind.as_str()).collect();
        assert!(kinds.contains(&"user"));
        assert!(kinds.contains(&"assistant"));
        assert!(kinds.contains(&"tool"));
        assert!(kinds.contains(&"summary"));
    }

    #[test]
    fn import_rejects_newer_format_version() {
        let home = tmp_home("ver");
        let cwd = tmp_home("vercwd");
        let file = home.join("future.json");
        fs::write(
            &file,
            serde_json::json!({
                "format_version": EXPORT_FORMAT_VERSION + 1,
                "exported_by": "zen 99",
                "exported_at": "2026-01-01T00:00:00Z",
                "session_id": "x",
                "usage": {"input_tokens": 0, "output_tokens": 0},
                "redacted": false,
                "messages": [],
            })
            .to_string(),
        )
        .unwrap();
        let err = run_import(&cwd, Some(home.as_path()), &file).unwrap_err();
        assert!(err.to_string().contains("newer than this zen"));
    }

    #[test]
    fn import_rejects_non_json() {
        let home = tmp_home("nonjson");
        let cwd = tmp_home("nonjsoncwd");
        let file = home.join("export.md");
        fs::write(&file, "# zen session x\n").unwrap();
        let err = run_import(&cwd, Some(home.as_path()), &file).unwrap_err();
        assert!(err.to_string().contains("only `--format json` exports"));
    }

    #[test]
    fn export_errors_on_missing_session() {
        let home = tmp_home("missing");
        let cwd = tmp_home("missingcwd");
        let err = run_export(&cwd, Some(home.as_path()), ExportFormat::Json, Some("nope"), None, false)
            .unwrap_err();
        assert!(err.to_string().contains("session not found"));
        let err2 = run_export(&cwd, Some(home.as_path()), ExportFormat::Json, None, None, false)
            .unwrap_err();
        assert!(err2.to_string().contains("no sessions found"));
    }

    #[test]
    fn corrupt_transcript_line_is_an_error_not_silent() {
        let home = tmp_home("corrupt");
        let cwd = tmp_home("corruptcwd");
        let path = crate::session::ensure_session_path(&home, &cwd, "sess-c-1").unwrap();
        fs::write(&path, "{\"type\":\"user\"}\nnot json at all\n").unwrap();
        let err = load_session_for_export(&path).unwrap_err();
        assert!(err.to_string().contains("invalid transcript line"));
    }
}
