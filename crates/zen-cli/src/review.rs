








use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use zen_core::{
    EventSink, HookRunner, NoHooks, NullSink, PermissionChecker, PermissionDecision, PermissionMode,
    ToolContext, ToolRegistry,
};

use crate::cli::{Cli, OutputFormat, ReviewArgs};





#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
    Nit,
}

impl Severity {

    pub fn rank(self) -> u8 {
        match self {
            Severity::Critical => 4,
            Severity::High => 3,
            Severity::Medium => 2,
            Severity::Low => 1,
            Severity::Nit => 0,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Critical => "critical",
            Severity::High => "high",
            Severity::Medium => "medium",
            Severity::Low => "low",
            Severity::Nit => "nit",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "critical" | "crit" | "p0" => Severity::Critical,
            "high" | "p1" => Severity::High,
            "medium" | "med" | "p2" => Severity::Medium,
            "low" | "p3" => Severity::Low,
            "nit" | "p4" | "info" => Severity::Nit,
            _ => return None,
        })
    }


    pub fn ordered() -> [Severity; 5] {
        [
            Severity::Critical,
            Severity::High,
            Severity::Medium,
            Severity::Low,
            Severity::Nit,
        ]
    }
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}




#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub severity: Severity,

    #[serde(default = "default_category")]
    pub category: String,
    pub file: String,
    #[serde(default)]
    pub line: Option<u32>,
    pub title: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub suggestion: String,
}

fn default_category() -> String {
    "bugs".to_string()
}


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewReport {
    pub scope: String,
    pub model: String,
    pub findings: Vec<Finding>,
    pub summary: ReviewSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewSummary {
    pub total: usize,
    pub critical: usize,
    pub high: usize,
    pub medium: usize,
    pub low: usize,
    pub nit: usize,

    pub max_severity: Option<Severity>,
}

impl ReviewReport {
    pub fn new(scope: String, model: String, findings: Vec<Finding>) -> Self {
        let mut summary = ReviewSummary {
            total: findings.len(),
            critical: 0,
            high: 0,
            medium: 0,
            low: 0,
            nit: 0,
            max_severity: None,
        };
        for f in &findings {
            match f.severity {
                Severity::Critical => summary.critical += 1,
                Severity::High => summary.high += 1,
                Severity::Medium => summary.medium += 1,
                Severity::Low => summary.low += 1,
                Severity::Nit => summary.nit += 1,
            }
            if summary
                .max_severity
                .is_none_or(|m| f.severity.rank() > m.rank())
            {
                summary.max_severity = Some(f.severity);
            }
        }
        Self {
            scope,
            model,
            findings,
            summary,
        }
    }


    pub fn at_or_above(&self, threshold: Severity) -> Vec<&Finding> {
        self.findings
            .iter()
            .filter(|f| f.severity.rank() >= threshold.rank())
            .collect()
    }
}




#[derive(Debug, Clone)]
pub enum ReviewScope {
    Uncommitted,
    Base(String),
    Commit(String),
}

impl ReviewScope {
    pub fn from_args(args: &ReviewArgs) -> Result<Self> {
        if args.base.is_some() && args.commit.is_some() {
            anyhow::bail!("--base and --commit are mutually exclusive");
        }
        if let Some(b) = &args.base {
            if b.trim().is_empty() {
                anyhow::bail!("--base requires a non-empty ref");
            }
            Ok(ReviewScope::Base(b.trim().to_string()))
        } else if let Some(c) = &args.commit {
            if c.trim().is_empty() {
                anyhow::bail!("--commit requires a non-empty sha");
            }
            Ok(ReviewScope::Commit(c.trim().to_string()))
        } else {
            Ok(ReviewScope::Uncommitted)
        }
    }

    pub fn describe(&self) -> String {
        match self {
            ReviewScope::Uncommitted => "uncommitted changes".to_string(),
            ReviewScope::Base(b) => format!("diff against base '{b}'"),
            ReviewScope::Commit(c) => format!("commit '{c}'"),
        }
    }
}

pub struct CollectedDiff {
    pub scope_description: String,
    pub files: Vec<String>,
    pub diff_text: String,
    pub truncated: bool,
}



const REVIEW_GIT_TIMEOUT: Duration = Duration::from_secs(15);


const UNTRACKED_FILE_CAP: usize = 20_000;

fn run_git(cwd: &Path, args: &[&str]) -> Result<String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn git (is git installed?)")?;
    let deadline = Instant::now() + REVIEW_GIT_TIMEOUT;
    loop {
        match child.try_wait().context("failed to poll git")? {
            Some(status) => {
                let out = child.wait_with_output().context("failed to read git output")?;
                if !status.success() {
                    let err = String::from_utf8_lossy(&out.stderr);
                    anyhow::bail!("git {} failed: {}", args.join(" "), err.trim());
                }
                return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
            }
            None if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            None => {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!("git {} timed out after {:?}", args.join(" "), REVIEW_GIT_TIMEOUT);
            }
        }
    }
}


