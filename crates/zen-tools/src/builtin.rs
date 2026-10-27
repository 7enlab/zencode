use crate::gitignore::{is_ignored, load_ignore_rules};
use async_trait::async_trait;
use regex::Regex;
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use crate::edit_match::apply_edit;
use zen_core::{Tool, ToolContext, ToolOutput, TodoItem};

pub const BASH_MAX_CHARS: usize = 30_000;

pub const BASH_DEFAULT_TIMEOUT_MS: u64 = 120_000;

pub const READ_DEFAULT_LIMIT: usize = 2000;

pub const READ_MAX_LINE_CHARS: usize = 2000;

fn detect_binary_kind(bytes: &[u8]) -> &'static str {
    if bytes.len() >= 8 && &bytes[0..8] == b"\x89PNG\r\n\x1a\n" {
        "PNG image"
    } else if bytes.len() >= 3 && &bytes[0..3] == b"\xff\xd8\xff" {
        "JPEG image"
    } else if bytes.len() >= 6 && (&bytes[0..6] == b"GIF87a" || &bytes[0..6] == b"GIF89a") {
        "GIF image"
    } else if bytes.len() >= 4 && &bytes[0..4] == b"RIFF" && bytes.len() >= 12 && &bytes[8..12] == b"WEBP" {
        "WebP image"
    } else if bytes.len() >= 4 && &bytes[0..4] == b"%PDF" {
        "PDF document"
    } else if bytes.len() >= 2 && &bytes[0..2] == b"\x1f\x8b" {
        "gzip archive"
    } else if bytes.len() >= 4 && &bytes[0..4] == b"\x7fELF" {
        "ELF binary"
    } else if bytes.len() >= 4 && &bytes[0..4] == b"PK\x03\x04" {
        "ZIP archive"
    } else if bytes.len() >= 2 && &bytes[0..2] == b"BM" {
        "BMP image"
    } else if bytes.len() >= 4 && (&bytes[0..4] == b"II\x2a\x00" || &bytes[0..4] == b"MM\x00\x2a") {
        "TIFF image"
    } else {
        "binary"
    }
}

pub fn expand_tilde(input: &str) -> PathBuf {
    if input == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = input.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(input)
}

pub fn resolve_path(cwd: &Path, input: &str) -> PathBuf {
    let expanded = expand_tilde(input);
    if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    }
}

pub fn canonicalize_lenient(p: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(p) {
        return c;
    }

    let mut existing: &Path = p;
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    while !existing.as_os_str().is_empty() && !existing.exists() {
        match existing.file_name() {
            Some(name) => {
                rest.push(name.to_os_string());
                existing = existing.parent().unwrap_or(Path::new(""));
            }
            None => break,
        }
    }
    let base = if existing.as_os_str().is_empty() {
        return lexical_normalize(p);
    } else {
        match std::fs::canonicalize(existing) {
            Ok(c) => c,
            Err(_) => return lexical_normalize(p),
        }
    };
    let mut out = base;
    for comp in rest.iter().rev() {
        out.push(comp);
    }
    lexical_normalize(&out)
}

fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty())
}

fn missing(key: &str) -> ToolOutput {
    ToolOutput::err(format!("Error: missing required parameter '{}'", key))
}

pub fn truncate_middle(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    let head_len = max_chars / 2;
    let tail_len = max_chars - head_len;
    let head: String = s.chars().take(head_len).collect();
    let tail: String = s.chars().skip(count - tail_len).collect();
    format!(
        "{}\n... [truncated {} chars] ...\n{}",
        head,
        count - max_chars,
        tail
    )
}

fn safe_file_stem(id: &str) -> String {
    let s: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .take(80)
        .collect();
    if s.is_empty() {
        "output".into()
    } else {
        s
    }
}

fn fresh_stem() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("out-{}-{}-{}", std::process::id(), nanos, N.fetch_add(1, Ordering::Relaxed))
}

pub fn spill_output(text: &str, max_chars: usize, ctx: &ToolContext) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let dir = ctx.scratch_dir().join("tool-output");
    let mut stem = ctx.current_tool_use_id.as_deref().map(safe_file_stem).unwrap_or_else(fresh_stem);
    if dir.join(format!("{}.txt", stem)).exists() {
        stem = format!("{}-{}", stem, fresh_stem());
    }
    let path = dir.join(format!("{}.txt", stem));
    let written = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, text));
    if let Err(e) = written {
        return format!(
            "{}\n[zen: could not save the full output to {}: {}]",
            truncate_middle(text, max_chars),
            path.display(),
            e
        );
    }
    let lines = text.lines().count();
    let head_len = max_chars / 2;
    let tail_len = max_chars - head_len;
    let head: String = text.chars().take(head_len).collect();
    let tail: String = text.chars().skip(count - tail_len).collect();
    let head_lines = head.lines().count();
    format!(
        "{}\n... [output too long: {} bytes, {} lines in total; {} chars omitted here. Full output saved to {} \
         (lines 1-{} shown above, the end below). Use Read with offset/limit or Grep on that file to see the rest.] ...\n{}",
        head,
        text.len(),
        lines,
        count - max_chars,
        path.display(),
        head_lines,
        tail
    )
}

pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "Read"
    }
    fn description(&self) -> &str {
        "Read a text file with 1-based line numbers (marks it read, enabling Write/Edit); page with offset/limit."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {"type": "string", "description": "File to read, absolute or relative to the working directory."},
                "offset": {"type": "integer", "description": "1-based line number to start from."},
                "limit": {"type": "integer", "description": "Maximum number of lines to return (default 2000)."}
            },
            "required": ["file_path"],
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        true
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let Some(fp) = arg_str(&args, "file_path") else { return missing("file_path") };
        let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(1).max(1) as usize;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(READ_DEFAULT_LIMIT);

        let path = canonicalize_lenient(&resolve_path(&ctx.cwd(), fp));
        if path.is_dir() {
            return ToolOutput::err(format!("Error: {} is a directory, not a file", path.display()));
        }

        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) => return ToolOutput::err(format!("Error reading {}: {}", path.display(), e)),
        };

        if bytes.contains(&0) {
            let kind = detect_binary_kind(&bytes);
            return ToolOutput::err(format!(
                "Error: {} is a binary file ({} bytes, {}) - Read only handles UTF-8 text",
                path.display(),
                bytes.len(),
                kind
            ));
        }

        let text = match String::from_utf8(bytes) {
            Ok(t) => t,
            Err(_) => {
                return ToolOutput::err(format!(
                    "Error: {} is not valid UTF-8 text ({} bytes) - Read only handles text files",
                    path.display(),
                    path.metadata().map(|m| m.len()).unwrap_or(0)
                ));
            }
        };
        ctx.read_files.lock().unwrap().insert(path.clone(), content_stamp(text.as_bytes()));

        let total = text.lines().count();
        let mut out = String::new();
        for (idx, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
            let shown = if line.chars().count() > READ_MAX_LINE_CHARS {
                let cut: String = line.chars().take(READ_MAX_LINE_CHARS).collect();
                format!("{}... [line truncated]", cut)
            } else {
                line.to_string()
            };
            out.push_str(&format!("{:>6}\t{}\n", idx + 1, shown));
        }
        if out.is_empty() {
            return ToolOutput::ok(format!(
                "(file {} has {} lines; nothing in range offset={} limit={})",
                path.display(),
                total,
                offset,
                limit
            ));
        }
        ToolOutput::ok(out)
    }
}

