//! Session archive / restore / delete management (`zen sessions ...`).
//!
//! Active sessions live as `<proj>/<session-id>.jsonl` under `~/.zen/projects/`
//! (see [`crate::session`]). Archiving moves the transcript (plus its scratch dir)
//! into `<proj>/archive/`, which keeps it out of the default `list` and out of
//! `--continue` (that only scans the top level), while `restore` moves it back.
//! Deleting removes the transcript and the scratch dir permanently.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Archive subdirectory inside each project dir.
pub const ARCHIVE_DIR_NAME: &str = "archive";

/// One session, active or archived.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    /// Session id (file stem).
    pub id: String,
    /// Full transcript path.
    pub path: PathBuf,
    /// True when under `<proj>/archive/`.
    pub archived: bool,
    /// Transcript mtime.
    pub modified: SystemTime,
    /// Number of transcript records (non-empty JSONL lines).
    pub message_count: usize,
    /// Summed `usage` records (see [`crate::session::load_usage`]).
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Latest `/rename` title, if any.
    pub title: Option<String>,
}

/// Archive dir for a project dir: `<proj>/archive`.
pub fn archive_dir(proj_dir: &Path) -> PathBuf {
    proj_dir.join(ARCHIVE_DIR_NAME)
}

/// Short display id: first 8 chars.
pub fn short_id(id: &str) -> &str {
    &id[..id.len().min(8)]
}

/// Format a mtime as `YYYY-MM-DD HH:MM UTC`.
pub fn fmt_mtime(t: SystemTime) -> String {
    let dt: DateTime<Utc> = t.into();
    dt.format("%Y-%m-%d %H:%M").to_string()
}

/// Build a [`SessionInfo`] from a transcript path.
pub fn session_info(path: &Path, archived: bool) -> SessionInfo {
    let id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("?")
        .to_string();
    let (message_count, input_tokens, output_tokens) = scan_transcript(path);
    SessionInfo {
        id,
        path: path.to_path_buf(),
        archived,
        modified: std::fs::metadata(path)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH),
        message_count,
        input_tokens,
        output_tokens,
        title: crate::session::session_title(path),
    }
}

/// Count records and sum usage tokens from a transcript file (best effort: a missing or
/// unreadable file yields zeros rather than an error, so `list` never fails on one bad file).
fn scan_transcript(path: &Path) -> (usize, u64, u64) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return (0, 0, 0);
    };
    let mut count = 0usize;
    let mut input = 0u64;
    let mut output = 0u64;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        count += 1;
        if v.get("subtype").and_then(|t| t.as_str()) == Some("usage") {
            input += v["usage"]["input_tokens"].as_u64().unwrap_or(0);
            output += v["usage"]["output_tokens"].as_u64().unwrap_or(0);
        }
    }
    (count, input, output)
}

/// Iterate every session file under `<home>/.zen/projects`, newest first.
/// `include_archived` adds `<proj>/archive/*.jsonl` entries (flagged `archived: true`).
pub fn list_all_sessions(home: &Path, include_archived: bool) -> Vec<SessionInfo> {
    let projects_root = home.join(".zen").join("projects");
    if !projects_root.exists() {
        return vec![];
    }
    let mut out: Vec<SessionInfo> = vec![];
    let Ok(proj_dirs) = std::fs::read_dir(&projects_root) else {
        return vec![];
    };
    for proj in proj_dirs.flatten() {
        let proj_path = proj.path();
        if !proj_path.is_dir() {
            continue;
        }
        collect_dir(&proj_path, false, &mut out);
        if include_archived {
            let ad = archive_dir(&proj_path);
            if ad.is_dir() {
                collect_dir(&ad, true, &mut out);
            }
        }
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.modified));
    out
}

fn collect_dir(dir: &Path, archived: bool, out: &mut Vec<SessionInfo>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_file() && p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
            out.push(session_info(&p, archived));
        }
    }
}

