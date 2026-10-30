use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use zen_core::FileCheckpoints;

pub const DEFAULT_MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Before {
    Missing,

    Saved(PathBuf),

    Skipped(String),
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub before: Before,

    after: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Turn {
    pub id: u64,

    pub label: String,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct UndoReport {
    pub turn_id: u64,
    pub label: String,

    pub restored: Vec<PathBuf>,

    pub removed: Vec<PathBuf>,

    pub skipped: Vec<(PathBuf, String)>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum UndoError {
    Nothing,

    Modified(Vec<PathBuf>),

    Io(String),
}

impl std::fmt::Display for UndoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UndoError::Nothing => write!(
                f,
                "nothing to undo: no turn changed files with Write/Edit/MultiEdit"
            ),
            UndoError::Modified(paths) => {
                write!(
                    f,
                    "refusing to undo: changed outside zen since the turn wrote them:"
                )?;
                for p in paths {
                    write!(f, "\n  {}", p.display())?;
                }
                write!(
                    f,
                    "\nRun `/undo --force` to restore anyway (those edits are lost)."
                )
            }
            UndoError::Io(e) => write!(f, "undo failed: {e}"),
        }
    }
}

struct State {
    next_id: u64,
    current: Option<Turn>,
    done: Vec<Turn>,
}

pub struct Checkpoints {
    dir: PathBuf,
    max_bytes: u64,
    state: Mutex<State>,
}

fn stamp(bytes: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    bytes.len().hash(&mut h);
    bytes.hash(&mut h);
    h.finish()
}

fn label_of(prompt: &str) -> String {
    let line = prompt.lines().next().unwrap_or("").trim();
    let mut out: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        out.push('…');
    }
    out
}

impl Checkpoints {
    pub fn new(dir: PathBuf) -> Self {
        Self::with_max_bytes(dir, DEFAULT_MAX_FILE_BYTES)
    }

    pub fn with_max_bytes(dir: PathBuf, max_bytes: u64) -> Self {
        Self {
            dir,
            max_bytes,
            state: Mutex::new(State {
                next_id: 1,
                current: None,
                done: Vec::new(),
            }),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn start_turn(&self, prompt: &str) {
        let mut st = self.state.lock().unwrap();
        Self::close_current(&mut st);
        let id = st.next_id;
        st.next_id += 1;
        st.current = Some(Turn {
            id,
            label: label_of(prompt),
            entries: Vec::new(),
        });
    }

    fn close_current(st: &mut State) {
        if let Some(t) = st.current.take() {
            if t.entries.iter().any(|e| e.after.is_some()) {
                st.done.push(t);
            }
        }
    }

    pub fn last_turn(&self) -> Option<Turn> {
        let mut st = self.state.lock().unwrap();
        Self::close_current(&mut st);
        st.done.last().cloned()
    }

    pub fn depth(&self) -> usize {
        let mut st = self.state.lock().unwrap();
        Self::close_current(&mut st);
        st.done.len()
    }

    fn snapshot(&self, turn_id: u64, n: usize, path: &Path) -> Before {
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Before::Missing,
            Err(e) => return Before::Skipped(format!("could not stat it: {e}")),
        };
        if meta.len() > self.max_bytes {
            return Before::Skipped(format!(
                "it is {:.1} MB, over the {:.0} MB checkpoint limit",
                meta.len() as f64 / 1_048_576.0,
                self.max_bytes as f64 / 1_048_576.0
            ));
        }
        let dir = self.dir.join(turn_id.to_string());
        let backup = dir.join(format!("{n}.bak"));
        match std::fs::create_dir_all(&dir).and_then(|_| std::fs::copy(path, &backup)) {
            Ok(_) => Before::Saved(backup),
            Err(e) => Before::Skipped(format!("could not save a copy: {e}")),
        }
    }

    pub fn undo(&self, force: bool) -> Result<UndoReport, UndoError> {
        let mut st = self.state.lock().unwrap();
        Self::close_current(&mut st);
        let Some(turn) = st.done.last() else {
            return Err(UndoError::Nothing);
        };
        if !force {
            let modified: Vec<PathBuf> = turn
                .entries
                .iter()
                .filter(|e| !matches!(e.before, Before::Skipped(_)))
                .filter(|e| match (e.after, std::fs::read(&e.path)) {
                    (Some(want), Ok(now)) => stamp(&now) != want,
                    (Some(_), Err(_)) => true,
                    (None, _) => false,
                })
                .map(|e| e.path.clone())
                .collect();
            if !modified.is_empty() {
                return Err(UndoError::Modified(modified));
            }
        }
        let turn = st.done.pop().expect("checked above");
        let mut report = UndoReport {
            turn_id: turn.id,
            label: turn.label.clone(),
            ..Default::default()
        };

        for e in turn.entries.iter().rev() {
            if e.after.is_none() {
                continue;
            }
            match &e.before {
                Before::Saved(backup) => {
                    let data = std::fs::read(backup)
                        .map_err(|err| UndoError::Io(format!("{}: {err}", backup.display())))?;
                    crate::atomic::write_atomic(&e.path, &data)
                        .map_err(|err| UndoError::Io(format!("{}: {err}", e.path.display())))?;
                    report.restored.push(e.path.clone());
                }
                Before::Missing => match std::fs::remove_file(&e.path) {
                    Ok(()) => report.removed.push(e.path.clone()),
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {
                        report.removed.push(e.path.clone())
                    }
                    Err(err) => return Err(UndoError::Io(format!("{}: {err}", e.path.display()))),
                },
                Before::Skipped(why) => report.skipped.push((e.path.clone(), why.clone())),
            }
        }
        report.restored.reverse();
        report.removed.reverse();
        report.skipped.reverse();
        let _ = std::fs::remove_dir_all(self.dir.join(turn.id.to_string()));
        Ok(report)
    }

    pub fn diff_last_turn(&self, base: &Path) -> Option<(Turn, String)> {
        let turn = self.last_turn()?;
        let mut out = String::new();
        for e in &turn.entries {
            if e.after.is_none() {
                continue;
            }
            let rel = e.path.strip_prefix(base).unwrap_or(&e.path);
            let label = rel
                .display()
                .to_string()
                .trim_start_matches(['/', '\\'])
                .to_string();
            let old = match &e.before {
                Before::Missing => None,
                Before::Saved(b) => Some(std::fs::read(b).unwrap_or_default()),
                Before::Skipped(why) => {
                    out.push_str(&format!("--- {label}\n(no checkpoint: {why})\n"));
                    continue;
                }
            };
            let new = std::fs::read(&e.path).ok();
            out.push_str(&file_diff(&label, old.as_deref(), new.as_deref()));
        }
        Some((turn, out))
    }
}

impl FileCheckpoints for Checkpoints {
    fn begin_turn(&self, prompt: &str) {
        self.start_turn(prompt);
    }

