//! Session management: transcript persistence, session IDs, project directories, and
//! resume/continue logic (SPEC §10).
//!
//! Transcripts are stored as JSONL files under `~/.zen/projects/<encoded-cwd>/<session-id>.jsonl`.
//! Each line is a `TranscriptRecord` with a `type` field distinguishing user/assistant/tool
//! messages from summary records (written after context compaction). The `load_messages`
//! function implements the SPEC requirement to load only messages *after* the last summary,
//! so that compacted history is not re-sent to the model.

use anyhow::Result;
use chrono::Utc;
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use uuid::Uuid;

/// Encode an absolute cwd into the project-dir name (SPEC §10): replace `/`, `\`, `:`
/// and `.` with `-`. Backslash and colon matter on Windows (`C:\Users\...`), where a
/// leftover `\` would be read as a path separator and `:` is not a valid filename char.
pub fn encode_cwd(path: &Path) -> String {
    let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let s = abs.to_string_lossy();
    s.replace(['/', '\\', ':', '.'], "-")
}

/// Build the transcript directory: <home>/.zen/projects/<encoded-cwd>
pub fn project_dir(home: &Path, encoded: &str) -> PathBuf {
    home.join(".zen").join("projects").join(encoded)
}

/// Build the transcript path for a session id.
pub fn session_path(proj_dir: &Path, session_id: &str) -> PathBuf {
    proj_dir.join(format!("{}.jsonl", session_id))
}

/// Scratch directory of a session (spilled tool output, background-job logs): the transcript
/// path without `.jsonl`, i.e. `<project-dir>/<session-id>/` (p0b).
pub fn session_scratch_dir(transcript: &Path) -> PathBuf {
    transcript.with_extension("")
}

/// Find a session file by id (search name) or path across all project dirs.
pub fn find_session_path(home: &Path, id_or_path: &str) -> Option<PathBuf> {
    let p = PathBuf::from(id_or_path);
    if p.is_absolute() && p.exists() {
        return Some(p);
    }
    if p.exists() {
        if let Ok(abs) = p.canonicalize() {
            return Some(abs);
        }
    }
    // search all session files in all project dirs
    let projects_root = home.join(".zen").join("projects");
    if !projects_root.exists() {
        return None;
    }
    // exact filename match
    for e in walkdir::WalkDir::new(&projects_root).into_iter().flatten() {
        let path = e.path();
        if path.file_name().map(|n| n.to_string_lossy() == format!("{}.jsonl", id_or_path)).unwrap_or(false) {
            return Some(path.to_path_buf());
        }
    }
    None
}

/// Get the most recent session file (by mtime) for the given cwd's project dir.
pub fn latest_session_for_cwd(home: &Path, encoded: &str) -> Option<PathBuf> {
    let dir = project_dir(home, encoded);
    if !dir.exists() {
        return None;
    }
    let mut latest: Option<(SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(&dir).ok()? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) {
            if latest.as_ref().is_none_or(|(t, _)| mtime > *t) {
                latest = Some((mtime, path));
            }
        }
    }
    latest.map(|(_, p)| p)
}

/// One JSONL line. Field names follow the public transcript shape (SPEC C9.1): `parentUuid`,
/// `sessionId` in camelCase.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct TranscriptRecord<'a> {
    r#type: &'a str,
    uuid: String,
    parent_uuid: Option<String>,
    session_id: String,
    timestamp: String,
    cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<Value>,
}

fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

/// Append a transcript record (assistant/user/tool/system/summary) to the session file.
pub fn append_record(path: &Path, record: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    use std::io::Write;
    writeln!(file, "{}", serde_json::to_string(record)?)?;
    Ok(())
}