fn changed_files_from_diff(diff: &str) -> Vec<String> {
    let mut files = Vec::new();
    for line in diff.lines() {

        if let Some(rest) = line.strip_prefix("diff --git ") {
            let parts: Vec<&str> = rest.split(' ').collect();
            if parts.len() == 2 {
                let b = parts[1].strip_prefix("b/").unwrap_or(parts[1]);
                if !files.contains(&b.to_string()) {
                    files.push(b.to_string());
                }
            }
        }
    }
    files
}


pub fn collect_diff(cwd: &Path, scope: &ReviewScope, max_bytes: usize) -> Result<CollectedDiff> {

    run_git(cwd, &["rev-parse", "--git-dir"]).context("not a git repository")?;

    let mut files: Vec<String> = Vec::new();
    let mut text = String::new();

    match scope {
        ReviewScope::Uncommitted => {

            let diff = run_git(cwd, &["diff", "HEAD", "--"])?;
            files.extend(changed_files_from_diff(&diff));
            text.push_str(&diff);


            let status = run_git(cwd, &["status", "--porcelain", "--"])?;
            for line in status.lines() {
                if let Some(path) = line.strip_prefix("?? ") {
                    let p = path.trim();
                    if p.is_empty() {
                        continue;
                    }
                    if !files.contains(&p.to_string()) {
                        files.push(p.to_string());
                    }
                    let full = cwd.join(p);
                    if full.is_file() {
                        text.push_str(&format!("\n--- new file: {p} ---\n"));
                        match std::fs::read(&full) {
                            Ok(bytes) => {
                                let mut s = String::from_utf8_lossy(&bytes).into_owned();
                                if s.len() > UNTRACKED_FILE_CAP {
                                    s.truncate(UNTRACKED_FILE_CAP);
                                    s.push_str("\n[... truncated: file larger than 20k ...]\n");
                                }
                                text.push_str(&s);
                                if !s.ends_with('\n') {
                                    text.push('\n');
                                }
                            }
                            Err(e) => {
                                text.push_str(&format!("[could not read: {e}]\n"));
                            }
                        }
                    }
                }
            }
        }
        ReviewScope::Base(base) => {
            let diff = run_git(cwd, &["diff", base, "--"])?;
            files.extend(changed_files_from_diff(&diff));
            text = diff;
        }
        ReviewScope::Commit(sha) => {
            let show = run_git(cwd, &["show", "--format=", sha, "--"])?;
            files.extend(changed_files_from_diff(&show));
            text = show;
        }
    }

    let truncated = text.len() > max_bytes;
    if truncated {
        text.truncate(max_bytes);
        text.push_str("\n[... diff truncated: exceeded --max-diff-bytes ...]\n");
    }

    Ok(CollectedDiff {
        scope_description: scope.describe(),
        files,
        diff_text: text,
        truncated,
    })
}





pub fn review_system_prompt() -> String {
    "You are a senior code reviewer. Review the provided diff carefully and report \
     concrete, actionable findings. You have read-only tools (Read, Glob, Grep) to \
     inspect surrounding code when a diff hunk lacks context. You CANNOT modify \
     anything: no write tools exist in this session.\n\n\
     OUTPUT CONTRACT (must follow exactly): output ONLY a JSON array of findings \
     inside a ```json fenced block, no prose outside it. Each finding:\n\
     {\"severity\": \"critical|high|medium|low|nit\", \
      \"category\": \"bugs|security|perf|style|tests-missing\", \
      \"file\": \"path/relative\", \"line\": 123 or null, \
      \"title\": \"short rule name\", \
      \"detail\": \"what is wrong and why it matters\", \
      \"suggestion\": \"concrete fix\"}\n\n\
     Severity guide:\n\
     - critical: data loss, exploitable security issue, crash on a normal path\n\
     - high: likely bug, broken error handling, race condition, wrong API use\n\
     - medium: real code smell, missing tests for new logic, clear perf problem\n\
     - low: minor clarity or maintainability note\n\
     - nit: formatting, naming, trivial style\n\n\
     Be strict but fair: do not invent problems. If the diff is clean, output []."
        .to_string()
}

