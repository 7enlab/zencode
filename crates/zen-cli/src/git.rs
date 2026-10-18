//! Small git helpers used to enrich the system prompt (best-effort, never fatal),
//! plus the `/diff` REPL command plumbing (parity item H6: Claude Code and Codex CLI
//! both have `/diff`; ZenCode shows `git diff HEAD` incl. untracked files).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Budget for the git probes at startup (perf p0): a slow or hung git (network FS, huge repo,
/// credential prompt) must not delay the first request. On timeout the info is just left out.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(2);

/// Budget for user-invoked git commands (`/diff`): local git is fast, but a huge repo can
/// take a few seconds; still bounded so a hung git can't hang the REPL forever.
pub const GIT_DIFF_TIMEOUT: Duration = Duration::from_secs(30);

/// A spawned git probe whose stdout is drained on a thread (so a full pipe never blocks it).
struct Probe {
    child: Child,
    reader: std::thread::JoinHandle<Vec<u8>>,
}

fn spawn_probe(program: &str, cwd: &Path, args: &[&str]) -> Option<Probe> {
    let mut child = Command::new(program)
        .arg("-C")
        .arg(cwd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out.read_to_end(&mut buf);
        buf
    });
    Some(Probe { child, reader })
}

/// Wait for `probe` until `deadline`; kill it and return `None` on timeout or failure.
fn finish_probe(probe: Option<Probe>, deadline: Instant) -> Option<String> {
    let Probe { mut child, reader } = probe?;
    // Reap the stdout-drain thread after killing the child; without this the thread
    // leaks every time a probe times out.
    let kill_and_reap = |mut child: Child, reader: std::thread::JoinHandle<Vec<u8>>| {
        let _ = child.kill();
        let _ = child.wait();
        let _ = reader.join();
    };
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = reader.join().ok()?;
                return status.success().then(|| String::from_utf8_lossy(&out).into_owned());
            }
            Ok(None) => {
                if Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(2));
                } else {
                    kill_and_reap(child, reader);
                    return None;
                }
            }
            // A transient `try_wait` error (e.g. EINTR) must not kill a healthy child;
            // retry unless the deadline has passed.
            Err(_) => {
                if Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(2));
                } else {
                    kill_and_reap(child, reader);
                    return None;
                }
            }
        }
    }
}

/// Run `git` with `args` in `cwd`, capturing stdout. Returns `None` on spawn failure,
/// non-zero exit, or timeout.
fn run_git(cwd: &Path, args: &[&str], timeout: Duration) -> Option<String> {
    let probe = spawn_probe("git", cwd, args);
    finish_probe(probe, Instant::now() + timeout)
}

fn parse_branch(out: &str) -> Option<String> {
    let branch = out.trim().to_string();
    if branch.is_empty() || branch == "HEAD" {
        None
    } else {
        Some(branch)
    }
}

fn parse_status(out: &str) -> String {
    let n = out.lines().filter(|l| !l.trim().is_empty()).count();
    if n == 0 {
        "clean".to_string()
    } else {
        format!("{} changed file(s)", n)
    }
}