    fn before_write(&self, path: &Path) -> Option<String> {
        let mut st = self.state.lock().unwrap();
        if st.current.is_none() {
            let id = st.next_id;
            st.next_id += 1;
            st.current = Some(Turn {
                id,
                label: String::new(),
                entries: Vec::new(),
            });
        }
        let turn = st.current.as_mut().expect("set above");
        if turn.entries.iter().any(|e| e.path == path) {
            return None;
        }
        let (id, n) = (turn.id, turn.entries.len());
        let before = self.snapshot(id, n, path);
        let warning = match &before {
            Before::Skipped(why) => Some(format!(
                "Note: no undo checkpoint for {} ({why}); /undo will not restore it.",
                path.display()
            )),
            _ => None,
        };
        turn.entries.push(Entry {
            path: path.to_path_buf(),
            before,
            after: None,
        });
        warning
    }

    fn after_write(&self, path: &Path, content: &[u8]) {
        let mut st = self.state.lock().unwrap();
        if let Some(e) = st
            .current
            .as_mut()
            .and_then(|t| t.entries.iter_mut().find(|e| e.path == path))
        {
            e.after = Some(stamp(content));
        }
    }
}

const DIFF_CELLS: usize = 4_000_000;

pub fn file_diff(label: &str, old: Option<&[u8]>, new: Option<&[u8]>) -> String {
    let (a_name, b_name) = (
        if old.is_some() {
            format!("a/{label}")
        } else {
            "/dev/null".into()
        },
        if new.is_some() {
            format!("b/{label}")
        } else {
            "/dev/null".into()
        },
    );
    let mut out = format!("--- {a_name}\n+++ {b_name}\n");
    let (old_b, new_b) = (old.unwrap_or_default(), new.unwrap_or_default());
    if old_b == new_b {
        out.push_str("(unchanged)\n");
        return out;
    }
    let (Ok(old_s), Ok(new_s)) = (std::str::from_utf8(old_b), std::str::from_utf8(new_b)) else {
        out.push_str(&format!(
            "Binary files differ ({} → {} bytes)\n",
            old_b.len(),
            new_b.len()
        ));
        return out;
    };
    let a: Vec<&str> = old_s.split_inclusive('\n').collect();
    let b: Vec<&str> = new_s.split_inclusive('\n').collect();
    match line_ops(&a, &b) {
        Some(ops) => out.push_str(&hunks(&a, &b, &ops, 3)),
        None => out.push_str(&format!(
            "(too large to diff: {} → {} lines)\n",
            a.len(),
            b.len()
        )),
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
    Same,
    Del,
    Add,
}

fn line_ops(a: &[&str], b: &[&str]) -> Option<Vec<Op>> {
    let pre = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let suf = a[pre..]
        .iter()
        .rev()
        .zip(b[pre..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (am, bm) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    let (n, m) = (am.len(), bm.len());
    if n.saturating_mul(m) > DIFF_CELLS {
        return None;
    }

    let w = m + 1;
    let mut lcs = vec![0u32; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i * w + j] = if am[i] == bm[j] {
                lcs[(i + 1) * w + j + 1] + 1
            } else {
                lcs[(i + 1) * w + j].max(lcs[i * w + j + 1])
            };
        }
    }
    let mut ops = vec![Op::Same; pre];
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && am[i] == bm[j] {
            ops.push(Op::Same);
            i += 1;
            j += 1;
        } else if i < n && (j == m || lcs[(i + 1) * w + j] >= lcs[i * w + j + 1]) {
            ops.push(Op::Del);
            i += 1;
        } else {
            ops.push(Op::Add);
            j += 1;
        }
    }
    ops.resize(ops.len() + suf, Op::Same);
    Some(ops)
}

fn push_line(out: &mut String, sign: char, line: &str) {
    out.push(sign);
    out.push_str(line);
    if !line.ends_with('\n') {
        out.push_str("\n\\ No newline at end of file\n");
    }
}

fn hunks(a: &[&str], b: &[&str], ops: &[Op], ctx: usize) -> String {
    let mut pos = Vec::with_capacity(ops.len());
    let (mut ia, mut ib) = (0usize, 0usize);
    for op in ops {
        pos.push((ia, ib));
        match op {
            Op::Same => {
                ia += 1;
                ib += 1;
            }
            Op::Del => ia += 1,
            Op::Add => ib += 1,
        }
    }
    let changed: Vec<usize> = (0..ops.len()).filter(|&k| ops[k] != Op::Same).collect();
    let mut out = String::new();
    let mut k = 0;
    while k < changed.len() {
        let start = changed[k].saturating_sub(ctx);
        let mut end = changed[k];
        while k + 1 < changed.len() && changed[k + 1] <= end + 2 * ctx + 1 {
            k += 1;
            end = changed[k];
        }
        let end = (end + ctx + 1).min(ops.len());
        let (a0, b0) = pos[start];
        let a_len = ops[start..end].iter().filter(|o| **o != Op::Add).count();
        let b_len = ops[start..end].iter().filter(|o| **o != Op::Del).count();
        let a_start = if a_len == 0 { a0 } else { a0 + 1 };
        let b_start = if b_len == 0 { b0 } else { b0 + 1 };
        out.push_str(&format!("@@ -{a_start},{a_len} +{b_start},{b_len} @@\n"));
        for idx in start..end {
            let (pa, pb) = pos[idx];
            match ops[idx] {
                Op::Same => push_line(&mut out, ' ', a[pa]),
                Op::Del => push_line(&mut out, '-', a[pa]),
                Op::Add => push_line(&mut out, '+', b[pb]),
            }
        }
        k += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "zen_ckpt_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(c: &Checkpoints, p: &Path, s: &str) -> Option<String> {
        let w = c.before_write(p);
        std::fs::write(p, s).unwrap();
        c.after_write(p, s.as_bytes());
        w
    }

    #[test]
    fn p0_3_undo_restores_edit_and_deletes_created_file() {
        let d = tmp("undo");
        let (a, b) = (d.join("a.txt"), d.join("b.txt"));
        std::fs::write(&a, "old\n").unwrap();
        let c = Checkpoints::new(d.join(".ck"));
        c.start_turn("edit a, create b");
        write(&c, &a, "new\n");
        write(&c, &a, "newer\n");
        write(&c, &b, "fresh\n");
        c.start_turn("just chatting");
        assert_eq!(c.depth(), 1);
        let r = c.undo(false).unwrap();
        assert_eq!(r.restored, vec![a.clone()]);
        assert_eq!(r.removed, vec![b.clone()]);
        assert_eq!(r.label, "edit a, create b");
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "old\n");
        assert!(!b.exists());
        assert_eq!(c.undo(false), Err(UndoError::Nothing));
        assert!(
            !d.join(".ck/1").exists(),
            "backups of an undone turn are removed"
        );
    }