pub fn build_review_prompt(diff: &CollectedDiff) -> String {
    let mut p = String::new();
    p.push_str("Review the following changes (");
    p.push_str(&diff.scope_description);
    p.push_str(").\n\nChanged files:\n");
    if diff.files.is_empty() {
        p.push_str("(none detected)\n");
    } else {
        for f in &diff.files {
            p.push_str("- ");
            p.push_str(f);
            p.push('\n');
        }
    }
    if diff.truncated {
        p.push_str("\nNOTE: the diff was truncated for size; focus on what is shown.\n");
    }
    p.push_str("\n```diff\n");
    p.push_str(&diff.diff_text);
    p.push_str("\n```\n\nNow output your findings as the JSON array described in your instructions.");
    p
}





pub fn extract_findings(text: &str) -> Result<Vec<Finding>> {

    if let Some(start) = text.find("```json") {
        let inner_start = start + "```json".len();
        if let Some(end) = text[inner_start..].find("```") {
            let inner = text[inner_start..inner_start + end].trim();
            if let Ok(v) = serde_json::from_str::<Vec<Finding>>(inner) {
                return Ok(v);
            }
        }
    }

    let mut search = text;
    while let Some(start) = search.find("```") {
        let inner_start = start + 3;

        let content_start = search[inner_start..]
            .find('\n')
            .map(|i| inner_start + i + 1)
            .unwrap_or(inner_start);
        if let Some(end) = search[content_start..].find("```") {
            let inner = search[content_start..content_start + end].trim();
            if inner.starts_with('[') {
                if let Ok(v) = serde_json::from_str::<Vec<Finding>>(inner) {
                    return Ok(v);
                }
            }
            search = &search[content_start + end..];
        } else {
            break;
        }
    }

    if let (Some(s), Some(e)) = (text.find('['), text.rfind(']')) {
        if s < e {
            let inner = &text[s..=e];
            if let Ok(v) = serde_json::from_str::<Vec<Finding>>(inner) {
                return Ok(v);
            }
        }
    }
    anyhow::bail!(
        "model output did not contain a parseable JSON findings array (output was {} chars)",
        text.len()
    )
}





pub const REVIEW_TOOLS: &[&str] = &["Read", "Glob", "Grep"];



pub fn read_only_registry() -> Result<ToolRegistry> {
    let mut registry = ToolRegistry::new();
    for t in zen_tools::builtin_tools() {
        if REVIEW_TOOLS.contains(&t.name()) {
            if !t.is_read_only() {
                anyhow::bail!(
                    "tool '{}' is not marked read-only; refusing to run review",
                    t.name()
                );
            }
            registry.register(t);
        }
    }
    for name in REVIEW_TOOLS {
        if registry.get(name).is_none() {
            anyhow::bail!("read-only tool '{name}' missing from registry; refusing to run review");
        }
    }
    Ok(registry)
}



struct ReviewPermissions;

impl PermissionChecker for ReviewPermissions {
    fn check(
        &self,
        tool_name: &str,
        _input: &serde_json::Value,
        read_only: bool,
        _ctx: &ToolContext,
    ) -> PermissionDecision {
        if read_only {
            PermissionDecision::Allow
        } else {
            PermissionDecision::Deny(format!(
                "zen review is read-only: tool '{tool_name}' is not permitted"
            ))
        }
    }
}