pub struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "Write"
    }
    fn description(&self) -> &str {
        "Create or overwrite a file (existing files must have been read first); parent dirs are created."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {"type": "string", "description": "File to write, absolute or relative to the working directory."},
                "content": {"type": "string", "description": "Full new contents of the file."}
            },
            "required": ["file_path", "content"],
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        false
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let Some(fp) = arg_str(&args, "file_path") else { return missing("file_path") };
        let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let path = canonicalize_lenient(&resolve_path(&ctx.cwd(), fp));

        let mut old_text: Option<String> = None;
        if path.exists() {
            if path.is_dir() {
                return ToolOutput::err(format!("Error: {} is a directory", path.display()));
            }
            let known = ctx.read_files.lock().unwrap().contains_key(&path);
            if !known {
                return ToolOutput::err(format!(
                    "Error: {} already exists and has not been read in this session. Read it before overwriting.",
                    path.display()
                ));
            }
            if let Ok(current) = tokio::fs::read(&path).await {
                if let Err(e) = check_fresh(ctx, &path, &current) {
                    return e;
                }
                old_text = String::from_utf8(current).ok();
            }
        }
        if let Err(msg) = crate::syntax_guard::guard_write(&path, old_text.as_deref(), content).await {
            return ToolOutput::err(msg);
        }
        if let Some(parent) = path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return ToolOutput::err(format!("Error creating directory {}: {}", parent.display(), e));
            }
        }
        let ck_warning = checkpoint_before(ctx, &path);
        match crate::atomic::write_atomic_async(path.clone(), content.as_bytes().to_vec()).await {
            Ok(warning) => {
                checkpoint_after(ctx, &path, content.as_bytes());
                ctx.read_files.lock().unwrap().insert(path.clone(), content_stamp(content.as_bytes()));
                ToolOutput::ok(with_warning(
                    with_warning(format!("Wrote {} bytes to {}", content.len(), path.display()), warning),
                    ck_warning,
                ))
            }
            Err(e) => ToolOutput::err(format!("Error writing {}: {}", path.display(), e)),
        }
    }
}

pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "Edit"
    }
    fn description(&self) -> &str {
        "Replace an exact string in a file (exactly once unless replace_all); file must have been read first."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {"type": "string", "description": "File to edit."},
                "old_string": {"type": "string", "description": "Exact text to replace."},
                "new_string": {"type": "string", "description": "Replacement text."},
                "replace_all": {"type": "boolean", "description": "Replace every occurrence instead of requiring uniqueness."}
            },
            "required": ["file_path", "old_string", "new_string"],
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        false
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let Some(fp) = arg_str(&args, "file_path") else { return missing("file_path") };
        let Some(old) = args.get("old_string").and_then(|v| v.as_str()) else {
            return missing("old_string");
        };
        let Some(new) = args.get("new_string").and_then(|v| v.as_str()) else {
            return missing("new_string");
        };
        let replace_all = args.get("replace_all").and_then(|v| v.as_bool()).unwrap_or(false);
        let (path, text) = match load_for_edit(ctx, fp).await {
            Ok(v) => v,
            Err(e) => return e,
        };
        let label = path.display().to_string();
        let applied = match apply_edit(&text, old, new, replace_all, &label) {
            Ok(a) => a,
            Err(e) => return ToolOutput::err(format!("Error: {}", e)),
        };
        let warning = match save_edit(ctx, &path, &text, &applied.text).await {
            Ok(w) => w,
            Err(e) => return e,
        };
        let note = applied.note.map(|n| format!(" ({})", n)).unwrap_or_default();
        ToolOutput::ok(with_warning(
            format!(
                "Replaced {} occurrence(s) in {}{}\n{}",
                if replace_all { applied.count } else { 1 },
                path.display(),
                note,
                snippet_around(&applied.text, new)
            ),
            warning,
        ))
    }
}