/// Append a typed record. `message` may be None for summary-only records.
#[allow(clippy::too_many_arguments)]
pub fn append_typed(
    path: &Path,
    r#type: &str,
    session_id: &str,
    cwd: &str,
    uuid_val: &str,
    parent_uuid: Option<&str>,
    message: Option<Value>,
    summary: Option<Value>,
) -> Result<()> {
    let rec = TranscriptRecord {
        r#type,
        uuid: uuid_val.to_string(),
        parent_uuid: parent_uuid.map(String::from),
        session_id: session_id.to_string(),
        timestamp: now_iso(),
        cwd: cwd.to_string(),
        message,
        summary,
    };
    append_record(path, &serde_json::to_value(rec)?)
}

/// Load the replayable history of a transcript as raw message JSON (SPEC C9.3).
///
/// Only what the model still needs is returned: if the transcript contains a compaction
/// `summary` record, everything before the *last* one is dropped and the summary itself comes
/// back as the first (user-role) message, followed by the messages recorded after it. Without a
/// summary the whole message history is returned. `system` metadata lines are skipped and a
/// corrupt line is an error (a silently truncated history would confuse the model).
pub fn load_messages(path: &Path) -> Result<Vec<Value>> {
    if !path.exists() {
        return Ok(vec![]);
    }
    let content = std::fs::read_to_string(path)?;
    let mut records: Vec<Value> = vec![];
    for (n, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(line)
            .map_err(|e| anyhow::anyhow!("{}:{}: invalid transcript line: {}", path.display(), n + 1, e))?;
        records.push(v);
    }
    let last_summary_idx = records
        .iter()
        .rposition(|v| v.get("type").and_then(|t| t.as_str()) == Some("summary"));

    let mut out = vec![];
    let start = match last_summary_idx {
        Some(i) => {
            if let Some(text) = summary_text(&records[i]) {
                out.push(serde_json::json!({ "role": "user", "content": text }));
            }
            i + 1
        }
        None => 0,
    };
    for v in &records[start..] {
        let typ = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if typ == "system" || typ == "summary" || typ == "plan_approved" {
            continue;
        }
        if let Some(msg) = v.get("message") {
            out.push(msg.clone());
        }
    }
    Ok(out)
}

/// The text of a summary record: `summary` may be a plain string or `{"summary": "..."}`.
fn summary_text(rec: &Value) -> Option<String> {
    let s = rec.get("summary")?;
    match s {
        Value::String(t) => Some(t.clone()),
        Value::Object(o) => o.get("summary").and_then(|v| v.as_str()).map(String::from),
        _ => None,
    }
}

/// [`load_messages`] decoded into chat messages (entries that do not parse are skipped).
pub fn load_history(path: &Path) -> Result<Vec<zen_core::ChatMessage>> {
    Ok(load_messages(path)?
        .into_iter()
        .filter_map(|v| serde_json::from_value::<zen_core::ChatMessage>(v).ok())
        .collect())
}

/// Appends one turn's worth of messages to a transcript, chaining `parentUuid`s.
///
/// The writer compares the agent history *before* and *after* a turn:
/// - if `after` extends `before`, only the new messages are appended (user prompt, assistant
///   tool calls, tool results, final answer — the whole exchange, so a resume sees tool use too);
/// - otherwise the turn compacted the history, so a `summary` record (the summary message's
///   text) is written first, followed by every message after it. [`load_messages`] then resumes
///   from that boundary.
pub struct TranscriptWriter {
    pub path: PathBuf,
    pub session_id: String,
    pub cwd: String,
    last_uuid: Option<String>,
    /// Kept open after the first record (perf p0): one open per writer instead of one
    /// mkdir+open+close per record. Flushed after every record, so readers and a crash see
    /// whole lines exactly as before.
    file: Option<std::io::BufWriter<std::fs::File>>,
}

impl TranscriptWriter {
    /// A writer for `path`; the parent chain starts empty.
    pub fn new(path: PathBuf, session_id: &str, cwd: &Path) -> Self {
        Self {
            path,
            session_id: session_id.to_string(),
            cwd: cwd.display().to_string(),
            last_uuid: None,
            file: None,
        }
    }

