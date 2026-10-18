//! GitHub integration through the `gh` CLI: PR review comments (`/pr_comments`,
//! `zen pr-comments`) and CI failure analysis (`/ci-fix`, `zen ci-fix`).
//!
//! Auth belongs to `gh` (`gh auth login`); zen never sees tokens. Fetching is
//! read-only; applying fixes happens through normal agent turns, gated by the
//! permission mode (see [`fix_offer_for_mode`]). All parse/format helpers are
//! pure so unit tests never touch the network.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::cli::PermissionMode;

/// Timeout for user-invoked `gh` calls (network I/O). A hung call is killed, never fatal.
pub const GH_TIMEOUT: Duration = Duration::from_secs(30);

/// Log lines kept per failed check: enough for diagnosis, bounded for the prompt.
pub const MAX_LOG_LINES: usize = 120;

/// Lines scanned for the heuristic failure summary.
pub const MAX_ERROR_LINES: usize = 25;

// ─── gh runner ───────────────────────────────────────────────────────────────

/// Run `gh <args>` in `cwd`, returning stdout. Killed at [`GH_TIMEOUT`].
fn run_gh(cwd: &Path, args: &[&str]) -> Result<String> {
    run_gh_with("gh", cwd, args)
}

/// Injectable binary name (tests use a bogus name to simulate "not installed").
fn run_gh_with(bin: &str, cwd: &Path, args: &[&str]) -> Result<String> {
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow!("gh CLI not found (install from https://cli.github.com): {e}"))?;
    // Drain both pipes on a thread so a full pipe never blocks the child.
    let mut out = child.stdout.take().expect("stdout piped");
    let mut err = child.stderr.take().expect("stderr piped");
    let reader = std::thread::spawn(move || {
        let mut o = Vec::new();
        let mut e = Vec::new();
        let _ = out.read_to_end(&mut o);
        let _ = err.read_to_end(&mut e);
        (o, e)
    });
    let deadline = Instant::now() + GH_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let (o, e) = reader.join().map_err(|_| anyhow!("gh output thread failed"))?;
                let stderr = String::from_utf8_lossy(&e);
                if !status.success() {
                    if stderr.contains("auth login") || stderr.contains("GH_TOKEN") {
                        return Err(anyhow!(
                            "gh is not authenticated (run `gh auth login`): {}",
                            stderr.trim()
                        ));
                    }
                    return Err(anyhow!("gh {} failed: {}", args.join(" "), stderr.trim()));
                }
                return Ok(String::from_utf8_lossy(&o).into_owned());
            }
            Ok(None) | Err(_) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = reader.join();
                    return Err(anyhow!("gh {} timed out after {:?}", args.join(" "), GH_TIMEOUT));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

/// Run `gh` and parse stdout as JSON.
fn gh_json(cwd: &Path, args: &[&str]) -> Result<serde_json::Value> {
    let out = run_gh(cwd, args)?;
    serde_json::from_str(&out)
        .with_context(|| format!("gh {} returned non-JSON output", args.join(" ")))
}

/// `true` when the `gh` CLI is installed and authenticated.
pub fn gh_available(cwd: &Path) -> bool {
    run_gh(cwd, &["auth", "status"]).is_ok()
}

// ─── PR comments ─────────────────────────────────────────────────────────────

/// Inline review comment vs. PR-conversation comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommentKind {
    Review,
    Issue,
}

/// One PR comment, normalised from the GitHub API shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrComment {
    pub id: u64,
    pub author: String,
    pub body: String,
    pub kind: CommentKind,
    /// File the comment is anchored to (`None` for conversation comments).
    pub path: Option<String>,
    pub line: Option<u64>,
    pub diff_hunk: Option<String>,
    pub created_at: String,
    pub url: String,
}

/// Everything `/pr_comments` needs: the PR plus both comment streams.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchedComments {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub review_comments: Vec<PrComment>,
    pub issue_comments: Vec<PrComment>,
}

impl FetchedComments {
    pub fn total(&self) -> usize {
        self.review_comments.len() + self.issue_comments.len()
    }
}

#[derive(Deserialize)]
struct RawUser {
    login: Option<String>,
}

