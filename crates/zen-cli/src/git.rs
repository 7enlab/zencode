//! Small git helpers used to enrich the system prompt (best-effort, never fatal).

use std::path::Path;
use std::process::Command;

/// Current branch name for the repo containing `cwd`, if any.
pub fn current_branch(cwd: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if branch.is_empty() || branch == "HEAD" {
        None
    } else {
        Some(branch)
    }
}

/// One-line `git status --porcelain` summary: "clean" or "N changed files".
pub fn status_summary(cwd: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["status", "--porcelain"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let n = text.lines().filter(|l| !l.trim().is_empty()).count();
    Some(if n == 0 {
        "clean".to_string()
    } else {
        format!("{} changed file(s)", n)
    })
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
}