    /// Append one JSON line through the kept-open handle and flush it.
    fn append_line(&mut self, record: &Value) -> Result<()> {
        use std::io::Write;
        if self.file.is_none() {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let f = std::fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
            self.file = Some(std::io::BufWriter::new(f));
        }
        let line = serde_json::to_string(record)?;
        let w = self.file.as_mut().expect("opened above");
        let res = writeln!(w, "{}", line).and_then(|_| w.flush());
        if let Err(e) = res {
            // Drop the handle so the next record reopens the file instead of reusing a bad one.
            self.file = None;
            return Err(e.into());
        }
        Ok(())
    }

    fn write(&mut self, kind: &str, message: Option<Value>, summary: Option<Value>) -> Result<()> {
        let id = Uuid::new_v4().to_string();
        let rec = TranscriptRecord {
            r#type: kind,
            uuid: id.clone(),
            parent_uuid: self.last_uuid.clone(),
            session_id: self.session_id.clone(),
            timestamp: now_iso(),
            cwd: self.cwd.clone(),
            message,
            summary,
        };
        self.append_line(&serde_json::to_value(rec)?)?;
        self.last_uuid = Some(id);
        Ok(())
    }

    /// Persist the difference between two snapshots of the agent history.
    pub fn persist_turn(
        &mut self,
        before: &[zen_core::ChatMessage],
        after: &[zen_core::ChatMessage],
    ) -> Result<()> {
        let extends = after.len() >= before.len() && after[..before.len()] == *before;
        let fresh: &[zen_core::ChatMessage] = if extends {
            &after[before.len()..]
        } else {
            let (first, rest) = match after.split_first() {
                Some(x) => x,
                None => return Ok(()),
            };
            let text = first.content.clone().unwrap_or_default();
            self.write("summary", None, Some(Value::String(text)))?;
            rest
        };
        for m in fresh {
            let kind = match m.role.as_str() {
                "assistant" => "assistant",
                "tool" => "tool",
                _ => "user",
            };
            self.write(kind, Some(serde_json::to_value(m)?), None)?;
        }
        Ok(())
    }

    /// Record an approved plan (plan-mode approval flow) as a typed transcript record.
    /// Skipped by [`load_messages`]: it is session metadata, not conversation.
    pub fn record_plan_approved(&mut self, plan: &str) -> Result<()> {
        self.write("plan_approved", Some(serde_json::json!({ "plan": plan })), None)
    }
}

/// Ensure the session directory exists and return the path.
pub fn ensure_session_path(home: &Path, cwd: &Path, session_id: &str) -> Result<PathBuf> {
    let encoded = encode_cwd(cwd);
    let proj = project_dir(home, &encoded);
    std::fs::create_dir_all(&proj)?;
    Ok(session_path(&proj, session_id))
}

/// Ensure the session directory exists and return it.
pub fn ensure_session_dir(home: &Path, cwd: &Path, session_id: &str) -> Result<PathBuf> {
    let encoded = encode_cwd(cwd);
    let proj = project_dir(home, &encoded);
    std::fs::create_dir_all(&proj)?;
    let _ = session_id;
    Ok(proj)
}

/// Generate a fresh session id.
pub fn new_session_id() -> String {
    Uuid::new_v4().to_string()
}

/// List all session IDs for the current cwd, most recent first.
pub fn list_sessions_for_cwd(home: &Path, cwd: &Path) -> Vec<(String, PathBuf)> {
    let encoded = encode_cwd(cwd);
    let dir = project_dir(home, &encoded);
    if !dir.exists() {
        return vec![];
    }
    let mut sessions: Vec<(SystemTime, String, PathBuf)> = vec![];
    for entry in std::fs::read_dir(&dir).ok().into_iter().flatten() {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                sessions.push((mtime, stem.to_string(), path));
            }
        }
    }
    sessions.sort_by_key(|s| std::cmp::Reverse(s.0));
    sessions.into_iter().map(|(_, id, p)| (id, p)).collect()
}