/// Branch and status summary, probed concurrently with one shared deadline. Either part is
/// `None` when git is missing, `cwd` is not a repo, or the probe outlives `timeout`.
pub fn git_info_with(program: &str, cwd: &Path, timeout: Duration) -> (Option<String>, Option<String>) {
    let deadline = Instant::now() + timeout;
    let branch = spawn_probe(program, cwd, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let status = spawn_probe(program, cwd, &["status", "--porcelain"]);
    let branch = finish_probe(branch, deadline).and_then(|o| parse_branch(&o));
    let status = finish_probe(status, deadline).map(|o| parse_status(&o));
    (branch, status)
}

/// [`git_info_with`] using `git` from PATH and [`GIT_TIMEOUT`].
pub fn git_info(cwd: &Path) -> (Option<String>, Option<String>) {
    git_info_with("git", cwd, GIT_TIMEOUT)
}

/// Current branch name for the repo containing `cwd`, if any.
pub fn current_branch(cwd: &Path) -> Option<String> {
    // The deadline covers the spawn itself, like `git_info_with` does.
    let deadline = Instant::now() + GIT_TIMEOUT;
    let p = spawn_probe("git", cwd, &["rev-parse", "--abbrev-ref", "HEAD"]);
    finish_probe(p, deadline).and_then(|o| parse_branch(&o))
}

/// One-line `git status --porcelain` summary: "clean" or "N changed files".
pub fn status_summary(cwd: &Path) -> Option<String> {
    let deadline = Instant::now() + GIT_TIMEOUT;
    let p = spawn_probe("git", cwd, &["status", "--porcelain"]);
    finish_probe(p, deadline).map(|o| parse_status(&o))
}

/// True when `cwd` is inside a git work tree.
pub fn is_git_repo(cwd: &Path) -> bool {
    matches!(
        run_git(cwd, &["rev-parse", "--is-inside-work-tree"], GIT_TIMEOUT),
        Some(o) if o.trim() == "true"
    )
}

/// Which `git diff` variant `/diff` should show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffMode {
    /// `git diff HEAD`: staged + unstaged changes vs HEAD (the default).
    WorkingTree,
    /// `git diff --cached`: staged changes only (`/diff --staged`).
    Staged,
    /// `git diff HEAD --stat`: one-line summary per file (`/diff --stat`).
    Stat,
}

/// Full diff text for `mode`, or `None` when git is missing, `cwd` is not a repo,
/// or the command times out. `color` selects `--color=always` vs `--color=never`;
/// the caller decides from tty detection + `NO_COLOR`.
pub fn git_diff(cwd: &Path, mode: DiffMode, color: bool) -> Option<String> {
    let color_arg = if color { "--color=always" } else { "--color=never" };
    let args: Vec<&str> = match mode {
        DiffMode::WorkingTree => vec!["diff", color_arg, "HEAD", "--"],
        DiffMode::Staged => vec!["diff", color_arg, "--cached", "--"],
        DiffMode::Stat => vec!["diff", color_arg, "HEAD", "--stat", "--"],
    };
    run_git(cwd, &args, GIT_DIFF_TIMEOUT)
}

/// Parse `?? <path>` lines out of `git status --porcelain=v1` output.
fn parse_untracked(porcelain: &str) -> Vec<String> {
    porcelain
        .lines()
        .filter_map(|l| l.strip_prefix("?? "))
        .map(|p| {
            // Porcelain quotes paths with special chars ("a b" -> "\"a b\""); unquote the
            // common case so the list is readable.
            let p = p.trim();
            if p.len() >= 2 && p.starts_with('"') && p.ends_with('"') {
                p[1..p.len() - 1].to_string()
            } else {
                p.to_string()
            }
        })
        .collect()
}

/// Untracked file paths (relative to repo root) for `/diff`'s default mode
/// (Codex parity: `/diff` includes untracked files). `None` when git fails.
pub fn untracked_files(cwd: &Path) -> Option<Vec<String>> {
    run_git(cwd, &["status", "--porcelain=v1"], GIT_DIFF_TIMEOUT).map(|o| parse_untracked(&o))
}

/// OS description, e.g. "linux (x86_64)".
pub fn os_info() -> String {
    format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH)
}

/// Create a git worktree at `path` on a new branch `branch` (from HEAD).
/// Returns the worktree path on success, or an error message.
pub fn create_worktree(cwd: &Path, path: &Path, branch: &str) -> Result<PathBuf, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["worktree", "add", "-b", branch, "--"])
        .arg(path)
        .output()
        .map_err(|e| format!("failed to run git: {}", e))?;
    if out.status.success() {
        Ok(path.to_path_buf())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// List worktrees for the repo containing `cwd`: (path, branch, commit).
/// Porcelain records are `worktree`, `HEAD`, then `branch` (or `detached`), separated by blank lines.
pub fn list_worktrees(cwd: &Path) -> Vec<(String, String, String)> {
    let p = spawn_probe("git", cwd, &["worktree", "list", "--porcelain"]);
    let out = match finish_probe(p, Instant::now() + GIT_TIMEOUT) {
        Some(o) => o,
        None => return vec![],
    };
    parse_worktree_porcelain(&out)
}

fn parse_worktree_porcelain(out: &str) -> Vec<(String, String, String)> {
    let mut result = Vec::new();
    let (mut path, mut branch, mut head) = (String::new(), String::new(), String::new());
    for line in out.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if !path.is_empty() {
                let b = if branch.is_empty() { "detached".to_string() } else { branch.clone() };
                result.push((path.clone(), b, head.clone()));
            }
            path.clear();
            branch.clear();
            head.clear();
        } else if let Some(p) = line.strip_prefix("worktree ") {
            path = p.to_string();
        } else if let Some(b) = line.strip_prefix("branch ") {
            branch = b.trim_start_matches("refs/heads/").to_string();
        } else if let Some(c) = line.strip_prefix("HEAD ") {
            head = c.to_string();
        }
    }
    result
}