/// Resolve a session id (or absolute path) to its transcript, searching active sessions
/// first and the archive second. Returns `(path, archived)`.
pub fn find_managed_session(home: &Path, id_or_path: &str) -> Option<(PathBuf, bool)> {
    let p = PathBuf::from(id_or_path);
    if p.is_absolute() && p.exists() {
        return Some((p.clone(), is_archived_path(home, &p)));
    }
    if p.exists() {
        if let Ok(abs) = p.canonicalize() {
            return Some((abs.clone(), is_archived_path(home, &abs)));
        }
    }
    for info in list_all_sessions(home, true) {
        if info.id == id_or_path {
            return Some((info.path, info.archived));
        }
    }
    None
}

fn is_archived_path(home: &Path, path: &Path) -> bool {
    let projects_root = home.join(".zen").join("projects");
    path.strip_prefix(&projects_root)
        .ok()
        .and_then(|rel| rel.components().nth(1))
        .and_then(|c| c.as_os_str().to_str())
        == Some(ARCHIVE_DIR_NAME)
}

/// Move a session's transcript (and scratch dir) into the project archive dir.
pub fn archive_session(home: &Path, id: &str) -> Result<PathBuf> {
    let (path, archived) =
        find_managed_session(home, id).ok_or_else(|| anyhow!("no session found: {}", id))?;
    if archived {
        return Err(anyhow!("session {} is already archived", short_id(id)));
    }
    let proj = path
        .parent()
        .ok_or_else(|| anyhow!("cannot determine project dir for {}", path.display()))?;
    let ad = archive_dir(proj);
    std::fs::create_dir_all(&ad)?;
    let dest = ad.join(path.file_name().ok_or_else(|| anyhow!("bad session path"))?);
    if dest.exists() {
        return Err(anyhow!(
            "session {} already exists in the archive; restore or delete it first",
            short_id(id)
        ));
    }
    move_session_files(&path, &dest)?;
    Ok(dest)
}

/// Move an archived session back to its project dir.
pub fn restore_session(home: &Path, id: &str) -> Result<PathBuf> {
    let (path, archived) =
        find_managed_session(home, id).ok_or_else(|| anyhow!("no session found: {}", id))?;
    if !archived {
        return Err(anyhow!(
            "session {} is not archived (nothing to restore)",
            short_id(id)
        ));
    }
    let archive = path
        .parent()
        .ok_or_else(|| anyhow!("cannot determine archive dir for {}", path.display()))?;
    let proj = archive
        .parent()
        .ok_or_else(|| anyhow!("cannot determine project dir for {}", path.display()))?;
    let dest = proj.join(path.file_name().ok_or_else(|| anyhow!("bad session path"))?);
    if dest.exists() {
        return Err(anyhow!(
            "an active session {} already exists; delete it first",
            short_id(id)
        ));
    }
    move_session_files(&path, &dest)?;
    Ok(dest)
}

/// Permanently delete a session: transcript plus its scratch dir (`<proj>/<id>/` or
/// `<proj>/archive/<id>/`). There is no undo.
pub fn delete_session(home: &Path, id: &str) -> Result<()> {
    let (path, _) =
        find_managed_session(home, id).ok_or_else(|| anyhow!("no session found: {}", id))?;
    // Scratch dir sits next to the transcript: the transcript path without `.jsonl`.
    let scratch = path.with_extension("");
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    if scratch.is_dir() {
        std::fs::remove_dir_all(&scratch)?;
    }
    Ok(())
}

/// Move transcript + scratch dir from `src` to `dst`.
fn move_session_files(src: &Path, dst: &Path) -> Result<()> {
    std::fs::rename(src, dst).map_err(|e| {
        anyhow!(
            "failed to move {} to {}: {}",
            src.display(),
            dst.display(),
            e
        )
    })?;
    let src_scratch = src.with_extension("");
    if src_scratch.is_dir() {
        let dst_scratch = dst.with_extension("");
        // Best effort: a scratch move failure must not lose the transcript (already moved).
        if let Err(e) = std::fs::rename(&src_scratch, &dst_scratch) {
            eprintln!(
                "warning: archived the transcript but could not move scratch dir {}: {}",
                src_scratch.display(),
                e
            );
        }
    }
    Ok(())
}