    #[test]
    fn p0_3_undo_refuses_outside_edits_unless_forced() {
        let d = tmp("refuse");
        let a = d.join("a.txt");
        std::fs::write(&a, "v1").unwrap();
        let c = Checkpoints::new(d.join(".ck"));
        c.start_turn("t1");
        write(&c, &a, "v2");
        std::fs::write(&a, "hand edit").unwrap();
        assert_eq!(c.undo(false), Err(UndoError::Modified(vec![a.clone()])));
        assert_eq!(
            std::fs::read_to_string(&a).unwrap(),
            "hand edit",
            "nothing touched"
        );
        assert_eq!(c.depth(), 1, "turn kept for a forced retry");
        c.undo(true).unwrap();
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "v1");
    }

    #[test]
    fn p0_3_undo_walks_back_turn_by_turn() {
        let d = tmp("stack");
        let a = d.join("a.txt");
        std::fs::write(&a, "1").unwrap();
        let c = Checkpoints::new(d.join(".ck"));
        c.start_turn("t1");
        write(&c, &a, "2");
        c.start_turn("t2");
        write(&c, &a, "3");
        c.undo(false).unwrap();
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "2");
        c.undo(false).unwrap();
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "1");
    }

    #[test]
    fn p0_3_large_file_is_skipped_with_warning() {
        let d = tmp("large");
        let big = d.join("big.bin");
        std::fs::write(&big, vec![b'x'; 64]).unwrap();
        let c = Checkpoints::with_max_bytes(d.join(".ck"), 16);
        c.start_turn("t");
        let w = write(&c, &big, "small now").expect("warning");
        assert!(
            w.contains("no undo checkpoint") && w.contains("checkpoint limit"),
            "{w}"
        );
        let r = c.undo(false).unwrap();
        assert_eq!(r.skipped.len(), 1);
        assert_eq!(
            std::fs::read_to_string(&big).unwrap(),
            "small now",
            "left alone"
        );
    }

    #[test]
    fn p0_3_failed_write_is_not_an_undo_step() {
        let d = tmp("failed");
        let a = d.join("a.txt");
        let c = Checkpoints::new(d.join(".ck"));
        c.start_turn("t");
        assert!(c.before_write(&a).is_none());
        assert_eq!(c.undo(false), Err(UndoError::Nothing));
    }

    #[test]
    fn p0_3_diff_last_turn() {
        let d = tmp("diff");
        let (a, b) = (d.join("a.txt"), d.join("b.txt"));
        std::fs::write(&a, "one\ntwo\nthree\n").unwrap();
        let c = Checkpoints::new(d.join(".ck"));
        assert!(c.diff_last_turn(&d).is_none());
        c.start_turn("t");
        write(&c, &a, "one\nTWO\nthree\n");
        write(&c, &b, "hi\n");
        let (turn, diff) = c.diff_last_turn(&d).unwrap();
        assert_eq!(turn.entries.len(), 2);
        assert!(
            diff.contains("--- a/a.txt\n+++ b/a.txt\n@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three\n"),
            "{diff}"
        );
        assert!(diff.contains("+++ b/b.txt\n"), "{diff}");
        let (_, abs) = c.diff_last_turn(Path::new("/elsewhere")).unwrap();
        assert!(!abs.contains("a//"), "absolute paths get one slash: {abs}");
        assert!(
            diff.contains("--- /dev/null\n") && diff.contains("@@ -0,0 +1,1 @@\n+hi\n"),
            "{diff}"
        );
    }

    #[test]
    fn p0_3_file_diff_hunks_and_edge_cases() {
        let old: String = (1..=20).map(|i| format!("l{i}\n")).collect();
        let new = old.replace("l2\n", "L2\n").replace("l19\n", "");
        let d = file_diff("f", Some(old.as_bytes()), Some(new.as_bytes()));
        assert_eq!(d.matches("@@ ").count(), 2, "{d}");
        assert!(d.contains("@@ -1,5 +1,5 @@\n l1\n-l2\n+L2\n"), "{d}");
        assert!(
            d.contains("@@ -16,5 +16,4 @@\n l16\n l17\n l18\n-l19\n l20\n"),
            "{d}"
        );
        assert!(file_diff("f", Some(b"x"), Some(b"x")).contains("(unchanged)"));
        assert!(file_diff("f", Some(b"a"), Some(b"b")).contains("\\ No newline at end of file"));
        assert!(file_diff("f", Some(&[0xff, 0]), Some(b"t")).contains("Binary files differ"));
        assert!(file_diff("f", Some(b"gone\n"), None)
            .contains("+++ /dev/null\n@@ -1,1 +0,0 @@\n-gone\n"));
    }
}