fn render_text(report: &ReviewReport, threshold: Severity) -> String {
    let mut out = String::new();
    out.push_str(&format!("zen review - {}\n", report.scope));
    out.push_str(&format!(
        "model: {} · findings: {}\n\n",
        report.model, report.summary.total
    ));
    if report.findings.is_empty() {
        out.push_str("No findings. Diff looks clean.\n");
        return out;
    }
    for sev in Severity::ordered() {
        let group: Vec<&Finding> = report
            .findings
            .iter()
            .filter(|f| f.severity == sev)
            .collect();
        if group.is_empty() {
            continue;
        }
        let marker = if sev.rank() >= threshold.rank() {
            " [fails --fail-on]"
        } else {
            ""
        };
        out.push_str(&format!("== {} ({}):{} ==\n", sev, group.len(), marker));
        for f in group {
            let loc = match f.line {
                Some(l) => format!("{}:{}", f.file, l),
                None => f.file.clone(),
            };
            out.push_str(&format!("- [{}] {} - {}\n", f.category, loc, f.title));
            if !f.detail.is_empty() {
                out.push_str(&format!("    {}\n", f.detail));
            }
            if !f.suggestion.is_empty() {
                out.push_str(&format!("    fix: {}\n", f.suggestion));
            }
        }
        out.push('\n');
    }
    out
}

fn render_json_value(report: &ReviewReport) -> serde_json::Value {
    serde_json::to_value(report).unwrap_or(serde_json::Value::Null)
}



fn print_stream_json(report: &ReviewReport, threshold: Severity, exit_code: i32) {
    let failed = !report.at_or_above(threshold).is_empty();
    println!(
        "{}",
        serde_json::json!({
            "type": "review",
            "event": "start",
            "scope": report.scope,
            "model": report.model,
        })
    );
    for f in &report.findings {
        println!(
            "{}",
            serde_json::json!({
                "type": "review",
                "event": "finding",
                "finding": f,
            })
        );
    }
    println!(
        "{}",
        serde_json::json!({
            "type": "review",
            "event": "complete",
            "summary": report.summary,
            "failed": failed,
            "exit_code": exit_code,
        })
    );
}






pub async fn run_review(
    cli: &Cli,
    cwd: &Path,
    home: Option<&Path>,
    args: &ReviewArgs,
) -> Result<i32> {
    let threshold = Severity::parse(&args.fail_on).unwrap_or_else(|| {
        eprintln!("warning: invalid --fail-on '{}', using 'high'", args.fail_on);
        Severity::High
    });

    let scope = match ReviewScope::from_args(args) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return Ok(2);
        }
    };

    let cfg = crate::settings::resolve_config_with_home(cli, cwd, home)?;


    let diff = match collect_diff(cwd, &scope, args.max_diff_bytes) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e:#}");
            return Ok(2);
        }
    };
    if diff.diff_text.trim().is_empty() {
        match args.output_format {
            OutputFormat::Text => {
                println!(
                    "zen review - {}: no changes to review.",
                    diff.scope_description
                );
            }
            OutputFormat::Json => {
                let report = ReviewReport::new(diff.scope_description, cfg.model.clone(), vec![]);
                println!("{}", serde_json::to_string_pretty(&render_json_value(&report))?);
            }
            OutputFormat::StreamJson => {
                let report = ReviewReport::new(diff.scope_description, cfg.model.clone(), vec![]);
                print_stream_json(&report, threshold, 0);
            }
        }
        return Ok(0);
    }

    if cfg.api_key().is_none() {
        eprintln!(
            "error: no API key configured (set it via `zen auth login` or env); cannot run review"
        );
        return Ok(2);
    }


    let registry = match read_only_registry() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e:#}");
            return Ok(2);
        }
    };
    let sink: Arc<dyn EventSink> = Arc::new(NullSink);
    let llm = cfg.llm_client(sink.clone());
    let session_id = format!("review-{}", uuid::Uuid::new_v4().hyphenated());
    let ctx = ToolContext::new(cwd.to_path_buf(), session_id.clone());
    let config = zen_agent::AgentConfig {
        model: cfg.model.clone(),
        system_prompt: review_system_prompt(),
        max_turns: 8,
        permission_mode: PermissionMode::Default,
        auto_compact_threshold: None,
        ..Default::default()
    };
    let mut agent = zen_agent::Agent::new(
        llm,
        registry,
        Arc::new(ReviewPermissions),
        None,
        Arc::new(NoHooks) as Arc<dyn HookRunner>,
        sink,
        ctx,
        config,
    );

    let result = agent.run_turn(&build_review_prompt(&diff)).await;
    if result.is_error {
        eprintln!(
            "error: review run failed ({}): {}",
            result.subtype, result.result
        );
        return Ok(2);
    }


    let findings = match extract_findings(&result.result) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: {e:#}");
            eprintln!("--- model output (unparseable) ---");
            eprintln!("{}", result.result);
            return Ok(2);
        }
    };
    let report = ReviewReport::new(diff.scope_description, cfg.model.clone(), findings);
    let failed = !report.at_or_above(threshold).is_empty();
    let exit_code = if failed { 1 } else { 0 };

    match args.output_format {
        OutputFormat::Text => {
            print!("{}", render_text(&report, threshold));
            if failed {
                println!(
                    "{} finding(s) at or above --fail-on={} → exit 1",
                    report.at_or_above(threshold).len(),
                    threshold
                );
            }
        }
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&render_json_value(&report))?);
        }
        OutputFormat::StreamJson => {
            print_stream_json(&report, threshold, exit_code);
        }
    }
    Ok(exit_code)
}