fn content_stamp(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

fn check_fresh(ctx: &ToolContext, path: &Path, current: &[u8]) -> Result<(), ToolOutput> {
    let seen = ctx.read_files.lock().unwrap().get(path).copied();
    match seen {
        Some(stamp) if stamp != content_stamp(current) => Err(ToolOutput::err(format!(
            "Error: {} changed on disk since you last read it (another command, a formatter or a person modified it). Read it again before editing.",
            path.display()
        ))),
        _ => Ok(()),
    }
}

async fn load_for_edit(ctx: &ToolContext, fp: &str) -> Result<(PathBuf, String), ToolOutput> {
    let path = canonicalize_lenient(&resolve_path(&ctx.cwd(), fp));
    if path.is_dir() {
        return Err(ToolOutput::err(format!("Error: {} is a directory", path.display())));
    }
    if !ctx.read_files.lock().unwrap().contains_key(&path) {
        return Err(ToolOutput::err(format!(
            "Error: {} has not been read in this session. Read it before editing.",
            path.display()
        )));
    }
    let text = match tokio::fs::read_to_string(&path).await {
        Ok(t) => t,
        Err(e) => return Err(ToolOutput::err(format!("Error reading {}: {}", path.display(), e))),
    };
    check_fresh(ctx, &path, text.as_bytes())?;
    Ok((path, text))
}

async fn save_edit(ctx: &ToolContext, path: &Path, old: &str, text: &str) -> Result<Option<String>, ToolOutput> {
    if let Err(msg) = crate::syntax_guard::guard_write(path, Some(old), text).await {
        return Err(ToolOutput::err(msg));
    }
    let ck_warning = checkpoint_before(ctx, path);
    let warning = match crate::atomic::write_atomic_async(path.to_path_buf(), text.as_bytes().to_vec()).await {
        Ok(w) => w,
        Err(e) => return Err(ToolOutput::err(format!("Error writing {}: {}", path.display(), e))),
    };
    checkpoint_after(ctx, path, text.as_bytes());
    ctx.read_files.lock().unwrap().insert(path.to_path_buf(), content_stamp(text.as_bytes()));
    Ok(match (warning, ck_warning) {
        (Some(w), Some(c)) => Some(format!("{w}\n{c}")),
        (w, c) => w.or(c),
    })
}

fn checkpoint_before(ctx: &ToolContext, path: &Path) -> Option<String> {
    ctx.checkpoints.as_ref().and_then(|c| c.before_write(path))
}

fn checkpoint_after(ctx: &ToolContext, path: &Path, content: &[u8]) {
    if let Some(c) = &ctx.checkpoints {
        c.after_write(path, content);
    }
}

fn with_warning(mut msg: String, warning: Option<String>) -> String {
    if let Some(w) = warning {
        msg.push('\n');
        msg.push_str(&w);
    }
    msg
}

pub struct MultiEditTool;

#[async_trait]
impl Tool for MultiEditTool {
    fn name(&self) -> &str {
        "MultiEdit"
    }
    fn description(&self) -> &str {
        "Apply several ordered string replacements to one file atomically; file must have been read first."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {"type": "string", "description": "File to edit."},
                "edits": {
                    "type": "array",
                    "description": "Replacements to apply in order.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_string": {"type": "string", "description": "Exact text to replace."},
                            "new_string": {"type": "string", "description": "Replacement text."},
                            "replace_all": {"type": "boolean", "description": "Replace every occurrence."}
                        },
                        "required": ["old_string", "new_string"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["file_path", "edits"],
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        false
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let Some(fp) = arg_str(&args, "file_path") else { return missing("file_path") };
        let Some(edits) = args.get("edits").and_then(|v| v.as_array()).filter(|e| !e.is_empty()) else {
            return missing("edits");
        };
        let (path, original) = match load_for_edit(ctx, fp).await {
            Ok(v) => v,
            Err(e) => return e,
        };
        let mut text = original.clone();
        let label = path.display().to_string();
        let mut total = 0;
        let mut notes = Vec::new();
        for (i, e) in edits.iter().enumerate() {
            let old = e.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
            let new = e.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
            let all = e.get("replace_all").and_then(|v| v.as_bool()).unwrap_or(false);
            match apply_edit(&text, old, new, all, &label) {
                Ok(a) => {
                    total += if all { a.count } else { 1 };
                    if let Some(n) = a.note {
                        notes.push(format!("edit {}: {}", i + 1, n));
                    }
                    text = a.text;
                }
                Err(msg) => {
                    return ToolOutput::err(format!(
                        "Error: edit {} of {} failed, so no edits were written: {}",
                        i + 1,
                        edits.len(),
                        msg
                    ))
                }
            }
        }
        let warning = match save_edit(ctx, &path, &original, &text).await {
            Ok(w) => w,
            Err(e) => return e,
        };
        let note = if notes.is_empty() { String::new() } else { format!(" ({})", notes.join("; ")) };
        ToolOutput::ok(with_warning(
            format!(
                "Applied {} edit(s), {} replacement(s), to {}{}",
                edits.len(),
                total,
                path.display(),
                note
            ),
            warning,
        ))
    }
}

fn snippet_around(text: &str, needle: &str) -> String {
    let Some(pos) = text.find(needle) else { return String::new() };
    let start = text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let start = text[..start].rfind('\n').map(|i| i + 1).unwrap_or(start);
    let end_off = pos + needle.len();
    let mut end = text[end_off..].find('\n').map(|i| end_off + i + 1).unwrap_or(text.len());
    if let Some(i) = text[end..].find('\n') {
        end += i + 1;
    }
    text[start..end.min(text.len())].to_string()
}

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "Bash"
    }
    fn description(&self) -> &str {
        "Run a shell command (persistent working directory); background:true runs it as a job (see JobOutput/JobKill)."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "Shell command to run with bash -c."},
                "timeout_ms": {"type": "integer", "description": "Kill the command after this many milliseconds (default 120000 or settings toolTimeouts.Bash, max 600000). Not applied to background jobs."},
                "description": {"type": "string", "description": "Short human-readable note about what the command does."},
                "background": {"type": "boolean", "description": "Run as a background job: returns a job id immediately; use JobOutput to read its output and JobKill to stop it. Use for servers, watchers and long builds/tests."},
                "run_in_background": {"type": "boolean", "description": "Same as background."}
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        false
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let Some(cmd_str) = arg_str(&args, "command") else { return missing("command") };
        let timeout_ms = args
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .or(ctx.tool_timeout_ms.filter(|t| *t > 0))
            .unwrap_or(BASH_DEFAULT_TIMEOUT_MS)
            .clamp(1, 600_000);
        let flag = |k: &str| args.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
        let background = flag("background") || flag("run_in_background");
        let cwd = ctx.cwd();

        let shell = zen_core::resolve_shell("bash");
        if background {

            return match crate::jobs::start_job(cmd_str, &cwd, &ctx.env, &ctx.scratch_dir(), ctx.api_key_env.as_deref()) {
                Ok(j) => ToolOutput::ok(format!(
                    "Started background job {} (pid {}). Output is logged to {}. Read new output with \
                     JobOutput {{\"job_id\":\"{}\"}} (add wait_ms to wait for it to finish); stop it with JobKill.",
                    j.id,
                    j.pid.unwrap_or(0),
                    j.log.display(),
                    j.id
                )),
                Err(e) => ToolOutput::err(format!("Error: {}", e)),
            };
        }

        let marker = "__ZEN_PWD_8f3a__";

        let wrapped = match shell.kind {
            zen_core::ShellKind::Posix => format!(
                "{{ {cmd_str}\n}} 2>&1\n__zen_exit=$?\nprintf '\\n{marker}%s\\n' \"$PWD\"\nexit $__zen_exit"
            ),
            zen_core::ShellKind::PowerShell => format!("& {{ {cmd_str} }} 2>&1; exit $LASTEXITCODE"),
            zen_core::ShellKind::Cmd => format!("({cmd_str}) 2>&1"),
        };
        let mut command = tokio::process::Command::new(&shell.program);
        command
            .args(shell.args(&wrapped))
            .current_dir(&cwd)
            .envs(ctx.env.iter())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        for name in zen_core::api_key_env_names(ctx.api_key_env.as_deref()) {
            command.env_remove(name);
        }

        let child = match command.spawn() {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(format!("Error: failed to start bash: {}", e)),
        };

        let fut = async {
            let mut child = child;
            let mut stdout_pipe = child.stdout.take();
            let read_out = async {
                let mut buf = Vec::new();
                if let Some(p) = stdout_pipe.as_mut() {
                    let _ = p.read_to_end(&mut buf).await;
                }
                buf
            };
            let (out_bytes, status) = tokio::join!(read_out, child.wait());
            (out_bytes, status)
        };

        match tokio::time::timeout(Duration::from_millis(timeout_ms), fut).await {
            Err(_) => ToolOutput::err(format!(
                "Error: command timed out after {} ms and was killed",
                timeout_ms
            )),
            Ok((out_bytes, status)) => {
                let status = match status {
                    Ok(s) => s,
                    Err(e) => return ToolOutput::err(format!("Error waiting for bash: {}", e)),
                };
                let mut combined = String::from_utf8_lossy(&out_bytes).to_string();

                if let Some(pos) = combined.rfind(&marker) {
                    let after = &combined[pos + marker.len()..];
                    let (new_pwd, rest) = match after.find('\n') {
                        Some(nl) => (&after[..nl], &after[nl + 1..]),
                        None => (after, ""),
                    };
                    let new_pwd = new_pwd.trim();
                    if !new_pwd.is_empty() {
                        let p = PathBuf::from(new_pwd);
                        if p.is_dir() {
                            *ctx.cwd.lock().unwrap() = p;
                        }
                    }
                    combined = format!("{}{}", &combined[..pos], rest);
                }

                let combined = combined.trim_end_matches('\n').to_string();
                let code = status.code().unwrap_or(-1);
                let body = spill_output(&combined, BASH_MAX_CHARS, ctx);
                if status.success() {
                    ToolOutput::ok(body)
                } else {
                    ToolOutput::err(format!("Exit code {}\n{}", code, body))
                }
            }
        }
    }
}

pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "Glob"
    }
    fn description(&self) -> &str {
        "Find files by glob pattern (e.g. '**/*.rs'); skips ignored dirs, newest first, max 1000 paths."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Glob pattern such as '**/*.rs' or '*.toml'."},
                "path": {"type": "string", "description": "Directory to search in (defaults to the working directory)."}
            },
            "required": ["pattern"],
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        true
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let Some(pattern) = arg_str(&args, "pattern") else { return missing("pattern") };
        let cwd = ctx.cwd();
        let base = match arg_str(&args, "path") {
            Some(p) => resolve_path(&cwd, p),
            None => cwd.clone(),
        };
        if !base.exists() {
            return ToolOutput::err(format!("Error: directory does not exist: {}", base.display()));
        }
        let matcher = match globset::GlobBuilder::new(pattern).literal_separator(true).build() {
            Ok(g) => g.compile_matcher(),
            Err(e) => return ToolOutput::err(format!("Error: invalid glob pattern: {}", e)),
        };
        let rules = load_ignore_rules(&base);
        let matches = glob_paths(&base, &matcher, &rules, !pattern.contains('/'), 1000);
        if matches.is_empty() {
            return ToolOutput::ok("No files matched");
        }
        ToolOutput::ok(
            matches
                .iter()
                .map(|(p, _)| p.to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }
}

type Matches = Vec<(PathBuf, std::time::SystemTime)>;