/// Append a `system`/`usage` record with the token usage of one `-p` run. Resume skips system
/// records for the history but sums these so the json result's usage covers the whole
/// conversation (SPEC C9.3).
pub fn append_usage(path: &Path, session_id: &str, usage: &zen_core::UsageStats) -> Result<()> {
    append_record(
        path,
        &serde_json::json!({
            "type": "system",
            "subtype": "usage",
            "sessionId": session_id,
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "usage": {"input_tokens": usage.input_tokens, "output_tokens": usage.output_tokens, "cache_read_input_tokens": usage.cache_read_input_tokens},
        }),
    )
}

/// Sum of all `usage` records in a transcript (zero when there are none).
pub fn load_usage(path: &Path) -> zen_core::UsageStats {
    let mut total = zen_core::UsageStats::default();
    let Ok(content) = std::fs::read_to_string(path) else { return total };
    for v in content.lines().filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok()) {
        if v.get("subtype").and_then(|t| t.as_str()) == Some("usage") {
            let n = |k: &str| v["usage"][k].as_u64().unwrap_or(0);
            total.input_tokens += n("input_tokens");
            total.output_tokens += n("output_tokens");
            total.cache_read_input_tokens += n("cache_read_input_tokens");
        }
    }
    total
}

/// Append a `custom-title` record (REPL `/rename`); it carries no message, so resume skips it.
pub fn append_title(path: &Path, session_id: &str, title: &str) -> Result<()> {
    append_record(
        path,
        &serde_json::json!({
            "type": "custom-title",
            "sessionId": session_id,
            "customTitle": title,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }),
    )
}

/// The latest `/rename` title of a transcript, if any.
pub fn session_title(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    content
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("custom-title"))
        .filter_map(|v| v.get("customTitle").and_then(|t| t.as_str()).map(String::from))
        .next_back()
}