#[derive(Deserialize)]
struct RawReviewComment {
    id: u64,
    body: Option<String>,
    path: Option<String>,
    line: Option<u64>,
    diff_hunk: Option<String>,
    created_at: Option<String>,
    html_url: Option<String>,
    user: Option<RawUser>,
}

#[derive(Deserialize)]
struct RawIssueComment {
    id: u64,
    body: Option<String>,
    created_at: Option<String>,
    html_url: Option<String>,
    user: Option<RawUser>,
}

fn author_of(user: &Option<RawUser>) -> String {
    user.as_ref()
        .and_then(|u| u.login.clone())
        .unwrap_or_else(|| "(unknown)".to_string())
}

/// Parse `gh api .../pulls/{n}/comments` output (pure; testable without network).
pub fn parse_review_comments(json: &str) -> Result<Vec<PrComment>> {
    let raw: Vec<RawReviewComment> =
        serde_json::from_str(json).context("review comments JSON did not parse")?;
    Ok(raw
        .into_iter()
        .map(|c| PrComment {
            id: c.id,
            author: author_of(&c.user),
            body: c.body.unwrap_or_default(),
            kind: CommentKind::Review,
            path: c.path,
            line: c.line,
            diff_hunk: c.diff_hunk,
            created_at: c.created_at.unwrap_or_default(),
            url: c.html_url.unwrap_or_default(),
        })
        .collect())
}

/// Parse `gh api .../issues/{n}/comments` output (pure; testable without network).
pub fn parse_issue_comments(json: &str) -> Result<Vec<PrComment>> {
    let raw: Vec<RawIssueComment> =
        serde_json::from_str(json).context("issue comments JSON did not parse")?;
    Ok(raw
        .into_iter()
        .map(|c| PrComment {
            id: c.id,
            author: author_of(&c.user),
            body: c.body.unwrap_or_default(),
            kind: CommentKind::Issue,
            path: None,
            line: None,
            diff_hunk: None,
            created_at: c.created_at.unwrap_or_default(),
            url: c.html_url.unwrap_or_default(),
        })
        .collect())
}

/// Resolve the PR: explicit `#123`/`123` arg, else the PR for the current branch.
fn resolve_pr(cwd: &Path, pr_arg: Option<&str>) -> Result<(String, u64)> {
    // The binary prints only the outermost error, so let the actionable auth hint
    // through unwrapped instead of burying it under repo-detection context.
    let nwo = gh_json(cwd, &["repo", "view", "--json", "nameWithOwner", "--jq", ".nameWithOwner"])
        .map_err(|e| {
            if e.to_string().contains("not authenticated") {
                e
            } else {
                anyhow!("could not determine the GitHub repo (is this a GitHub checkout?)")
            }
        })?;
    let nwo = nwo.as_str().unwrap_or("").trim().to_string();
    if nwo.is_empty() {
        return Err(anyhow!("could not determine the GitHub repo"));
    }
    let number: u64 = match pr_arg {
        Some(a) => a
            .trim()
            .trim_start_matches('#')
            .parse()
            .with_context(|| format!("invalid PR number: {a}"))?,
        None => {
            let out = run_gh(cwd, &["pr", "view", "--json", "number", "--jq", ".number"])
                .context("no PR number given and no PR found for the current branch")?;
            out.trim()
                .parse()
                .context("could not parse the current branch's PR number")?
        }
    };
    Ok((nwo, number))
}

/// Fetch a PR's title plus both comment streams. Read-only.
pub fn fetch_pr_comments(cwd: &Path, pr_arg: Option<&str>) -> Result<FetchedComments> {
    let (nwo, number) = resolve_pr(cwd, pr_arg)?;
    let meta = gh_json(
        cwd,
        &[
            "api",
            &format!("repos/{nwo}/pulls/{number}"),
            "--jq",
            "{number: .number, title: .title, url: .html_url}",
        ],
    )
    .with_context(|| format!("could not fetch PR #{number}"))?;
    let review_json = run_gh(
        cwd,
        &["api", &format!("repos/{nwo}/pulls/{number}/comments"), "--paginate"],
    )
    .with_context(|| format!("could not fetch review comments for PR #{number}"))?;
    let issue_json = run_gh(
        cwd,
        &["api", &format!("repos/{nwo}/issues/{number}/comments"), "--paginate"],
    )
    .with_context(|| format!("could not fetch conversation comments for PR #{number}"))?;
    Ok(FetchedComments {
        repo: nwo,
        number: meta["number"].as_u64().unwrap_or(number),
        title: meta["title"].as_str().unwrap_or("").to_string(),
        url: meta["url"].as_str().unwrap_or("").to_string(),
        review_comments: parse_review_comments(&review_json)?,
        issue_comments: parse_issue_comments(&issue_json)?,
    })
}