/// Archive every active session whose transcript is older than `days` days.
/// Returns the archived session ids (short display form not used here: full ids).
/// `days == 0` disables (returns empty).
pub fn auto_archive_old(home: &Path, days: u64) -> Result<Vec<String>> {
    if days == 0 {
        return Ok(vec![]);
    }
    let cutoff = SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(days.saturating_mul(86400)))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut archived = vec![];
    for info in list_all_sessions(home, false) {
        if info.modified < cutoff {
            match archive_session(home, &info.id) {
                Ok(_) => archived.push(info.id),
                Err(e) => eprintln!("warning: auto-archive of {} failed: {}", short_id(&info.id), e),
            }
        }
    }
    Ok(archived)
}

/// Pure check for the delete confirmation prompt (tested directly; the prompt itself reads
/// stdin in [`confirm_delete_prompt`]).
pub fn parse_confirmation_answer(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Ask on stderr whether to permanently delete a session. Returns true on `y`/`yes`.
/// Errors on non-interactive stdin (scripts must pass `--yes`).
pub fn confirm_delete_prompt(id: &str) -> Result<bool> {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        anyhow::bail!("refusing to delete session without --yes on non-interactive stdin");
    }
    eprint!(
        "Permanently delete session {}? This cannot be undone. [y/N] ",
        short_id(id)
    );
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(parse_confirmation_answer(&answer))
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_home(tag: &str) -> PathBuf {
        let h = std::env::temp_dir().join(format!("zen_sm_{}_{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&h);
        h
    }

    fn make_session(home: &Path, cwd: &Path, id: &str) -> PathBuf {
        let path = crate::session::ensure_session_path(home, cwd, id).unwrap();
        for i in 0..3 {
            crate::session::append_typed(
                &path,
                "user",
                id,
                cwd.to_str().unwrap(),
                &format!("u{}", i),
                None,
                Some(serde_json::json!({"role":"user","content":"hi"})),
                None,
            )
            .unwrap();
        }
        // One usage record so token accounting is exercised.
        crate::session::append_usage(
            &path,
            id,
            &zen_core::UsageStats { input_tokens: 100, output_tokens: 50, ..Default::default() },
        )
        .unwrap();
        path
    }

    #[test]
    fn archive_restore_roundtrip() {
        let home = tmp_home("rt");
        let cwd = home.join("proj");
        fs::create_dir_all(&cwd).unwrap();
        let path = make_session(&home, &cwd, "sess-rt");

        let dest = archive_session(&home, "sess-rt").unwrap();
        assert!(!path.exists());
        assert!(dest.exists());
        assert!(dest.to_string_lossy().contains("archive"));

        // Default list excludes it; --all includes it flagged.
        assert!(list_all_sessions(&home, false).is_empty());
        let all = list_all_sessions(&home, true);
        assert_eq!(all.len(), 1);
        assert!(all[0].archived);
        assert_eq!(all[0].message_count, 4);
        assert_eq!(all[0].input_tokens, 100);

        // Double archive is an error.
        assert!(archive_session(&home, "sess-rt").is_err());

        let back = restore_session(&home, "sess-rt").unwrap();
        assert_eq!(back, path);
        assert!(path.exists());
        let listed = list_all_sessions(&home, false);
        assert_eq!(listed.len(), 1);
        assert!(!listed[0].archived);

        // Restoring an active session is an error.
        assert!(restore_session(&home, "sess-rt").is_err());
    }

    #[test]
    fn delete_removes_transcript_and_scratch() {
        let home = tmp_home("del");
        let cwd = home.join("proj");
        fs::create_dir_all(&cwd).unwrap();
        let path = make_session(&home, &cwd, "sess-del");
        // Scratch dir with a file in it.
        let scratch = path.with_extension("");
        fs::create_dir_all(&scratch).unwrap();
        fs::write(scratch.join("spill.txt"), "x").unwrap();

        delete_session(&home, "sess-del").unwrap();
        assert!(!path.exists());
        assert!(!scratch.exists());
        assert!(list_all_sessions(&home, true).is_empty());

        // Unknown id errors.
        assert!(delete_session(&home, "nope").is_err());
    }

    #[test]
    fn delete_archived_session() {
        let home = tmp_home("delarch");
        let cwd = home.join("proj");
        fs::create_dir_all(&cwd).unwrap();
        make_session(&home, &cwd, "sess-da");
        archive_session(&home, "sess-da").unwrap();
        delete_session(&home, "sess-da").unwrap();
        assert!(list_all_sessions(&home, true).is_empty());
    }

    #[test]
    fn auto_archive_old_only_archives_stale() {
        let home = tmp_home("auto");
        let cwd = home.join("proj");
        fs::create_dir_all(&cwd).unwrap();
        let old_path = make_session(&home, &cwd, "sess-old");
        make_session(&home, &cwd, "sess-new");

        // Backdate the old session by 10 days via mtime.
        let ten_days = std::time::Duration::from_secs(10 * 86400);
        let old_mtime = SystemTime::now().checked_sub(ten_days).unwrap();
        let f = fs::File::options().write(true).open(&old_path).unwrap();
        f.set_modified(old_mtime).unwrap();

        let archived = auto_archive_old(&home, 7).unwrap();
        assert_eq!(archived, vec!["sess-old".to_string()]);
        // Fresh session untouched.
        let active = list_all_sessions(&home, false);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "sess-new");

        // days == 0 disables.
        let none = auto_archive_old(&home, 0).unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn find_managed_session_reports_archived_flag() {
        let home = tmp_home("find");
        let cwd = home.join("proj");
        fs::create_dir_all(&cwd).unwrap();
        make_session(&home, &cwd, "sess-f");
        let (p, arch) = find_managed_session(&home, "sess-f").unwrap();
        assert!(!arch);
        assert!(p.exists());
        archive_session(&home, "sess-f").unwrap();
        let (_, arch2) = find_managed_session(&home, "sess-f").unwrap();
        assert!(arch2);
        assert!(find_managed_session(&home, "missing").is_none());
    }

    #[test]
    fn confirmation_answer_parsing() {
        assert!(parse_confirmation_answer("y"));
        assert!(parse_confirmation_answer("Y"));
        assert!(parse_confirmation_answer("yes"));
        assert!(parse_confirmation_answer("  YES  "));
        assert!(!parse_confirmation_answer("n"));
        assert!(!parse_confirmation_answer(""));
        assert!(!parse_confirmation_answer("yeah"));
    }

    #[test]
    fn short_id_and_fmt_mtime() {
        assert_eq!(short_id("abcdef123456"), "abcdef12");
        assert_eq!(short_id("abc"), "abc");
        let s = fmt_mtime(SystemTime::UNIX_EPOCH);
        assert_eq!(s, "1970-01-01 00:00");
    }

    #[test]
    fn archive_moves_scratch_dir_too() {
        let home = tmp_home("scratch");
        let cwd = home.join("proj");
        fs::create_dir_all(&cwd).unwrap();
        let path = make_session(&home, &cwd, "sess-s");
        let scratch = path.with_extension("");
        fs::create_dir_all(&scratch).unwrap();
        fs::write(scratch.join("log.txt"), "x").unwrap();

        let dest = archive_session(&home, "sess-s").unwrap();
        assert!(!scratch.exists());
        assert!(dest.with_extension("").join("log.txt").exists());

        restore_session(&home, "sess-s").unwrap();
        assert!(scratch.join("log.txt").exists());
    }
}