fn glob_paths(
    base: &Path,
    matcher: &globset::GlobMatcher,
    rules: &[crate::gitignore::IgnoreRule],
    match_file_name: bool,
    cap: usize,
) -> Matches {
    let mut out: Matches = Vec::new();
    let walker = walkdir::WalkDir::new(base).into_iter().filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        if name == ".git" || name == "target" || name == "node_modules" {
            return false;
        }
        if e.file_type().is_dir() {
            !is_ignored(rules, e.path(), true)
        } else {
            !is_ignored(rules, e.path(), false)
        }
    });
    for entry in walker.flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let full = entry.path();
        let rel = full.strip_prefix(base).unwrap_or(full);
        let file_name = Path::new(full.file_name().unwrap_or_default());
        let hit = matcher.is_match(rel) || (match_file_name && matcher.is_match(file_name));
        if !hit {
            continue;
        }
        let mtime = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        out.push((rel.to_path_buf(), mtime));
    }
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out.truncate(cap);
    out
}

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "Grep"
    }
    fn description(&self) -> &str {
        "Search file contents with a regex (ripgrep if installed); modes: files_with_matches, content, count."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Regular expression to search for."},
                "path": {"type": "string", "description": "File or directory to search (defaults to the working directory)."},
                "glob": {"type": "string", "description": "Only search files matching this glob, e.g. '*.rs'."},
                "type": {"type": "string", "description": "Only search files of this ripgrep file type, e.g. 'rust'."},
                "output_mode": {"type": "string", "enum": ["files_with_matches", "content", "count"], "description": "Result style (default files_with_matches)."},
                "-i": {"type": "boolean", "description": "Case-insensitive search."},
                "-n": {"type": "boolean", "description": "Prefix content matches with line numbers."},
                "-A": {"type": "integer", "description": "Lines of trailing context."},
                "-B": {"type": "integer", "description": "Lines of leading context."},
                "-C": {"type": "integer", "description": "Lines of context on both sides."},
                "head_limit": {"type": "integer", "description": "Maximum number of result lines (default 250)."}
            },
            "required": ["pattern"],
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        true
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let Some(pattern) = arg_str(&args, "pattern") else { return missing("pattern") };
        let cwd = ctx.cwd();
        let target = match arg_str(&args, "path") {
            Some(p) => resolve_path(&cwd, p),
            None => cwd.clone(),
        };
        if !target.exists() {
            return ToolOutput::err(format!("Error: path does not exist: {}", target.display()));
        }
        let glob = arg_str(&args, "glob").map(|s| s.to_string());
        let ftype = arg_str(&args, "type").map(|s| s.to_string());
        let mode = arg_str(&args, "output_mode").unwrap_or("files_with_matches").to_string();
        let case_insensitive = args.get("-i").and_then(|v| v.as_bool()).unwrap_or(false);
        let line_numbers = args.get("-n").and_then(|v| v.as_bool()).unwrap_or(false);
        let mut after = args.get("-A").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let mut before = args.get("-B").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        if let Some(c) = args.get("-C").and_then(|v| v.as_u64()) {
            after = c as usize;
            before = c as usize;
        }
        let head_limit = args
            .get("head_limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(250)
            .max(1) as usize;

        if !matches!(mode.as_str(), "files_with_matches" | "content" | "count") {
            return ToolOutput::err(format!(
                "Error: output_mode must be files_with_matches, content or count (got '{}')",
                mode
            ));
        }

        let (pattern, literal_note) = match regex::Regex::new(pattern) {
            Ok(_) => (pattern.to_string(), None),
            Err(e) => (regex::escape(pattern), Some(literal_fallback_note(pattern, &e.to_string()))),
        };

        let with_note = |s: String| -> ToolOutput {
            match &literal_note {
                Some(n) if s.trim().is_empty() || s.trim() == "No matches found" => {
                    ToolOutput::err(format!("Error: invalid regex, and no literal matches either. {}", n))
                }
                Some(n) => ToolOutput::ok(format!("{}\n{}", n, s)),
                None => ToolOutput::ok(s),
            }
        };
        if rg_available() {
            match run_rg(&target, &cwd, &pattern, &mode, case_insensitive, line_numbers, before, after, glob.as_deref(), ftype.as_deref(), head_limit).await {
                Ok(s) => return with_note(s),

                Err(RgError::BadPattern(msg)) => return ToolOutput::err(msg),
                Err(RgError::Spawn) => {}
            }
        }
        let builtin = regex::RegexBuilder::new(&pattern)
            .case_insensitive(case_insensitive)
            .build();
        let re = match builtin {
            Ok(r) => r,
            Err(e) => return ToolOutput::err(format!("Error: invalid regex: {}", e)),
        };
        with_note(search_builtin(
            &target,
            &cwd,
            &re,
            &mode,
            line_numbers,
            before,
            after,
            glob.as_deref(),
            head_limit,
        ))
    }
}

fn literal_fallback_note(pattern: &str, err: &str) -> String {
    let first = err.lines().rev().find(|l| l.trim_start().starts_with("error:")).unwrap_or("").trim();
    let why = first.strip_prefix("error:").unwrap_or(first).trim();
    if why.is_empty() {
        format!("[zen: '{}' is not a valid regex; searched for it as literal text]", pattern)
    } else {
        format!("[zen: '{}' is not a valid regex ({}); searched for it as literal text]", pattern, why)
    }
}

enum RgError {
    Spawn,
    BadPattern(String),
}

fn rg_available() -> bool {
    use std::sync::OnceLock;
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        std::process::Command::new("rg")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

#[allow(clippy::too_many_arguments)]
async fn run_rg(
    target: &Path,
    cwd: &Path,
    pattern: &str,
    mode: &str,
    case_insensitive: bool,
    line_numbers: bool,
    before: usize,
    after: usize,
    glob: Option<&str>,
    ftype: Option<&str>,
    head_limit: usize,
) -> Result<String, RgError> {
    let mut cmd = tokio::process::Command::new("rg");
    cmd.arg("--color").arg("never");
    if case_insensitive {
        cmd.arg("-i");
    }
    if glob.is_some() || ftype.is_some() {
        cmd.arg("--no-ignore-vcs");
    }
    match mode {
        "files_with_matches" => {
            cmd.arg("-l");
        }
        "count" => {
            cmd.arg("-c");
        }
        _ => {
            if line_numbers {
                cmd.arg("--line-number");
            }
            if before > 0 {
                cmd.arg("-B").arg(before.to_string());
            }
            if after > 0 {
                cmd.arg("-A").arg(after.to_string());
            }
        }
    }
    if let Some(g) = glob {
        cmd.arg("-g").arg(g);
    }
    if let Some(t) = ftype {
        cmd.arg("-t").arg(t);
    }
    cmd.arg("--").arg(pattern).arg(target);
    cmd.current_dir(cwd);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);

    let out = tokio::time::timeout(Duration::from_secs(60), cmd.output())
        .await
        .map_err(|_| RgError::Spawn)?
        .map_err(|_| RgError::Spawn)?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    if out.status.code() == Some(2) {
        let msg = stderr.trim();
        if msg.contains("regex") || msg.contains("pattern") {
            return Err(RgError::BadPattern(format!("Error: invalid regex: {}", msg)));
        }
    }
    Ok(truncate_lines(&normalize_paths(&stdout, target, cwd), head_limit))
}

fn normalize_paths(text: &str, target: &Path, cwd: &Path) -> String {
    if !target.is_absolute() {
        return text.to_string();
    }
    let prefix = format!("{}/", target.display());
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let rel = line
            .strip_prefix(&prefix)
            .map(|rest| {
                let base = target.strip_prefix(cwd).unwrap_or(target);
                if base.as_os_str().is_empty() {
                    rest.to_string()
                } else {
                    format!("{}/{}", base.display(), rest)
                }
            })
            .unwrap_or_else(|| line.to_string());
        out.push_str(&rel);
        out.push('\n');
    }
    out
}

fn truncate_lines(text: &str, head_limit: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= head_limit {
        return text.to_string();
    }
    let mut out = lines[..head_limit].join("\n");
    out.push_str(&format!(
        "\n... [{} more matching lines not shown]",
        lines.len() - head_limit
    ));
    out
}