// ─── Severity ────────────────────────────────────────────────────────────────

/// Rough priority of a review comment, from keyword heuristics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
    Nit,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Self::Critical => "CRITICAL",
            Self::High => "HIGH",
            Self::Medium => "MEDIUM",
            Self::Low => "LOW",
            Self::Nit => "NIT",
        }
    }
    /// Lower rank = shown first.
    fn rank(self) -> u8 {
        match self {
            Self::Critical => 0,
            Self::High => 1,
            Self::Medium => 2,
            Self::Low => 3,
            Self::Nit => 4,
        }
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

/// Keyword heuristic over the comment body (pure; testable).
pub fn severity_of(body: &str) -> Severity {
    let b = body.to_lowercase();
    if contains_any(
        &b,
        &[
            "security",
            "vulnerab",
            "unsound",
            "data race",
            "panic",
            "must fix",
            "blocking",
            "do not merge",
        ],
    ) {
        Severity::Critical
    } else if contains_any(
        &b,
        &[
            "bug", "wrong", "incorrect", "broken", "fails", "missing", "should", "fix this",
        ],
    ) {
        Severity::High
    } else if contains_any(&b, &["nitpick", "nit:", "typo", "style:"]) {
        Severity::Nit
    } else if contains_any(&b, &["consider", "suggestion", "maybe", "could"]) {
        Severity::Low
    } else {
        Severity::Medium
    }
}

// ─── Grouping & display ──────────────────────────────────────────────────────

/// Comments grouped for display: one group per file (alphabetical, severity-first
/// within the file), conversation comments last.
#[derive(Debug, Clone)]
pub struct CommentGroup {
    pub title: String,
    pub comments: Vec<PrComment>,
}

pub fn group_comments(fetched: &FetchedComments) -> Vec<CommentGroup> {
    use std::collections::BTreeMap;
    let mut by_file: BTreeMap<String, Vec<PrComment>> = BTreeMap::new();
    for c in &fetched.review_comments {
        by_file
            .entry(c.path.clone().unwrap_or_else(|| "(no file)".to_string()))
            .or_default()
            .push(c.clone());
    }
    let mut groups: Vec<CommentGroup> = by_file
        .into_iter()
        .map(|(title, mut comments)| {
            comments.sort_by(|a, b| {
                severity_of(&b.body)
                    .rank()
                    .cmp(&severity_of(&a.body).rank())
                    .then(a.line.cmp(&b.line))
            });
            CommentGroup { title, comments }
        })
        .collect();
    if !fetched.issue_comments.is_empty() {
        groups.push(CommentGroup {
            title: "General discussion".to_string(),
            comments: fetched.issue_comments.clone(),
        });
    }
    groups
}

/// Human-readable, file-grouped rendering of PR comments (pure; testable).
pub fn format_comments_text(fetched: &FetchedComments) -> String {
    let mut s = String::new();
    s.push_str(&format!("PR #{}: {}\n{}\n", fetched.number, fetched.title, fetched.url));
    if fetched.total() == 0 {
        s.push_str("No review comments.\n");
        return s;
    }
    s.push_str(&format!("{} comment(s):\n", fetched.total()));
    for g in group_comments(fetched) {
        s.push_str(&format!(
            "\n## {} ({} comment{})\n",
            g.title,
            g.comments.len(),
            if g.comments.len() == 1 { "" } else { "s" }
        ));
        for c in &g.comments {
            let loc = match c.line {
                Some(l) => format!(":{l}"),
                None => String::new(),
            };
            s.push_str(&format!(
                "  [{}] @{}{}\n",
                severity_of(&c.body).label(),
                c.author,
                loc
            ));
            for line in c.body.lines().take(15) {
                s.push_str(&format!("  > {line}\n"));
            }
            if c.body.lines().count() > 15 {
                s.push_str("  > ...\n");
            }
            if !c.url.is_empty() {
                s.push_str(&format!("  {}\n", c.url));
            }
        }
    }
    s
}

// ─── CI status ───────────────────────────────────────────────────────────────

/// One failed GitHub Actions workflow run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailingCheck {
    pub run_id: u64,
    pub name: String,
    pub conclusion: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_excerpt: Option<String>,
}