/// A worktree/branch name typed after `/worktree`: no option-like or path-escaping names.
pub fn valid_worktree_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.starts_with('.')
        && !name.contains("..")
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Remove the worktree at `path` (does not delete the branch). Refuses when it has
/// uncommitted changes, so work is never silently discarded.
pub fn remove_worktree(cwd: &Path, path: &Path) -> Result<(), String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["worktree", "remove"])
        .arg(path)
        .output()
        .map_err(|e| format!("failed to run git: {}", e))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Changed files in the working tree: (status, path) pairs.
/// Status is the two-char porcelain code, e.g. "M ", " M", "??", "A ".
pub fn changed_files(cwd: &Path) -> Vec<(String, String)> {
    let p = spawn_probe("git", cwd, &["status", "--porcelain"]);
    let out = match finish_probe(p, Instant::now() + GIT_TIMEOUT) {
        Some(o) => o,
        None => return vec![],
    };
    out.lines()
        .filter_map(|l| {
            if l.len() < 4 {
                return None;
            }
            let status = l[..2].to_string();
            let path = l[3..].trim().to_string();
            // Handle renames: "R  old -> new" — take the new path.
            let path = path.split(" -> ").last().unwrap_or(&path).to_string();
            Some((status, path))
        })
        .collect()
}

/// Stage all changes and commit with `message`. Returns the commit summary line.
pub fn stage_and_commit(cwd: &Path, message: &str) -> Result<String, String> {
    let add = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["add", "-A"])
        .output()
        .map_err(|e| format!("failed to run git add: {}", e))?;
    if !add.status.success() {
        return Err(String::from_utf8_lossy(&add.stderr).trim().to_string());
    }
    let commit = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["commit", "-m", message])
        .output()
        .map_err(|e| format!("failed to run git commit: {}", e))?;
    if commit.status.success() {
        Ok(String::from_utf8_lossy(&commit.stdout)
            .lines()
            .next()
            .unwrap_or("committed")
            .to_string())
    } else {
        Err(String::from_utf8_lossy(&commit.stderr).trim().to_string())
    }
}