#[allow(clippy::too_many_arguments)]
pub fn search_builtin(
    target: &Path,
    cwd: &Path,
    re: &Regex,
    mode: &str,
    line_numbers: bool,
    before: usize,
    after: usize,
    glob: Option<&str>,
    head_limit: usize,
) -> String {
    let glob_matcher = glob.and_then(|g| globset::GlobBuilder::new(g).build().ok().map(|g| g.compile_matcher()));
    let base = if target.is_dir() { target } else { target.parent().unwrap_or(cwd) };
    let rules = load_ignore_rules(base);

    let mut files: Vec<PathBuf> = Vec::new();
    if target.is_file() {
        files.push(target.to_path_buf());
    } else {
        let walker = walkdir::WalkDir::new(target).into_iter().filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            if name == ".git" || name == "target" || name == "node_modules" {
                return false;
            }
            if e.file_type().is_dir() {
                !is_ignored(&rules, e.path(), true)
            } else {
                !is_ignored(&rules, e.path(), false)
            }
        });
        for entry in walker.flatten() {
            if !entry.file_type().is_file() {
                continue;
            }
            if let Some(m) = &glob_matcher {
                let rel = entry.path().strip_prefix(base).unwrap_or(entry.path());
                let name = Path::new(entry.file_name());
                if !m.is_match(rel) && !m.is_match(name) {
                    continue;
                }
            }
            files.push(entry.path().to_path_buf());
        }
        files.sort();
    }

    let display = |p: &Path| -> String {
        p.strip_prefix(cwd).unwrap_or(p).to_string_lossy().to_string()
    };

    let mut out_lines: Vec<String> = Vec::new();
    let mut total_matches = 0usize;
    for file in &files {
        let Ok(content) = std::fs::read(file) else { continue };
        if content.contains(&0u8) {
            continue;
        }
        let text = String::from_utf8_lossy(&content);
        let lines: Vec<&str> = text.lines().collect();
        let mut hits: Vec<usize> = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            if re.is_match(line) {
                hits.push(i);
            }
        }
        if hits.is_empty() {
            continue;
        }
        total_matches += hits.len();
        match mode {
            "files_with_matches" => out_lines.push(display(file)),
            "count" => out_lines.push(format!("{}:{}", display(file), hits.len())),
            _ => {
                let mut shown = vec![false; lines.len()];
                for &h in &hits {
                    let lo = h.saturating_sub(before);
                    let hi = (h + after + 1).min(lines.len());
                    for s in shown.iter_mut().take(hi).skip(lo) {
                        *s = true;
                    }
                }
                for (i, line) in lines.iter().enumerate() {
                    if !shown[i] {
                        continue;
                    }
                    let is_hit = hits.contains(&i);
                    let sep = if is_hit { ':' } else { '-' };
                    if line_numbers {
                        out_lines.push(format!("{}{}{}{}{}", display(file), sep, i + 1, sep, line));
                    } else if is_hit {
                        out_lines.push(format!("{}:{}", display(file), line));
                    } else {
                        out_lines.push(format!("{}-{}", display(file), line));
                    }
                }
            }
        }
        if out_lines.len() >= head_limit {
            break;
        }
    }

    if out_lines.is_empty() {
        return "No matches found".to_string();
    }
    let shown = out_lines.len();
    let mut text = out_lines[..shown.min(head_limit)].join("\n");
    if mode == "content" && total_matches > 0 && shown >= head_limit {
        text.push_str(&format!("\n... [truncated at {} lines]", head_limit));
    }
    text
}

pub struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "WebFetch"
    }
    fn description(&self) -> &str {
        "Fetch a URL and return it as readable text (follows redirects, 10s timeout, 5MB cap)."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "Absolute http(s) URL to fetch."},
                "prompt": {"type": "string", "description": "What to look for on the page; recorded as a focus hint in the tool result."}
            },
            "required": ["url"],
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        false
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let Some(url) = arg_str(&args, "url") else { return missing("url") };
        let prompt = arg_str(&args, "prompt").unwrap_or("").to_string();
        let url = upgrade_scheme(url);
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return ToolOutput::err(format!("Error: unsupported URL '{}'", url));
        }
        let allow_private = ctx.allow_private_network;

        let client = match with_extra_roots(reqwest::Client::builder())
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("ZenCode/0.1 (+https://example.invalid/zen)")
            .build()
        {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(format!("Error: could not build HTTP client: {}", e)),
        };

        let mut url = url;
        let mut hops = 0u32;
        let resp = loop {
            if let Err(e) = check_fetch_target(&url, allow_private).await {
                return ToolOutput::err(e);
            }
            let r = match client.get(&url).send().await {
                Ok(r) => r,
                Err(e) => return ToolOutput::err(format!("Error fetching {}: {}", url, e)),
            };
            if !r.status().is_redirection() {
                break r;
            }
            hops += 1;
            if hops > 10 {
                return ToolOutput::err(format!("Error: too many redirects fetching {}", url));
            }
            let loc = r.headers().get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            if loc.is_empty() {
                break r;
            }
            url = match join_redirect_url(&url, &loc) {
                Ok(u) => u,
                Err(e) => return ToolOutput::err(e),
            };
        };
        let status = resp.status();
        if !status.is_success() {
            return ToolOutput::err(format!("Error: {} returned HTTP {}", url, status.as_u16()));
        }
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = match read_capped(resp, WEBFETCH_MAX_BYTES).await {
            Ok(b) => b,
            Err(e) => return ToolOutput::err(format!("Error reading {}: {}", url, e)),
        };
        let raw = String::from_utf8_lossy(&bytes).to_string();
        let is_html = content_type.contains("html") || looks_like_html(&raw);
        let text = if is_html {
            html_to_text(&raw)
        } else {
            raw
        };
        let body = truncate_chars(&text, WEBFETCH_MAX_CHARS);
        let mut out = format!("URL: {}\nContent-Type: {}\n\n", url, content_type);
        if !prompt.is_empty() {
            out.push_str(&format!("(focus requested: {})\n\n", prompt));
        }
        out.push_str(&body);
        ToolOutput::ok(out)
    }
}

pub const WEBFETCH_MAX_BYTES: usize = 5 * 1024 * 1024;

pub const WEBFETCH_MAX_CHARS: usize = 100_000;

fn upgrade_scheme(url: &str) -> String {
    let t = url.trim();
    if let Some(rest) = t.strip_prefix("http://") {
        return format!("https://{}", rest);
    }
    t.to_string()
}

fn is_public_ip(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()

                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0b1100_0000) == 0b0100_0000))
        }
        IpAddr::V6(v6) => {

            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(v4));
            }

            let is_doc = v6.segments()[0] == 0x2001 && v6.segments()[1] == 0x0db8;
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
                || is_doc)
        }
    }
}

fn url_host_port(url: &str) -> (String, u16) {
    let default_port = if url.starts_with("https://") { 443 } else { 80 };
    let after_scheme = url.split("://").nth(1).unwrap_or(url);
    let host_port = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = host_port.split('@').next_back().unwrap_or(host_port);
    if let Some(rest) = host_port.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            let host = &rest[..end];
            let port = rest[end + 1..]
                .strip_prefix(':')
                .and_then(|p| p.parse::<u16>().ok())
                .unwrap_or(default_port);
            return (host.to_string(), port);
        }
    }
    match host_port.rsplit_once(':') {
        Some((h, p)) if p.parse::<u16>().is_ok() => (h.to_string(), p.parse().unwrap_or(default_port)),
        _ => (host_port.to_string(), default_port),
    }
}

fn join_redirect_url(base: &str, loc: &str) -> Result<String, String> {
    let base_url = reqwest::Url::parse(base)
        .map_err(|e| format!("Error: cannot parse URL '{}': {}", base, e))?;
    base_url
        .join(loc)
        .map(|u| u.to_string())
        .map_err(|e| format!("Error: bad redirect target '{}': {}", loc, e))
}

async fn check_fetch_target(url: &str, allow_private: bool) -> Result<(), String> {
    if allow_private {
        return Ok(());
    }
    let (host, port) = url_host_port(url);
    if host.is_empty() {
        return Err(format!("Error: cannot parse host from URL '{}'", url));
    }

    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if !is_public_ip(ip) {
            return Err(format!(
                "Error: refusing to fetch non-public address '{}' (set allowPrivateNetwork to allow)",
                host
            ));
        }
        return Ok(());
    }

    let addrs = tokio::net::lookup_host(format!("{}:{}", host, port))
        .await
        .map_err(|e| format!("Error: cannot resolve host '{}': {}", host, e))?;
    let mut any = false;
    for addr in addrs {
        any = true;
        if !is_public_ip(addr.ip()) {
            return Err(format!(
                "Error: refusing to fetch '{}' (resolves to non-public address {}; set allowPrivateNetwork to allow)",
                host,
                addr.ip()
            ));
        }
    }
    if !any {
        return Err(format!("Error: host '{}' resolved to no addresses", host));
    }
    Ok(())
}

fn looks_like_html(s: &str) -> bool {
    let head = s.trim_start().chars().take(200).collect::<String>().to_lowercase();
    head.starts_with("<!doctype html") || head.starts_with("<html") || head.contains("<head>")
}