/// Failing checks for the current branch (serializable for `--json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiStatus {
    pub branch: String,
    pub pr_number: Option<u64>,
    pub failing: Vec<FailingCheck>,
}

#[derive(Deserialize)]
struct RawWorkflowRun {
    #[serde(rename = "databaseId")]
    database_id: u64,
    name: Option<String>,
    conclusion: Option<String>,
    url: Option<String>,
}

/// Parse `gh run list --json databaseId,name,conclusion,url`, keeping failures.
/// Pure; testable without network.
pub fn parse_workflow_runs(json: &str) -> Result<Vec<FailingCheck>> {
    let raw: Vec<RawWorkflowRun> =
        serde_json::from_str(json).context("workflow runs JSON did not parse")?;
    Ok(raw
        .into_iter()
        .filter(|r| r.conclusion.as_deref() == Some("failure"))
        .map(|r| FailingCheck {
            run_id: r.database_id,
            name: r.name.unwrap_or_else(|| "(unnamed)".to_string()),
            conclusion: r.conclusion.unwrap_or_else(|| "failure".to_string()),
            url: r.url.unwrap_or_default(),
            log_excerpt: None,
        })
        .collect())
}

/// Keep the last `max_lines` of a log (failures usually surface at the end).
pub fn truncate_log(log: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = log.lines().collect();
    if lines.len() <= max_lines {
        return log.to_string();
    }
    format!(
        "... ({} earlier lines omitted)\n{}",
        lines.len() - max_lines,
        lines[lines.len() - max_lines..].join("\n")
    )
}

/// Heuristic: the log lines most likely to explain the failure.
pub fn extract_error_lines(log: &str, max: usize) -> Vec<String> {
    const KEYS: &[&str] = &[
        "error",
        "failed",
        "failure",
        "panicked",
        "assertion",
        "traceback",
        "exception",
    ];
    let mut out = Vec::new();
    for line in log.lines() {
        let l = line.to_lowercase();
        // "0 failed" / "0 errors" are success summaries, not failures.
        if l.contains("0 failed") || l.contains("0 errors") || l.contains("0 error") {
            continue;
        }
        if KEYS.iter().any(|k| l.contains(k)) {
            out.push(line.trim().to_string());
            if out.len() >= max {
                break;
            }
        }
    }
    out
}

/// Fetch failing workflow runs for the current branch, with failed-log excerpts.
/// Read-only; a missing log never fails the whole call.
pub fn fetch_ci_status(cwd: &Path) -> Result<CiStatus> {
    let branch =
        crate::git::current_branch(cwd).ok_or_else(|| anyhow!("not a git repo (or no branch)"))?;
    let pr_number = run_gh(cwd, &["pr", "view", "--json", "number", "--jq", ".number"])
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok());
    let runs_json = run_gh(
        cwd,
        &[
            "run",
            "list",
            "--branch",
            &branch,
            "--limit",
            "10",
            "--json",
            "databaseId,name,conclusion,url",
        ],
    )
    .context("could not list workflow runs (is `gh` authenticated?)")?;
    let mut failing = parse_workflow_runs(&runs_json)?;
    for f in failing.iter_mut() {
        if let Ok(log) = run_gh(cwd, &["run", "view", &f.run_id.to_string(), "--log-failed"]) {
            f.log_excerpt = Some(truncate_log(&log, MAX_LOG_LINES));
        }
    }
    Ok(CiStatus {
        branch,
        pr_number,
        failing,
    })
}