/// Append a summary (compaction) record to the transcript.
pub fn append_summary(path: &Path, session_id: &str, cwd: &str, uuid_val: &str, parent_uuid: Option<&str>, summary_text: &str) -> Result<()> {
    let summary_val = serde_json::json!({"type":"summary","summary":summary_text});
    append_typed(path, "summary", session_id, cwd, uuid_val, parent_uuid, None, Some(summary_val))
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn p0b_1_session_scratch_dir_sits_next_to_the_transcript() {
        let t = Path::new("/h/.zen/projects/-p/abc-123.jsonl");
        assert_eq!(session_scratch_dir(t), PathBuf::from("/h/.zen/projects/-p/abc-123"));
    }

    #[test]
    fn encode_cwd_replaces_slashes_and_dots() {
        let tmp = std::env::temp_dir().join(format!("zenenc_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        // canonicalize on tmp dir
        let enc = encode_cwd(&tmp);
        let s = enc;
        assert!(!s.contains('/'));
        assert!(!s.contains('.'));
        assert!(!s.contains('\\'));
        assert!(!s.contains(':'));
    }

    #[test]
    fn append_then_load_roundtrip() {
        let home = std::env::temp_dir().join(format!("zen_sess_rt_{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let cwd = std::env::temp_dir().join(format!("zen_sess_cwd_{}", std::process::id()));
        let _ = fs::remove_dir_all(&cwd);
        fs::create_dir_all(&cwd).unwrap();

        let path = ensure_session_path(&home, &cwd, "sess-rt-1").unwrap();
        let m1 = serde_json::json!({"role":"user","content":"hello"});
        let m2 = serde_json::json!({"role":"assistant","content":"hi there"});
        append_typed(&path, "user", "sess-rt-1", cwd.to_str().unwrap(), "u1", None, Some(m1.clone()), None).unwrap();
        append_typed(&path, "assistant", "sess-rt-1", cwd.to_str().unwrap(), "a1", Some("u1"), Some(m2.clone()), None).unwrap();

        // Before any summary: the whole history.
        let pre = load_messages(&path).unwrap();
        assert_eq!(pre, vec![m1.clone(), m2.clone()]);

        append_typed(&path, "summary", "sess-rt-1", cwd.to_str().unwrap(), "sum1", Some("a1"), None, Some(serde_json::json!({"summary":"done"}))).unwrap();
        let m3 = serde_json::json!({"role":"user","content":"after"});
        append_typed(&path, "user", "sess-rt-1", cwd.to_str().unwrap(), "u2", Some("sum1"), Some(m3.clone()), None).unwrap();

        // After a summary: summary as first message, then what followed; older turns dropped.
        let msgs = load_messages(&path).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0], serde_json::json!({"role":"user","content":"done"}));
        assert_eq!(msgs[1], m3);
        // Records use camelCase keys.
        let first: Value = serde_json::from_str(std::fs::read_to_string(&path).unwrap().lines().next().unwrap()).unwrap();
        assert!(first.get("sessionId").is_some() && first.get("parentUuid").is_some());
    }

    #[test]
    fn transcript_writer_appends_delta_and_summary_on_compaction() {
        use zen_core::ChatMessage;
        let dir = std::env::temp_dir().join(format!("zen_tw_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let mut w = TranscriptWriter::new(path.clone(), "s", &dir);
        let t1 = vec![ChatMessage::user("q1"), ChatMessage::assistant(Some("a1".into()), None)];
        w.persist_turn(&[], &t1).unwrap();
        let mut t2 = t1.clone();
        t2.push(ChatMessage::user("q2"));
        t2.push(ChatMessage::assistant(Some("a2".into()), None));
        w.persist_turn(&t1, &t2).unwrap();
        assert_eq!(load_history(&path).unwrap(), t2);
        // Compaction: history replaced by summary + tail.
        let t3 = vec![
            ChatMessage::user("[Compaction summary of the conversation so far]\nS"),
            ChatMessage::assistant(Some("a2".into()), None),
            ChatMessage::user("q3"),
        ];
        w.persist_turn(&t2, &t3).unwrap();
        assert_eq!(load_history(&path).unwrap(), t3);
        // parentUuid chain: every record after the first points at its predecessor.
        let lines: Vec<Value> = std::fs::read_to_string(&path).unwrap().lines()
            .map(|l| serde_json::from_str(l).unwrap()).collect();
        for pair in lines.windows(2) {
            assert_eq!(pair[1]["parentUuid"], pair[0]["uuid"]);
        }
    }

    #[test]
    fn latest_session_picks_newest() {
        let home = std::env::temp_dir().join(format!("zen_sess_latest_{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let cwd = std::env::temp_dir().join(format!("zen_sess_lcwd_{}", std::process::id()));
        let _ = fs::remove_dir_all(&cwd);
        fs::create_dir_all(&cwd).unwrap();

        let p1 = ensure_session_path(&home, &cwd, "old").unwrap();
        append_typed(&p1, "user", "old", cwd.to_str().unwrap(), "u", None, Some(serde_json::json!({"role":"user","content":"hi"})), None).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        let p2 = ensure_session_path(&home, &cwd, "new").unwrap();
        append_typed(&p2, "user", "new", cwd.to_str().unwrap(), "u", None, Some(serde_json::json!({"role":"user","content":"hi"})), None).unwrap();

        let latest = latest_session_for_cwd(&home, &encode_cwd(&cwd)).unwrap();
        assert_eq!(latest.file_name().unwrap(), "new.jsonl");
    }

    #[test]
    fn find_session_by_id() {
        let home = std::env::temp_dir().join(format!("zen_sess_find_{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let cwd = std::env::temp_dir().join(format!("zen_sess_fcwd_{}", std::process::id()));
        let _ = fs::remove_dir_all(&cwd);
        fs::create_dir_all(&cwd).unwrap();
        let path = ensure_session_path(&home, &cwd, "findme").unwrap();
        append_typed(&path, "user", "findme", cwd.to_str().unwrap(), "u", None, Some(serde_json::json!({"role":"user","content":"hi"})), None).unwrap();
        assert!(path.exists());
        let found = find_session_path(&home, "findme").unwrap();
        assert_eq!(found, path);
        assert!(find_session_path(&home, "nonexistent").is_none());
    }

    #[test]
    fn find_session_by_path() {
        let home = std::env::temp_dir().join(format!("zen_sess_findp_{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let cwd = std::env::temp_dir().join(format!("zen_sess_fpcwd_{}", std::process::id()));
        let _ = fs::remove_dir_all(&cwd);
        fs::create_dir_all(&cwd).unwrap();
        let path = ensure_session_path(&home, &cwd, "sess-x").unwrap();
        append_typed(&path, "user", "sess-x", cwd.to_str().unwrap(), "u", None, None, None).unwrap();
        let found = find_session_path(&home, path.to_str().unwrap()).unwrap();
        assert_eq!(found, path);
    }

    #[test]
    fn list_sessions_for_cwd_returns_sorted() {
        let home = std::env::temp_dir().join(format!("zen_sess_list_{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let cwd = std::env::temp_dir().join(format!("zen_sess_lcwd_{}", std::process::id()));
        let _ = fs::remove_dir_all(&cwd);
        fs::create_dir_all(&cwd).unwrap();

        let p1 = ensure_session_path(&home, &cwd, "sess-a").unwrap();
        append_typed(&p1, "user", "sess-a", cwd.to_str().unwrap(), "u", None, Some(serde_json::json!({"role":"user","content":"hi"})), None).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        let p2 = ensure_session_path(&home, &cwd, "sess-b").unwrap();
        append_typed(&p2, "user", "sess-b", cwd.to_str().unwrap(), "u", None, Some(serde_json::json!({"role":"user","content":"hi"})), None).unwrap();

        let sessions = list_sessions_for_cwd(&home, &cwd);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].0, "sess-b"); // newest first
        assert_eq!(sessions[1].0, "sess-a");
    }

    #[test]
    fn p0_2_transcript_writer_keeps_file_open_and_flushes_each_record() {
        use zen_core::ChatMessage;
        let dir = std::env::temp_dir().join(format!("zen_tw_open_{}", Uuid::new_v4()));
        let path = dir.join("nested").join("s.jsonl");
        let mut w = TranscriptWriter::new(path.clone(), "s", &dir);
        assert!(w.file.is_none(), "nothing opened before the first record");
        let mut hist = vec![];
        for i in 0..5 {
            let before = hist.clone();
            hist.push(ChatMessage::user(format!("q{}", i)));
            w.persist_turn(&before, &hist).unwrap();
            assert!(w.file.is_some(), "handle stays open between records");
            // Flushed: the record is on disk while the writer is still alive.
            let users = std::fs::read_to_string(&path)
                .unwrap()
                .lines()
                .filter(|l| l.contains("\"type\":\"user\""))
                .count();
            assert_eq!(users, i + 1);
            // Another appender on the same file (usage records) interleaves whole lines.
            if i == 2 {
                append_usage(&path, "s", &zen_core::UsageStats::default()).unwrap();
            }
        }
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content.lines().count(), 6);
        for l in content.lines() {
            serde_json::from_str::<Value>(l).expect("every line is whole JSON");
        }
        let msgs = load_messages(&path).unwrap();
        assert_eq!(msgs.len(), 5);
        // Parent chain is intact across the kept-open writes.
        let recs: Vec<Value> = content.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let users: Vec<&Value> = recs.iter().filter(|r| r["type"] == "user").collect();
        for pair in users.windows(2) {
            assert_eq!(pair[1]["parentUuid"], pair[0]["uuid"]);
        }
    }
}
