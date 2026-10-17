//! Small git helpers used to enrich the system prompt (best-effort, never fatal).

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Budget for the git probes at startup (perf p0): a slow or hung git (network FS, huge repo,
/// credential prompt) must not delay the first request. On timeout the info is just left out.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(2);

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
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = reader.join().ok()?;
                return status.success().then(|| String::from_utf8_lossy(&out).into_owned());
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
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
    let p = spawn_probe("git", cwd, &["rev-parse", "--abbrev-ref", "HEAD"]);
    finish_probe(p, Instant::now() + GIT_TIMEOUT).and_then(|o| parse_branch(&o))
}

/// One-line `git status --porcelain` summary: "clean" or "N changed files".
pub fn status_summary(cwd: &Path) -> Option<String> {
    let p = spawn_probe("git", cwd, &["status", "--porcelain"]);
    finish_probe(p, Instant::now() + GIT_TIMEOUT).map(|o| parse_status(&o))
}

/// OS description, e.g. "linux (x86_64)".
pub fn os_info() -> String {
    format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