/// Human-readable rendering of failing checks, with heuristic failure summaries.
pub fn format_ci_text(status: &CiStatus) -> String {
    let mut s = String::new();
    s.push_str(&format!("branch: {}", status.branch));
    if let Some(n) = status.pr_number {
        s.push_str(&format!(" (PR #{n})"));
    }
    s.push('\n');
    if status.failing.is_empty() {
        s.push_str("All CI checks passing.\n");
        return s;
    }
    s.push_str(&format!("{} failing check(s):\n", status.failing.len()));
    for f in &status.failing {
        s.push_str(&format!("\n## {} — {}\n{}\n", f.name, f.conclusion, f.url));
        match &f.log_excerpt {
            Some(log) => {
                let errors = extract_error_lines(log, MAX_ERROR_LINES);
                if errors.is_empty() {
                    s.push_str("(no clear error lines in the failed log)\n");
                } else {
                    s.push_str("likely failure cause:\n");
                    for e in &errors {
                        s.push_str(&format!("  ! {e}\n"));
                    }
                }
            }
            None => s.push_str("(could not fetch the failed log)\n"),
        }
    }
    s
}

// ─── Agent prompts ───────────────────────────────────────────────────────────

/// Prompt for an agent turn that addresses PR review comments.
pub fn pr_fix_prompt(fetched: &FetchedComments) -> String {
    let mut p = format!(
        "Address the following review comments on PR #{} \"{}\" ({}).\n\
         For each comment, fix the underlying issue in the code. \
         Do not post replies on GitHub; only change the code.\n\n",
        fetched.number, fetched.title, fetched.url
    );
    for g in group_comments(fetched) {
        p.push_str(&format!("### {}\n", g.title));
        for c in &g.comments {
            p.push_str(&format!(
                "- @{} [{}]: {}\n",
                c.author,
                severity_of(&c.body).label(),
                c.body
            ));
        }
        p.push('\n');
    }
    p
}

/// Prompt for an agent turn that diagnoses and fixes CI failures.
pub fn ci_fix_prompt(status: &CiStatus) -> String {
    let mut p = format!(
        "The following GitHub Actions checks failed on branch `{}`{}:\n\n",
        status.branch,
        status
            .pr_number
            .map(|n| format!(" (PR #{n})"))
            .unwrap_or_default()
    );
    for f in &status.failing {
        p.push_str(&format!("## {} — {}\n{}\n", f.name, f.conclusion, f.url));
        if let Some(log) = &f.log_excerpt {
            p.push_str("Failed log excerpt:\n```\n");
            p.push_str(log);
            p.push_str("\n```\n");
        }
    }
    p.push_str(
        "\nAnalyze the failures and fix the underlying issues in the code. \
         Do not modify CI workflow files unless the failure is clearly in the workflow itself. \
         After fixing, summarize what was changed and how to verify (e.g. `cargo test`).\n",
    );
    p
}

// ─── Permission gating ───────────────────────────────────────────────────────

/// Whether a REPL command may offer to dispatch an agent turn that applies fixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixOffer {
    /// Read-only: never offer (plan mode).
    Never,
    /// Ask the user first (default mode).
    Ask,
    /// Proceed without asking (acceptEdits / bypassPermissions / dontAsk).
    Auto,
}