#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use clap::Parser;

    fn review_args(extra: &[&str]) -> ReviewArgs {

        let mut argv: Vec<String> = vec!["zen".to_string(), "review".to_string()];
        argv.extend(extra.iter().map(|s| s.to_string()));
        let cli = Cli::parse_from(argv);
        match cli.subcommand {
            Some(crate::cli::Commands::Review(a)) => a,
            other => panic!("expected review subcommand, got {other:?}"),
        }
    }

    fn git_env(cmd: &mut Command) {
        cmd.env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t");
    }

    fn git_repo() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zen_review_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut init = Command::new("git");
        git_env(&mut init);
        let out = init
            .args(["init", "-q", "-b", "main"])
            .current_dir(&dir)
            .output()
            .expect("git failed");
        assert!(out.status.success());
        std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
        let mut add = Command::new("git");
        git_env(&mut add);
        assert!(add
            .args(["add", "."])
            .current_dir(&dir)
            .output()
            .unwrap()
            .status
            .success());
        let mut commit = Command::new("git");
        git_env(&mut commit);
        assert!(commit
            .args(["commit", "-qm", "init"])
            .current_dir(&dir)
            .output()
            .unwrap()
            .status
            .success());
        dir
    }

    fn git_in(dir: &Path, args: &[&str]) {
        let mut cmd = Command::new("git");
        git_env(&mut cmd);
        let out = cmd.args(args).current_dir(dir).output().expect("git failed");
        assert!(out.status.success(), "git {args:?} failed");
    }



    #[test]
    fn severity_parse_all_forms() {
        assert_eq!(Severity::parse("critical"), Some(Severity::Critical));
        assert_eq!(Severity::parse("HIGH"), Some(Severity::High));
        assert_eq!(Severity::parse("p2"), Some(Severity::Medium));
        assert_eq!(Severity::parse("low"), Some(Severity::Low));
        assert_eq!(Severity::parse("nit"), Some(Severity::Nit));
        assert_eq!(Severity::parse("bogus"), None);
    }

    #[test]
    fn severity_rank_ordering_for_thresholds() {
        assert!(Severity::Critical.rank() > Severity::High.rank());
        assert!(Severity::High.rank() > Severity::Medium.rank());
        assert!(Severity::Medium.rank() > Severity::Low.rank());
        assert!(Severity::Low.rank() > Severity::Nit.rank());
    }

    #[test]
    fn at_or_above_filters_correctly() {
        let mk = |s| Finding {
            severity: s,
            category: "bugs".into(),
            file: "f.rs".into(),
            line: Some(1),
            title: "t".into(),
            detail: "".into(),
            suggestion: "".into(),
        };
        let report = ReviewReport::new(
            "s".into(),
            "m".into(),
            vec![mk(Severity::Critical), mk(Severity::Medium), mk(Severity::Nit)],
        );
        assert_eq!(report.at_or_above(Severity::High).len(), 1);
        assert_eq!(report.at_or_above(Severity::Medium).len(), 2);
        assert_eq!(report.at_or_above(Severity::Nit).len(), 3);
        assert_eq!(report.summary.total, 3);
        assert_eq!(report.summary.critical, 1);
        assert_eq!(report.summary.max_severity, Some(Severity::Critical));
    }



    #[test]
    fn parse_review_defaults() {
        let a = review_args(&[]);
        assert!(a.base.is_none() && a.commit.is_none() && !a.uncommitted);
        assert_eq!(a.fail_on, "high");
        assert_eq!(a.output_format, OutputFormat::Text);
    }

    #[test]
    fn parse_review_scope_flags() {
        let a = review_args(&["--base", "main"]);
        assert_eq!(a.base.as_deref(), Some("main"));
        let a = review_args(&["--commit", "abc123"]);
        assert_eq!(a.commit.as_deref(), Some("abc123"));
        let a = review_args(&[
            "--uncommitted",
            "--fail-on",
            "medium",
            "--output-format",
            "json",
        ]);
        assert!(a.uncommitted);
        assert_eq!(a.fail_on, "medium");
        assert_eq!(a.output_format, OutputFormat::Json);
    }

    #[test]
    fn parse_review_stream_json_format() {
        let a = review_args(&["--output-format", "stream-json"]);
        assert_eq!(a.output_format, OutputFormat::StreamJson);
    }

    #[test]
    fn scope_rejects_base_and_commit_together() {
        let a = review_args(&["--base", "main", "--commit", "abc"]);
        assert!(ReviewScope::from_args(&a).is_err());
    }

    #[test]
    fn scope_defaults_to_uncommitted() {
        let a = review_args(&[]);
        assert!(matches!(
            ReviewScope::from_args(&a).unwrap(),
            ReviewScope::Uncommitted
        ));
    }



    #[test]
    fn collect_uncommitted_diff() {
        let dir = git_repo();
        std::fs::write(dir.join("a.txt"), "hello\nworld\n").unwrap();
        let diff = collect_diff(&dir, &ReviewScope::Uncommitted, 1_000_000).unwrap();
        assert!(diff.diff_text.contains("world"));
        assert!(diff.files.iter().any(|f| f == "a.txt"));
        assert!(!diff.truncated);
    }

    #[test]
    fn collect_uncommitted_includes_untracked() {
        let dir = git_repo();
        std::fs::write(dir.join("new.rs"), "fn main() {}\n").unwrap();
        let diff = collect_diff(&dir, &ReviewScope::Uncommitted, 1_000_000).unwrap();
        assert!(diff.diff_text.contains("new file: new.rs"));
        assert!(diff.diff_text.contains("fn main()"));
        assert!(diff.files.iter().any(|f| f == "new.rs"));
    }

    #[test]
    fn collect_base_diff() {
        let dir = git_repo();
        git_in(&dir, &["checkout", "-qb", "feature"]);
        std::fs::write(dir.join("b.txt"), "feature work\n").unwrap();
        git_in(&dir, &["add", "."]);
        git_in(&dir, &["commit", "-qm", "feat"]);
        let diff = collect_diff(&dir, &ReviewScope::Base("main".into()), 1_000_000).unwrap();
        assert!(diff.diff_text.contains("feature work"));
    }

    #[test]
    fn collect_commit_diff() {
        let dir = git_repo();
        std::fs::write(dir.join("a.txt"), "hello\nchanged\n").unwrap();
        git_in(&dir, &["add", "."]);
        git_in(&dir, &["commit", "-qm", "change"]);
        let sha = {
            let mut cmd = Command::new("git");
            git_env(&mut cmd);
            String::from_utf8(
                cmd.args(["rev-parse", "HEAD"])
                    .current_dir(&dir)
                    .output()
                    .unwrap()
                    .stdout,
            )
            .unwrap()
        };
        let diff =
            collect_diff(&dir, &ReviewScope::Commit(sha.trim().to_string()), 1_000_000).unwrap();
        assert!(diff.diff_text.contains("changed"));
    }

    #[test]
    fn collect_diff_truncates_large_diffs() {
        let dir = git_repo();
        let big = "x".repeat(5000);
        std::fs::write(dir.join("a.txt"), format!("hello\n{big}\n")).unwrap();
        let diff = collect_diff(&dir, &ReviewScope::Uncommitted, 100).unwrap();
        assert!(diff.truncated);
        assert!(diff.diff_text.contains("truncated"));
    }

    #[test]
    fn collect_diff_outside_repo_errors() {
        let dir = std::env::temp_dir().join(format!("zen_review_norepo_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(collect_diff(&dir, &ReviewScope::Uncommitted, 1_000_000).is_err());
    }

    #[test]
    fn collect_diff_bad_base_errors() {
        let dir = git_repo();
        assert!(collect_diff(
            &dir,
            &ReviewScope::Base("no-such-branch-xyz".into()),
            1_000_000
        )
        .is_err());
    }



    #[test]
    fn registry_contains_only_read_tools() {
        let r = read_only_registry().unwrap();
        let names = r.names();
        assert_eq!(names.len(), 3);
        for n in ["Read", "Glob", "Grep"] {
            assert!(names.contains(&n.to_string()), "missing {n}");
        }

        for n in ["Write", "Edit", "MultiEdit", "Bash", "WebFetch", "TodoWrite"] {
            assert!(r.get(n).is_none(), "write tool {n} must not be registered");
        }

        for t in zen_tools::builtin_tools() {
            if REVIEW_TOOLS.contains(&t.name()) {
                assert!(t.is_read_only(), "{} must be read-only", t.name());
            }
        }
    }

    #[test]
    fn review_permissions_deny_non_readonly() {
        use zen_core::PermissionChecker;
        let p = ReviewPermissions;
        let ctx = ToolContext::new(PathBuf::from("/tmp"), "s".to_string());
        let v = serde_json::json!({});
        assert!(matches!(
            p.check("Read", &v, true, &ctx),
            PermissionDecision::Allow
        ));
        assert!(matches!(
            p.check("Write", &v, false, &ctx),
            PermissionDecision::Deny(_)
        ));
        assert!(matches!(
            p.check("Bash", &v, false, &ctx),
            PermissionDecision::Deny(_)
        ));
    }



    #[test]
    fn extract_from_json_fence() {
        let text = "Here you go:\n```json\n[{\"severity\":\"high\",\"category\":\"bugs\",\"file\":\"a.rs\",\"line\":3,\"title\":\"oops\",\"detail\":\"d\",\"suggestion\":\"s\"}]\n```\n";
        let f = extract_findings(text).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::High);
        assert_eq!(f[0].file, "a.rs");
        assert_eq!(f[0].line, Some(3));
    }

    #[test]
    fn extract_bare_array() {
        let text = r#"[{"severity":"nit","file":"b.rs","title":"t"}]"#;
        let f = extract_findings(text).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::Nit);
        assert_eq!(f[0].line, None);
        assert_eq!(f[0].category, "bugs");
    }

    #[test]
    fn extract_empty_array_means_clean() {
        let f = extract_findings("```json\n[]\n```").unwrap();
        assert!(f.is_empty());
    }

    #[test]
    fn extract_garbage_errors() {
        assert!(extract_findings("looks fine to me, ship it").is_err());
        assert!(extract_findings("```json\nnot json\n```").is_err());
    }



    #[test]
    fn json_shape_has_summary_and_findings() {
        let f = Finding {
            severity: Severity::Medium,
            category: "perf".into(),
            file: "x.rs".into(),
            line: None,
            title: "slow".into(),
            detail: "d".into(),
            suggestion: "s".into(),
        };
        let report = ReviewReport::new("s".into(), "m".into(), vec![f]);
        let v = render_json_value(&report);
        assert_eq!(v["summary"]["total"], 1);
        assert_eq!(v["summary"]["medium"], 1);
        assert_eq!(v["findings"][0]["severity"], "medium");
        assert_eq!(v["findings"][0]["file"], "x.rs");
        assert!(v["findings"][0]["line"].is_null());
    }

    #[test]
    fn text_groups_by_severity() {
        let mk = |s: Severity, t: &str| Finding {
            severity: s,
            category: "bugs".into(),
            file: "f.rs".into(),
            line: Some(1),
            title: t.into(),
            detail: "".into(),
            suggestion: "".into(),
        };
        let report = ReviewReport::new(
            "s".into(),
            "m".into(),
            vec![mk(Severity::Low, "l"), mk(Severity::Critical, "c")],
        );
        let text = render_text(&report, Severity::High);
        let crit_pos = text.find("critical").unwrap();
        let low_pos = text.find("low (1)").unwrap();
        assert!(crit_pos < low_pos, "critical group must come first");
        assert!(text.contains("[fails --fail-on]"));
    }



    #[test]
    fn system_prompt_demands_json_only() {
        let p = review_system_prompt();
        assert!(p.contains("```json"));
        assert!(p.contains("CANNOT modify"));
        assert!(p.contains("critical"));
    }

    #[test]
    fn user_prompt_embeds_diff_and_files() {
        let diff = CollectedDiff {
            scope_description: "uncommitted changes".into(),
            files: vec!["a.rs".into()],
            diff_text: "diff --git a/a.rs b/a.rs".into(),
            truncated: false,
        };
        let p = build_review_prompt(&diff);
        assert!(p.contains("a.rs"));
        assert!(p.contains("```diff"));
    }
}