/// Heuristic commit message from changed files.
/// Picks a conventional-commit prefix from the paths, then lists the scope.
pub fn suggest_commit_message(files: &[(String, String)]) -> String {
    if files.is_empty() {
        return "chore: update files".to_string();
    }
    let paths: Vec<&str> = files.iter().map(|(_, p)| p.as_str()).collect();

    // Prefix heuristic.
    let prefix = if paths.iter().all(|p| p.ends_with(".md") || p.contains("CHANGELOG")) {
        "docs"
    } else if paths.iter().all(|p| p.contains("test") || p.contains("spec")) {
        "test"
    } else if paths.iter().any(|p| p.contains("Cargo.toml") || p.contains("package.json")) {
        "chore"
    } else if paths.iter().any(|p| p.ends_with(".rs") || p.ends_with(".ts") || p.ends_with(".py")) {
        // New files suggest a feature, modifications suggest a fix.
        let added = files.iter().any(|(s, _)| s.trim_start().starts_with('A') || s == "??");
        if added { "feat" } else { "fix" }
    } else {
        "chore"
    };

    // Scope: common top-level directory, or file count.
    let scope = if paths.len() == 1 {
        paths[0].to_string()
    } else {
        let dirs: Vec<&str> = paths
            .iter()
            .filter_map(|p| p.split('/').next())
            .collect();
        let first = dirs.first().copied().unwrap_or("");
        if !first.is_empty() && dirs.iter().all(|d| *d == first) {
            format!("{} ({} files)", first, paths.len())
        } else {
            format!("{} files", paths.len())
        }
    };

    format!("{}: update {}", prefix, scope)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_porcelain_keeps_branch_with_its_path() {
        let out = "worktree /r\nHEAD 1111111aaaa\nbranch refs/heads/main\n\nworktree /r-x\nHEAD 2222222bbbb\ndetached\n";
        let v = parse_worktree_porcelain(out);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0], ("/r".into(), "main".into(), "1111111aaaa".into()));
        assert_eq!(v[1], ("/r-x".into(), "detached".into(), "2222222bbbb".into()));
    }

    #[test]
    fn worktree_names_reject_options_and_traversal() {
        assert!(valid_worktree_name("feature-1"));
        assert!(!valid_worktree_name("-b"));
        assert!(!valid_worktree_name("../x"));
        assert!(!valid_worktree_name("a/b"));
        assert!(!valid_worktree_name(""));
    }
    use std::path::PathBuf;

    /// Fresh temp repo with one committed file; `None` when git is unavailable.
    fn init_repo() -> Option<PathBuf> {
        let dir = std::env::temp_dir().join(format!("zen_git_diff_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).ok()?;
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&dir)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !git(&["init", "-q", "-b", "main"]) {
            return None; // git (>= 2.28 for -b) not available here
        }
        git(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "init"]);
        std::fs::write(dir.join("a.txt"), "one\n").ok()?;
        git(&["add", "a.txt"]);
        git(&[
            "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "-m", "add a",
        ]);
        Some(dir)
    }

    #[test]
    fn os_info_nonempty() {
        assert!(!os_info().is_empty());
    }

    #[test]
    fn branch_in_non_repo_is_none_or_some() {
        // Must never panic, regardless of environment.
        let _ = current_branch(&std::env::temp_dir());
    }

    #[test]
    fn status_summary_no_panic() {
        let _ = status_summary(&std::env::temp_dir());
    }

    #[test]
    fn p0_3_missing_git_gives_no_info() {
        let (b, s) = git_info_with("zen-no-such-git-binary", &std::env::temp_dir(), GIT_TIMEOUT);
        assert_eq!((b, s), (None, None));
    }

    #[test]
    fn suggest_commit_message_empty() {
        assert_eq!(suggest_commit_message(&[]), "chore: update files");
    }

    #[test]
    fn suggest_commit_message_single_rs() {
        let files = vec![(" M".to_string(), "crates/zen-cli/src/git.rs".to_string())];
        assert_eq!(
            suggest_commit_message(&files),
            "fix: update crates/zen-cli/src/git.rs"
        );
    }

    #[test]
    fn suggest_commit_message_new_rs_is_feat() {
        let files = vec![("??".to_string(), "crates/zen-cli/src/new.rs".to_string())];
        assert_eq!(
            suggest_commit_message(&files),
            "feat: update crates/zen-cli/src/new.rs"
        );
    }

    #[test]
    fn suggest_commit_message_docs() {
        let files = vec![
            (" M".to_string(), "README.md".to_string()),
            (" M".to_string(), "docs/guide.md".to_string()),
        ];
        assert_eq!(suggest_commit_message(&files), "docs: update 2 files");
    }

    #[test]
    fn suggest_commit_message_same_dir() {
        let files = vec![
            (" M".to_string(), "crates/zen-cli/src/a.rs".to_string()),
            (" M".to_string(), "crates/zen-cli/src/b.rs".to_string()),
        ];
        assert_eq!(
            suggest_commit_message(&files),
            "fix: update crates (2 files)"
        );
    }

    #[test]
    fn changed_files_no_panic_outside_repo() {
        // Must never panic, regardless of environment.
        let _ = changed_files(&std::env::temp_dir());
    }

    #[cfg(unix)]
    #[test]
    fn p0_3_hung_git_is_killed_at_the_timeout() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("zen_git_slow_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("git");
        std::fs::write(&fake, "#!/bin/sh\nexec sleep 5\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let t0 = Instant::now();
        let (b, s) = git_info_with(fake.to_str().unwrap(), &dir, Duration::from_millis(300));
        let took = t0.elapsed();
        assert_eq!((b, s), (None, None));
        // Both probes share one deadline: ~300 ms total, not 2 x 300 ms and never 5 s.
        assert!(took < Duration::from_millis(1500), "took {:?}", took);
    }

    #[test]
    fn p0_3_real_repo_reports_branch_and_status() {
        let dir = std::env::temp_dir().join(format!("zen_git_repo_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let ok = Command::new("git")
            .args(["init", "-q", "-b", "p0main"])
            .current_dir(&dir)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return; // git (>= 2.28) not available here; covered by the other tests
        }
        let (_, status) = git_info(&dir);
        assert_eq!(status.as_deref(), Some("clean"));
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        let (_, status) = git_info(&dir);
        assert_eq!(status.as_deref(), Some("1 changed file(s)"));
    }

    #[test]
    fn is_git_repo_false_outside_repo() {
        assert!(!is_git_repo(&std::env::temp_dir()));
    }

    #[test]
    fn diff_clean_repo_is_empty() {
        let Some(dir) = init_repo() else { return };
        assert!(is_git_repo(&dir));
        let d = git_diff(&dir, DiffMode::WorkingTree, false).unwrap();
        assert!(d.trim().is_empty(), "clean repo should have empty diff");
    }

    #[test]
    fn diff_working_tree_shows_unstaged_change() {
        let Some(dir) = init_repo() else { return };
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        let d = git_diff(&dir, DiffMode::WorkingTree, false).expect("diff should run");
        assert!(d.contains("diff --git a/a.txt b/a.txt"), "diff header missing:\n{d}");
        assert!(d.contains("+two"), "added line missing:\n{d}");
        // Unstaged change must NOT appear in the staged diff.
        let s = git_diff(&dir, DiffMode::Staged, false).expect("staged diff should run");
        assert!(s.trim().is_empty(), "staged diff should be empty:\n{s}");
    }

    #[test]
    fn diff_staged_shows_staged_change() {
        let Some(dir) = init_repo() else { return };
        std::fs::write(dir.join("a.txt"), "one\nstaged-line\n").unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&dir)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
        };
        git(&["add", "a.txt"]).ok();
        let s = git_diff(&dir, DiffMode::Staged, false).expect("staged diff should run");
        assert!(s.contains("+staged-line"), "staged line missing:\n{s}");
    }

    #[test]
    fn diff_stat_summarizes() {
        let Some(dir) = init_repo() else { return };
        std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let st = git_diff(&dir, DiffMode::Stat, false).expect("stat should run");
        assert!(st.contains("a.txt"), "stat should name the file:\n{st}");
        assert!(st.contains("1 file changed"), "stat summary missing:\n{st}");
        // Stat must not contain a full unified diff.
        assert!(!st.contains("@@"), "stat should not contain hunks:\n{st}");
    }

    #[test]
    fn untracked_files_lists_new_files() {
        let Some(dir) = init_repo() else { return };
        assert!(untracked_files(&dir).expect("status should run").is_empty());
        std::fs::write(dir.join("new.txt"), "hello\n").unwrap();
        let u = untracked_files(&dir).expect("status should run");
        assert_eq!(u, vec!["new.txt".to_string()]);
    }

    #[test]
    fn untracked_files_none_outside_repo() {
        assert!(untracked_files(&std::env::temp_dir()).is_none()
            || untracked_files(&std::env::temp_dir()).unwrap().is_empty());
    }

    #[test]
    fn parse_untracked_handles_quoted_paths() {
        let out = "?? plain.txt\n?? \"with space.txt\"\nM  staged.txt\n";
        assert_eq!(
            parse_untracked(out),
            vec!["plain.txt".to_string(), "with space.txt".to_string()]
        );
    }

    #[test]
    fn diff_color_flag_adds_ansi() {
        let Some(dir) = init_repo() else { return };
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        let plain = git_diff(&dir, DiffMode::WorkingTree, false).unwrap();
        assert!(!plain.contains("\x1b["), "plain diff must not have ANSI codes");
        let colored = git_diff(&dir, DiffMode::WorkingTree, true).unwrap();
        assert!(colored.contains("\x1b["), "colored diff should have ANSI codes");
    }
}