async fn read_capped(resp: reqwest::Response, cap: usize) -> anyhow::Result<Vec<u8>> {
    let mut stream = resp;
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.chunk().await? {
        if buf.len() + chunk.len() > cap {
            let room = cap.saturating_sub(buf.len());
            buf.extend_from_slice(&chunk[..room]);
            break;
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

fn with_extra_roots(mut b: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    if let Some(pem) = zen_core::extra_root_pem() {
        if let Ok(certs) = reqwest::Certificate::from_pem_bundle(&pem) {
            for c in certs {
                b = b.add_root_certificate(c);
            }
        }
    }
    b
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push_str("\n... [truncated]");
    out
}

fn strip_tag(html: &str, tag: &str) -> String {
    let re = Regex::new(&format!(r"(?is)<{0}\b[^>]*>.*?</{0}\s*>", tag)).unwrap();
    re.replace_all(html, " ").to_string()
}

fn decode_entities(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

pub fn html_to_text(html: &str) -> String {
    let mut s = html.to_string();

    let comment = Regex::new(r"(?s)<!--.*?-->").unwrap();
    s = comment.replace_all(&s, " ").to_string();
    for tag in ["script", "style", "noscript", "svg", "head", "template"] {
        s = strip_tag(&s, tag);
    }

    let block = Regex::new(
        r"(?i)</?(p|div|br|li|ul|ol|tr|table|thead|tbody|h[1-6]|section|article|header|footer|nav|pre|blockquote|hr)\b[^>]*>",
    )
    .unwrap();
    s = block.replace_all(&s, "\n").to_string();
    let tags = Regex::new(r"(?s)<[^>]*>").unwrap();
    s = tags.replace_all(&s, " ").to_string();
    s = decode_entities(&s);

    let mut out = String::with_capacity(s.len());
    let mut blank = 0;
    for line in s.lines() {
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            blank += 1;
            if blank <= 1 && !out.is_empty() {
                out.push('\n');
            }
        } else {
            blank = 0;
            out.push_str(&collapsed);
            out.push('\n');
        }
    }
    out.trim().to_string()
}

pub struct TodoWriteTool;

#[async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &str {
        "TodoWrite"
    }
    fn description(&self) -> &str {
        "Replace the session task list; keep exactly one item in_progress."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "The complete task list (this replaces the previous one).",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": {"type": "string", "description": "What needs to be done."},
                            "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]},
                            "activeForm": {"type": "string", "description": "Present-tense wording shown while the item is in progress."}
                        },
                        "required": ["content", "status"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["todos"],
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        true
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let Some(arr) = args.get("todos").and_then(|v| v.as_array()) else {
            return missing("todos");
        };
        let mut items = Vec::new();
        for (i, raw) in arr.iter().enumerate() {
            let content = raw
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if content.is_empty() {
                return ToolOutput::err(format!("Error: todos[{}] is missing 'content'", i));
            }
            let status = raw
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("pending")
                .to_string();
            if !matches!(status.as_str(), "pending" | "in_progress" | "completed") {
                return ToolOutput::err(format!(
                    "Error: todos[{}] has invalid status '{}' (expected pending, in_progress or completed)",
                    i, status
                ));
            }
            let active_form = raw
                .get("activeForm")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            items.push(TodoItem { content, status, active_form });
        }
        *ctx.todos.lock().unwrap() = items.clone();
        ToolOutput::ok(render_todos(&items))
    }
}

pub fn render_todos(items: &[TodoItem]) -> String {
    if items.is_empty() {
        return "Todo list cleared (0 items).".to_string();
    }
    let mut out = String::new();
    for item in items {
        let (mark, label) = match item.status.as_str() {
            "completed" => ("[x]", item.content.clone()),
            "in_progress" => (
                "[>]",
                item.active_form.clone().unwrap_or_else(|| item.content.clone()),
            ),
            _ => ("[ ]", item.content.clone()),
        };
        out.push_str(&format!("{} {}\n", mark, label));
    }
    let done = items.iter().filter(|i| i.status == "completed").count();
    out.push_str(&format!(
        "\n{} of {} done, {} in progress.",
        done,
        items.len(),
        items.iter().filter(|i| i.status == "in_progress").count()
    ));
    out
}

pub fn builtin_tools() -> Vec<std::sync::Arc<dyn Tool>> {
    vec![
        std::sync::Arc::new(ReadTool),
        std::sync::Arc::new(WriteTool),
        std::sync::Arc::new(EditTool),
        std::sync::Arc::new(MultiEditTool),
        std::sync::Arc::new(BashTool),
        std::sync::Arc::new(GlobTool),
        std::sync::Arc::new(GrepTool),
        std::sync::Arc::new(WebFetchTool),
        std::sync::Arc::new(TodoWriteTool),
    ]
}

#[cfg(test)]
mod tests {
    #[test]
    fn ssrf_guard_rejects_ipv4_mapped_ipv6() {
        use std::net::IpAddr;
        assert!(!super::is_public_ip("::ffff:127.0.0.1".parse::<IpAddr>().unwrap()));
        assert!(!super::is_public_ip("::ffff:10.0.0.1".parse::<IpAddr>().unwrap()));
        assert!(!super::is_public_ip("::ffff:169.254.169.254".parse::<IpAddr>().unwrap()));
        assert!(super::is_public_ip("::ffff:8.8.8.8".parse::<IpAddr>().unwrap()));
    }

    use super::*;
    use serde_json::json;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zen-tools-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ctx_for(dir: &Path) -> ToolContext {
        ToolContext::new(dir.to_path_buf(), "test-session")
    }

    #[tokio::test]
    async fn read_numbers_lines_and_pages() {
        let dir = tmpdir("read");
        let file = dir.join("a.txt");
        std::fs::write(&file, "alpha\nbeta\ngamma\ndelta\n").unwrap();
        let ctx = ctx_for(&dir);

        let out = ReadTool
            .call(json!({"file_path": "a.txt", "offset": 2, "limit": 2}), &ctx)
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("     2\tbeta"), "{}", out.content);
        assert!(out.content.contains("     3\tgamma"), "{}", out.content);
        assert!(!out.content.contains("alpha"));
        assert!(!out.content.contains("delta"));

        let canonical = canonicalize_lenient(&file);
        assert!(ctx.read_files.lock().unwrap().contains_key(&canonical));

        let err = ReadTool.call(json!({"file_path": "missing.txt"}), &ctx).await;
        assert!(err.is_error);
        assert!(err.content.contains("missing.txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn write_requires_prior_read_for_existing_file() {
        let dir = tmpdir("write");
        let existing = dir.join("e.txt");
        std::fs::write(&existing, "old").unwrap();
        let ctx = ctx_for(&dir);

        let denied = WriteTool.call(json!({"file_path": "e.txt", "content": "new"}), &ctx).await;
        assert!(denied.is_error);
        assert!(denied.content.contains("has not been read"), "{}", denied.content);
        assert_eq!(std::fs::read_to_string(&existing).unwrap(), "old");

        ReadTool.call(json!({"file_path": "e.txt"}), &ctx).await;
        let ok = WriteTool.call(json!({"file_path": "e.txt", "content": "new"}), &ctx).await;
        assert!(!ok.is_error, "{}", ok.content);
        assert_eq!(std::fs::read_to_string(&existing).unwrap(), "new");

        let created = WriteTool
            .call(json!({"file_path": "deep/nested/f.txt", "content": "hi"}), &ctx)
            .await;
        assert!(!created.is_error, "{}", created.content);
        assert_eq!(std::fs::read_to_string(dir.join("deep/nested/f.txt")).unwrap(), "hi");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn edit_unique_replace_all_and_not_found() {
        let dir = tmpdir("edit");
        let file = dir.join("c.txt");
        std::fs::write(&file, "one\ntwo\none\n").unwrap();
        let ctx = ctx_for(&dir);
        ReadTool.call(json!({"file_path": "c.txt"}), &ctx).await;

        let nf = EditTool
            .call(json!({"file_path": "c.txt", "old_string": "zzz", "new_string": "x"}), &ctx)
            .await;
        assert!(nf.is_error);
        assert!(nf.content.contains("not found"), "{}", nf.content);

        let amb = EditTool
            .call(json!({"file_path": "c.txt", "old_string": "one", "new_string": "ONE"}), &ctx)
            .await;
        assert!(amb.is_error);
        assert!(amb.content.contains("occurs 2 times"), "{}", amb.content);

        let all = EditTool
            .call(
                json!({"file_path": "c.txt", "old_string": "one", "new_string": "ONE", "replace_all": true}),
                &ctx,
            )
            .await;
        assert!(!all.is_error, "{}", all.content);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "ONE\ntwo\nONE\n");

        let one = EditTool
            .call(json!({"file_path": "c.txt", "old_string": "two", "new_string": "TWO"}), &ctx)
            .await;
        assert!(!one.is_error, "{}", one.content);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "ONE\nTWO\nONE\n");

        let same = EditTool
            .call(json!({"file_path": "c.txt", "old_string": "TWO", "new_string": "TWO"}), &ctx)
            .await;
        assert!(same.is_error);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn edit_requires_prior_read() {
        let dir = tmpdir("edit-noread");
        std::fs::write(dir.join("x.txt"), "hello").unwrap();
        let ctx = ctx_for(&dir);
        let out = EditTool
            .call(json!({"file_path": "x.txt", "old_string": "hello", "new_string": "bye"}), &ctx)
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("has not been read"), "{}", out.content);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn bash_reports_exit_code_and_combines_output() {
        let dir = tmpdir("bash-out");
        let ctx = ctx_for(&dir);
        let ok = BashTool.call(json!({"command": "echo out; echo err 1>&2"}), &ctx).await;
        assert!(!ok.is_error, "{}", ok.content);
        assert!(ok.content.contains("out"));
        assert!(ok.content.contains("err"));

        let bad = BashTool.call(json!({"command": "echo boom 1>&2; exit 3"}), &ctx).await;
        assert!(bad.is_error);
        assert!(bad.content.contains("Exit code 3"), "{}", bad.content);
        assert!(bad.content.contains("boom"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn bash_cd_persists_between_calls() {
        let dir = tmpdir("bash-cd");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let ctx = ctx_for(&dir);

        let first = tokio::time::timeout(
            Duration::from_secs(10),
            BashTool.call(json!({"command": "mkdir -p deeper && cd deeper"}), &ctx),
        )
        .await
        .expect("no hang");
        assert!(!first.is_error, "{}", first.content);
        assert_eq!(ctx.cwd(), canonicalize_lenient(&dir.join("deeper")));

        let second = tokio::time::timeout(
            Duration::from_secs(10),
            BashTool.call(json!({"command": "pwd"}), &ctx),
        )
        .await
        .expect("no hang");
        assert!(second.content.contains("deeper"), "{}", second.content);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn bash_timeout_kills_the_command() {
        let dir = tmpdir("bash-timeout");
        let ctx = ctx_for(&dir);
        let t0 = std::time::Instant::now();
        let out = tokio::time::timeout(
            Duration::from_secs(10),
            BashTool.call(json!({"command": "sleep 30", "timeout_ms": 300}), &ctx),
        )
        .await
        .expect("tool returned instead of hanging");
        assert!(out.is_error);
        assert!(out.content.contains("timed out"), "{}", out.content);
        assert!(t0.elapsed() < Duration::from_secs(5));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn bash_truncates_middle_output() {
        let dir = tmpdir("bash-trunc");
        let ctx = ctx_for(&dir);
        let out = BashTool
            .call(json!({"command": "seq 1 40000"}), &ctx)
            .await;

        assert!(out.content.contains("output too long") && out.content.contains("Full output saved to"), "no spill notice");
        assert!(out.content.chars().count() <= BASH_MAX_CHARS + 400);

        assert!(out.content.starts_with('1'), "{}", &out.content[..20.min(out.content.len())]);
        assert!(out.content.trim_end().ends_with("40000"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn truncate_middle_keeps_head_and_tail() {
        let s: String = (1..=1000).map(|i| format!("line{}\n", i)).collect();
        let t = truncate_middle(&s, 100);
        assert!(t.chars().count() < s.chars().count());
        assert!(t.starts_with("line1\n"));
        assert!(t.trim_end().ends_with("line1000"));
        assert!(t.contains("truncated"));
        assert_eq!(truncate_middle("short", 100), "short");
    }

    #[tokio::test]
    async fn glob_sorts_by_mtime_desc_and_skips_ignored() {
        let dir = tmpdir("glob");
        std::fs::write(dir.join("old.rs"), "old").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(dir.join("new.rs"), "new").unwrap();
        std::fs::write(dir.join("skip.txt"), "x").unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("target/nope.rs"), "x").unwrap();
        std::fs::create_dir_all(dir.join("node_modules")).unwrap();
        std::fs::write(dir.join("node_modules/nope.rs"), "x").unwrap();
        std::fs::write(dir.join(".gitignore"), "ignored.rs\n").unwrap();
        std::fs::write(dir.join("ignored.rs"), "x").unwrap();
        let ctx = ctx_for(&dir);

        let out = GlobTool.call(json!({"pattern": "**/*.rs"}), &ctx).await;
        assert!(!out.is_error, "{}", out.content);
        let lines: Vec<&str> = out.content.lines().collect();
        assert_eq!(lines, vec!["new.rs", "old.rs"], "got: {}", out.content);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn grep_three_modes_builtin_walker() {
        let dir = tmpdir("grep");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn alpha() {}\nfn beta() {}\n").unwrap();
        std::fs::write(dir.join("src/b.rs"), "// nothing here\n").unwrap();
        std::fs::write(dir.join("src/c.txt"), "alpha again\n").unwrap();
        let ctx = ctx_for(&dir);
        let re = Regex::new("alpha").unwrap();

        let files = search_builtin(
            &dir,
            &dir,
            &re,
            "files_with_matches",
            false,
            0,
            0,
            Some("*.rs"),
            100,
        ).replace('\\', "/");
        assert!(files.contains("src/a.rs"), "{}", files);
        assert!(!files.contains("src/c.txt"), "{}", files);
        assert!(!files.contains("src/b.rs"), "{}", files);

        let content = search_builtin(&dir, &dir, &re, "content", true, 0, 0, None, 100).replace('\\', "/");
        assert!(content.contains("src/a.rs:1:fn alpha() {}"), "{}", content);

        let counts = search_builtin(&dir, &dir, &re, "count", false, 0, 0, None, 100).replace('\\', "/");
        assert!(counts.contains("src/a.rs:1"), "{}", counts);
        assert!(counts.contains("src/c.txt:1"), "{}", counts);

        let ci = GrepTool
            .call(json!({"pattern": "ALPHA", "-i": true, "glob": "*.rs"}), &ctx)
            .await;
        assert!(!ci.is_error, "{}", ci.content);
        assert!(ci.content.contains("a.rs"), "{}", ci.content);

        let counted = GrepTool
            .call(json!({"pattern": "alpha", "output_mode": "count"}), &ctx)
            .await;
        assert!(counted.content.contains(":"), "{}", counted.content);

        let none = search_builtin(&dir, &dir, &Regex::new("zzzznope").unwrap(), "content", false, 0, 0, None, 100);
        assert_eq!(none, "No matches found");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn grep_invalid_regex_is_an_error() {
        let dir = tmpdir("grep-bad");
        let ctx = ctx_for(&dir);
        let out = GrepTool.call(json!({"pattern": "(unclosed"}), &ctx).await;
        assert!(out.is_error);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn html_to_text_strips_markup_and_scripts() {
        let html = r#"<html><head><title>T</title><style>.x{color:red}</style>
            </head><body><script>alert('bad')</script>
            <h1>Heading</h1><p>Hello&nbsp;world &amp; friends</p>
            <!-- comment --><div>Second   line</div></body></html>"#;
        let text = html_to_text(html);
        assert!(text.contains("Heading"), "{}", text);
        assert!(text.contains("Hello world & friends"), "{}", text);
        assert!(text.contains("Second line"), "{}", text);
        assert!(!text.contains("alert"), "{}", text);
        assert!(!text.contains("color:red"), "{}", text);
        assert!(!text.contains("<!--"), "{}", text);
        assert!(!text.contains("<p>"), "{}", text);
    }

    #[test]
    fn scheme_upgrade_and_truncate() {
        assert_eq!(upgrade_scheme("http://example.com/x"), "https://example.com/x");
        assert_eq!(upgrade_scheme("https://example.com"), "https://example.com");
        assert!(looks_like_html("<!DOCTYPE html><p>x</p>"));
        assert!(!looks_like_html("{\"json\":true}"));
        assert!(truncate_chars("abcdef", 3).starts_with("abc"));
        assert_eq!(truncate_chars("abc", 10), "abc");
    }

    #[tokio::test]
    async fn todo_write_replaces_list_and_renders() {
        let dir = tmpdir("todo");
        let ctx = ctx_for(&dir);
        let out = TodoWriteTool
            .call(
                json!({"todos": [
                    {"content": "write code", "status": "in_progress", "activeForm": "writing code"},
                    {"content": "run tests", "status": "pending"},
                    {"content": "ship it", "status": "completed"}
                ]}),
                &ctx,
            )
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("[>] writing code"), "{}", out.content);
        assert!(out.content.contains("[ ] run tests"), "{}", out.content);
        assert!(out.content.contains("[x] ship it"), "{}", out.content);
        assert!(out.content.contains("1 of 3 done"), "{}", out.content);
        assert_eq!(ctx.todos.lock().unwrap().len(), 3);

        TodoWriteTool
            .call(json!({"todos": [{"content": "only", "status": "pending"}]}), &ctx)
            .await;
        assert_eq!(ctx.todos.lock().unwrap().len(), 1);
        assert_eq!(ctx.todos.lock().unwrap()[0].content, "only");

        let bad = TodoWriteTool
            .call(json!({"todos": [{"content": "x", "status": "bogus"}]}), &ctx)
            .await;
        assert!(bad.is_error);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn builtin_tools_cover_the_spec_list() {
        let names: Vec<String> = builtin_tools().iter().map(|t| t.name().to_string()).collect();
        assert_eq!(
            names,
            vec!["Read", "Write", "Edit", "MultiEdit", "Bash", "Glob", "Grep", "WebFetch", "TodoWrite"]
        );
        let tools = builtin_tools();
        for t in &tools {
            assert_eq!(t.parameters_schema()["type"], "object");
            assert!(!t.description().is_empty());
        }
        let readonly = |n: &str| tools.iter().find(|t| t.name() == n).unwrap().is_read_only();
        assert!(readonly("Read") && readonly("Glob") && readonly("Grep") && readonly("TodoWrite"));
        assert!(!readonly("Write") && !readonly("Edit") && !readonly("MultiEdit") && !readonly("Bash") && !readonly("WebFetch"));
    }

    #[tokio::test]
    async fn r7_edit_and_write_refuse_a_file_changed_since_read() {
        let dir = tmpdir("stale");
        let file = dir.join("s.txt");
        std::fs::write(&file, "hello\n").unwrap();
        let ctx = ctx_for(&dir);
        ReadTool.call(json!({"file_path": "s.txt"}), &ctx).await;
        std::fs::write(&file, "hello world\n").unwrap();

        let e = EditTool
            .call(json!({"file_path": "s.txt", "old_string": "hello", "new_string": "bye"}), &ctx)
            .await;
        assert!(e.is_error && e.content.contains("changed on disk"), "{}", e.content);
        let w = WriteTool.call(json!({"file_path": "s.txt", "content": "x"}), &ctx).await;
        assert!(w.is_error && w.content.contains("changed on disk"), "{}", w.content);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hello world\n");

        ReadTool.call(json!({"file_path": "s.txt"}), &ctx).await;
        for (o, n) in [("hello", "bye"), ("world", "moon")] {
            let ok = EditTool.call(json!({"file_path": "s.txt", "old_string": o, "new_string": n}), &ctx).await;
            assert!(!ok.is_error, "{}", ok.content);
        }
        let ok = WriteTool.call(json!({"file_path": "s.txt", "content": "final\n"}), &ctx).await;
        assert!(!ok.is_error, "{}", ok.content);
        let ok = EditTool.call(json!({"file_path": "s.txt", "old_string": "final", "new_string": "done"}), &ctx).await;
        assert!(!ok.is_error, "{}", ok.content);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "done\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn r7_edit_indent_fallback_and_closest_match_on_miss() {
        let dir = tmpdir("fuzzy");
        let file = dir.join("m.py");
        std::fs::write(&file, "def f():\n\tif x:\n\t\treturn 1\n\treturn 0\n").unwrap();
        let ctx = ctx_for(&dir);
        ReadTool.call(json!({"file_path": "m.py"}), &ctx).await;

        let ok = EditTool
            .call(json!({"file_path": "m.py", "old_string": "    if x:\n        return 1", "new_string": "    if x:\n        return 2"}), &ctx)
            .await;
        assert!(!ok.is_error, "{}", ok.content);
        assert!(ok.content.contains("indentation"), "the result says a fallback was used: {}", ok.content);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "def f():\n\tif x:\n\t\treturn 2\n\treturn 0\n");

        let miss = EditTool
            .call(json!({"file_path": "m.py", "old_string": "return 9", "new_string": "return 3"}), &ctx)
            .await;
        assert!(miss.is_error);
        assert!(miss.content.contains("not found") && miss.content.contains("return 2"), "{}", miss.content);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn r7_multiedit_is_all_or_nothing() {
        let dir = tmpdir("multi");
        let file = dir.join("t.rs");
        std::fs::write(&file, "let a = 1;\nlet b = 2;\nlet c = 3;\n").unwrap();
        let ctx = ctx_for(&dir);

        let unread = MultiEditTool
            .call(json!({"file_path": "t.rs", "edits": [{"old_string": "1", "new_string": "9"}]}), &ctx)
            .await;
        assert!(unread.is_error && unread.content.contains("has not been read"), "{}", unread.content);
        ReadTool.call(json!({"file_path": "t.rs"}), &ctx).await;

        let failed = MultiEditTool
            .call(
                json!({"file_path": "t.rs", "edits": [
                    {"old_string": "a = 1", "new_string": "a = 10"},
                    {"old_string": "zzz", "new_string": "q"},
                    {"old_string": "c = 3", "new_string": "c = 30"}
                ]}),
                &ctx,
            )
            .await;
        assert!(failed.is_error && failed.content.contains("edit 2 of 3"), "{}", failed.content);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "let a = 1;\nlet b = 2;\nlet c = 3;\n");

        let ok = MultiEditTool
            .call(
                json!({"file_path": "t.rs", "edits": [
                    {"old_string": "a = 1", "new_string": "a = 10"},
                    {"old_string": "a = 10;\nlet b", "new_string": "a = 10;\nlet bb"},
                    {"old_string": "let", "new_string": "const", "replace_all": true}
                ]}),
                &ctx,
            )
            .await;
        assert!(!ok.is_error, "{}", ok.content);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "const a = 10;\nconst bb = 2;\nconst c = 3;\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn r8_edit_that_breaks_python_is_not_written() {
        if std::process::Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let dir = tmpdir("synguard");
        let file = dir.join("g.py");
        std::fs::write(&file, "def f(x):\n    return x + 1\n").unwrap();
        let ctx = ctx_for(&dir);
        ReadTool.call(json!({"file_path": "g.py"}), &ctx).await;
        let bad = EditTool
            .call(json!({"file_path": "g.py", "old_string": "return x + 1", "new_string": "return (x + 1"}), &ctx)
            .await;
        assert!(bad.is_error && bad.content.contains("NOT applied"), "{}", bad.content);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "def f(x):\n    return x + 1\n");
        let ok = EditTool
            .call(json!({"file_path": "g.py", "old_string": "return x + 1", "new_string": "return x + 2"}), &ctx)
            .await;
        assert!(!ok.is_error, "{}", ok.content);
        let w = WriteTool.call(json!({"file_path": "h.py", "content": "def broken(:\n"}), &ctx).await;
        assert!(w.is_error && !dir.join("h.py").exists(), "{}", w.content);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