/// Pure mapping from permission mode to fix-offer behaviour (testable).
pub fn fix_offer_for_mode(mode: PermissionMode) -> FixOffer {
    match mode {
        PermissionMode::Plan => FixOffer::Never,
        PermissionMode::Default => FixOffer::Ask,
        PermissionMode::AcceptEdits | PermissionMode::BypassPermissions | PermissionMode::DontAsk => {
            FixOffer::Auto
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const REVIEW_JSON: &str = r#"[
        {
            "id": 101,
            "body": "This is a security issue: unsanitized input reaches the shell.",
            "path": "src/main.rs",
            "line": 42,
            "diff_hunk": "@@ -40,3 +40,3 @@",
            "created_at": "2026-10-17T10:00:00Z",
            "html_url": "https://github.com/o/r/pull/1#discussion_r101",
            "user": {"login": "hark"}
        },
        {
            "id": 102,
            "body": "nit: typo in comment",
            "path": "src/lib.rs",
            "line": 7,
            "diff_hunk": null,
            "created_at": "2026-10-17T11:00:00Z",
            "html_url": "https://github.com/o/r/pull/1#discussion_r102",
            "user": {"login": "muse"}
        }
    ]"#;

    const ISSUE_JSON: &str = r#"[
        {
            "id": 201,
            "body": "LGTM, let's merge after CI passes.",
            "created_at": "2026-10-17T12:00:00Z",
            "html_url": "https://github.com/o/r/pull/1#issuecomment-201",
            "user": {"login": "zen"}
        }
    ]"#;

    const RUNS_JSON: &str = r#"[
        {"databaseId": 11, "name": "test", "conclusion": "failure", "url": "https://github.com/o/r/actions/runs/11"},
        {"databaseId": 12, "name": "lint", "conclusion": "success", "url": "https://github.com/o/r/actions/runs/12"},
        {"databaseId": 13, "name": "build", "conclusion": "in_progress", "url": "https://github.com/o/r/actions/runs/13"}
    ]"#;

    fn sample_fetched() -> FetchedComments {
        FetchedComments {
            repo: "o/r".to_string(),
            number: 1,
            title: "Add feature".to_string(),
            url: "https://github.com/o/r/pull/1".to_string(),
            review_comments: parse_review_comments(REVIEW_JSON).unwrap(),
            issue_comments: parse_issue_comments(ISSUE_JSON).unwrap(),
        }
    }

    #[test]
    fn parse_review_comments_maps_fields() {
        let cs = parse_review_comments(REVIEW_JSON).unwrap();
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[0].id, 101);
        assert_eq!(cs[0].author, "hark");
        assert_eq!(cs[0].kind, CommentKind::Review);
        assert_eq!(cs[0].path.as_deref(), Some("src/main.rs"));
        assert_eq!(cs[0].line, Some(42));
        assert!(cs[0].diff_hunk.is_some());
        assert_eq!(cs[1].path.as_deref(), Some("src/lib.rs"));
    }

    #[test]
    fn parse_review_comments_rejects_bad_json() {
        assert!(parse_review_comments("not json").is_err());
        assert!(parse_review_comments("{}").is_err());
    }

    #[test]
    fn parse_issue_comments_are_conversation_kind() {
        let cs = parse_issue_comments(ISSUE_JSON).unwrap();
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].kind, CommentKind::Issue);
        assert_eq!(cs[0].path, None);
        assert_eq!(cs[0].line, None);
        assert_eq!(cs[0].author, "zen");
    }

    #[test]
    fn severity_table() {
        assert_eq!(severity_of("security vulnerability here"), Severity::Critical);
        assert_eq!(severity_of("do not merge yet"), Severity::Critical);
        assert_eq!(severity_of("this is wrong"), Severity::High);
        assert_eq!(severity_of("bug: off by one"), Severity::High);
        assert_eq!(severity_of("you should handle the error"), Severity::High);
        assert_eq!(severity_of("nit: typo"), Severity::Nit);
        assert_eq!(severity_of("style: indent"), Severity::Nit);
        assert_eq!(severity_of("consider using a map"), Severity::Low);
        assert_eq!(severity_of("maybe rename this"), Severity::Low);
        assert_eq!(severity_of("why is this needed?"), Severity::Medium);
        assert_eq!(severity_of("looks good"), Severity::Medium);
    }

    #[test]
    fn group_comments_by_file_severity_first() {
        let f = sample_fetched();
        let groups = group_comments(&f);
        assert_eq!(groups.len(), 3); // src/lib.rs, src/main.rs, General discussion
        assert_eq!(groups[0].title, "src/lib.rs");
        assert_eq!(groups[1].title, "src/main.rs");
        assert_eq!(groups[2].title, "General discussion");
        assert_eq!(groups[2].comments[0].kind, CommentKind::Issue);
    }

    #[test]
    fn format_comments_text_shows_groups_and_severity() {
        let text = format_comments_text(&sample_fetched());
        assert!(text.contains("PR #1: Add feature"));
        assert!(text.contains("## src/main.rs"));
        assert!(text.contains("[CRITICAL]"));
        assert!(text.contains("@hark:42"));
        assert!(text.contains("[NIT]"));
        assert!(text.contains("## General discussion"));
    }

    #[test]
    fn format_comments_text_empty() {
        let f = FetchedComments {
            repo: "o/r".into(),
            number: 2,
            title: "x".into(),
            url: "u".into(),
            review_comments: vec![],
            issue_comments: vec![],
        };
        assert!(format_comments_text(&f).contains("No review comments"));
    }

    #[test]
    fn parse_workflow_runs_keeps_only_failures() {
        let failing = parse_workflow_runs(RUNS_JSON).unwrap();
        assert_eq!(failing.len(), 1);
        assert_eq!(failing[0].run_id, 11);
        assert_eq!(failing[0].name, "test");
        assert_eq!(failing[0].log_excerpt, None);
    }

    #[test]
    fn truncate_log_keeps_tail() {
        let log: String = (0..200).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let t = truncate_log(&log, 120);
        assert!(t.starts_with("... (80 earlier lines omitted)"));
        assert!(t.contains("line 199"));
        assert!(!t.contains("line 0\n"));
        // Short logs pass through unchanged.
        assert_eq!(truncate_log("a\nb", 120), "a\nb");
    }

    #[test]
    fn extract_error_lines_finds_failures() {
        let log = " Compiling foo\nerror[E0308]: mismatched types\n   Finished ok\ntest bar FAILED\ntest result: FAILED. 0 passed; 1 failed\n0 errors here\n";
        let errs = extract_error_lines(log, 25);
        assert!(errs.iter().any(|l| l.contains("mismatched types")));
        assert!(errs.iter().any(|l| l.contains("FAILED")));
        assert!(!errs.iter().any(|l| l.contains("0 errors")));
    }

    #[test]
    fn fix_offer_for_mode_all_modes() {
        assert_eq!(fix_offer_for_mode(PermissionMode::Plan), FixOffer::Never);
        assert_eq!(fix_offer_for_mode(PermissionMode::Default), FixOffer::Ask);
        assert_eq!(fix_offer_for_mode(PermissionMode::AcceptEdits), FixOffer::Auto);
        assert_eq!(
            fix_offer_for_mode(PermissionMode::BypassPermissions),
            FixOffer::Auto
        );
        assert_eq!(fix_offer_for_mode(PermissionMode::DontAsk), FixOffer::Auto);
    }

    #[test]
    fn run_gh_missing_binary_is_clear_error() {
        let err = run_gh_with("zen-no-such-gh-binary", &std::env::temp_dir(), &["--version"])
            .unwrap_err();
        assert!(err.to_string().contains("gh CLI not found"), "{err}");
    }

    #[test]
    fn pr_fix_prompt_lists_comments_and_forbids_replies() {
        let p = pr_fix_prompt(&sample_fetched());
        assert!(p.contains("PR #1"));
        assert!(p.contains("@hark"));
        assert!(p.contains("[CRITICAL]"));
        assert!(p.contains("Do not post replies on GitHub"));
    }

    #[test]
    fn ci_fix_prompt_contains_branch_and_logs() {
        let status = CiStatus {
            branch: "muse/x".into(),
            pr_number: Some(7),
            failing: vec![FailingCheck {
                run_id: 11,
                name: "test".into(),
                conclusion: "failure".into(),
                url: "https://example.invalid/runs/11".into(),
                log_excerpt: Some("error: boom".into()),
            }],
        };
        let p = ci_fix_prompt(&status);
        assert!(p.contains("muse/x"));
        assert!(p.contains("PR #7"));
        assert!(p.contains("error: boom"));
        assert!(p.contains("Do not modify CI workflow files"));
    }

    #[test]
    fn format_ci_text_empty_is_all_passing() {
        let status = CiStatus {
            branch: "main".into(),
            pr_number: None,
            failing: vec![],
        };
        let t = format_ci_text(&status);
        assert!(t.contains("All CI checks passing"));
    }

    #[test]
    fn fetched_total_counts_both_streams() {
        assert_eq!(sample_fetched().total(), 3);
    }
}
