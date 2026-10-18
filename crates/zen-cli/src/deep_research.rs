//! `/deep-research` agentic workflow (parity item H7 — Claude Code's `/deep-research`).
//!
//! A `/deep-research <topic>` REPL command that fans out N parallel research
//! sub-agents (via the `Task` tool already in the agent's registry), shows live
//! progress as each finishes, then asks the main model to synthesize the
//! findings into a cited markdown report.
//!
//! Budget control is deterministic, because dollar-accurate tracking is not on
//! `main` yet (cost ledger = PR #40, loop-level spend cap = PR #53):
//!
//! - `--max-subagents N` (default 4, hard cap 8) bounds parallelism;
//! - `--timeout-secs N` (default 600) bounds each researcher;
//! - `--max-budget-usd X` (or `ZEN_MAX_BUDGET_USD`) runs a pre-flight check
//!   against a deliberately conservative estimate. Once PR #53 merges, its
//!   agent-loop pre-flight composes automatically: researchers run through
//!   `run_turn`, which will enforce the shared session cap.

use anyhow::Result;
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Default researcher count: one per research angle.
pub const DEFAULT_MAX_SUBAGENTS: usize = 4;
/// Hard cap: never fan out more researchers than this, whatever the user asks.
pub const MAX_SUBAGENTS_HARD_CAP: usize = 8;
/// Default per-researcher wall-clock budget.
pub const DEFAULT_RESEARCHER_TIMEOUT_SECS: u64 = 600;
/// Minimum sane per-researcher timeout enforced at parse time.
pub const MIN_RESEARCHER_TIMEOUT_SECS: u64 = 30;
/// Raw findings are truncated to this many chars in prompts and the appendix.
pub const MAX_FINDING_CHARS: usize = 4000;

/// Conservative pre-flight cost ceiling per researcher, USD. Deliberately an
/// over-estimate until the cost ledger (PR #40) provides real pricing.
pub const ESTIMATED_USD_PER_RESEARCHER: f64 = 0.25;
/// Same, for the final synthesis turn.
pub const ESTIMATED_USD_SYNTHESIS: f64 = 0.10;

/// Parsed `/deep-research` options.
#[derive(Debug, Clone, PartialEq)]
pub struct DeepResearchOptions {
    pub topic: String,
    pub max_subagents: usize,
    pub researcher_timeout_secs: u64,
    pub max_budget_usd: Option<f64>,
}

/// Split an arg string on whitespace, respecting double quotes so multi-word
/// topics work: `/deep-research "prompt caching" --max-subagents 2`.
fn tokenize(args: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut has_token = false;
    for ch in args.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                has_token = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if has_token {
                    out.push(std::mem::take(&mut cur));
                    has_token = false;
                }
            }
            c => {
                cur.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        out.push(cur);
    }
    out
}

/// Parse `/deep-research <topic> [--max-subagents N] [--timeout-secs N] [--max-budget-usd X]`.
pub fn parse_deep_research_args(args: &str) -> Result<DeepResearchOptions, String> {
    let tokens = tokenize(args);
    let mut topic_parts: Vec<String> = Vec::new();
    let mut max_subagents = DEFAULT_MAX_SUBAGENTS;
    let mut timeout_secs = DEFAULT_RESEARCHER_TIMEOUT_SECS;
    let mut max_budget_usd: Option<f64> = None;
    let mut i = 0;
    while i < tokens.len() {
        match tokens[i].as_str() {
            "--max-subagents" => {
                i += 1;
                let v = tokens.get(i).ok_or("--max-subagents needs a value")?;
                max_subagents = v
                    .parse::<usize>()
                    .map_err(|_| format!("bad --max-subagents value: {v}"))?;
            }
            "--timeout-secs" => {
                i += 1;
                let v = tokens.get(i).ok_or("--timeout-secs needs a value")?;
                timeout_secs = v
                    .parse::<u64>()
                    .map_err(|_| format!("bad --timeout-secs value: {v}"))?;
            }
            "--max-budget-usd" => {
                i += 1;
                let v = tokens.get(i).ok_or("--max-budget-usd needs a value")?;
                let f: f64 = v
                    .parse()
                    .map_err(|_| format!("bad --max-budget-usd value: {v}"))?;
                if !f.is_finite() || f <= 0.0 {
                    return Err("--max-budget-usd must be positive".into());
                }
                max_budget_usd = Some(f);
            }
            other if other.starts_with("--") => return Err(format!("unknown flag: {other}")),
            _ => topic_parts.push(tokens[i].clone()),
        }
        i += 1;
    }
    if topic_parts.is_empty() {
        return Err(
            "usage: /deep-research <topic> [--max-subagents N] [--timeout-secs N] [--max-budget-usd X]"
                .into(),
        );
    }
    if max_subagents == 0 {
        return Err("--max-subagents must be at least 1".into());
    }
    Ok(DeepResearchOptions {
        topic: topic_parts.join(" "),
        max_subagents: max_subagents.min(MAX_SUBAGENTS_HARD_CAP),
        researcher_timeout_secs: timeout_secs.max(MIN_RESEARCHER_TIMEOUT_SECS),
        max_budget_usd,
    })
}

/// One research angle: a named lens plus the brief handed to its researcher.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResearchAngle {
    pub name: &'static str,
    pub title: &'static str,
    pub brief: &'static str,
}

/// The four fixed angles, in fan-out order.
pub static RESEARCH_ANGLES: &[ResearchAngle] = &[
    ResearchAngle {
        name: "docs",
        title: "Official docs & specs",
        brief: "Find the authoritative documentation, specifications, READMEs and API \
                references. Prefer primary sources over blog posts and tutorials.",
    },
    ResearchAngle {
        name: "code",
        title: "Codebase exploration",
        brief: "Explore the local codebase with Read/Glob/Grep for how the topic is \
                implemented, used, or tested here. If the topic is unrelated to this \
                repository, say so in one sentence and stop.",
    },
    ResearchAngle {
        name: "web",
        title: "Web research",
        brief: "Use WebSearch for current information: recent releases, changelogs, \
                community discussion, known issues. Note dates for time-sensitive claims.",
    },
    ResearchAngle {
        name: "competitors",
        title: "Alternatives & comparison",
        brief: "Research alternatives and competitors: what else solves this problem, \
                how they compare on the dimensions users care about, and when each wins.",
    },
];

/// First `max` angles (capped at the number of defined angles).
pub fn research_angles(max: usize) -> Vec<&'static ResearchAngle> {
    RESEARCH_ANGLES.iter().take(max.min(RESEARCH_ANGLES.len())).collect()
}

/// The self-contained prompt handed to one researcher sub-agent.
pub fn researcher_prompt(topic: &str, angle: &ResearchAngle) -> String {
    format!(
        "You are a research sub-agent. Your angle: **{title}**.\n\
         \n\
         Topic: {topic}\n\
         \n\
         {brief}\n\
         \n\
         Rules:\n\
         - Work independently; do not ask follow-up questions.\n\
         - Be thorough but efficient: aim to finish in a handful of tool calls.\n\
         - Cite every factual claim inline as [source: <url or file path>].\n\
         - End with a \"Key findings\" list (3-8 bullets) and a one-line confidence \
         note (high/medium/low) per finding.\n\
         - Reply with your findings as plain text; no preamble about your role.",
        title = angle.title,
        topic = topic,
        brief = angle.brief,
    )
}

/// Conservative pre-flight cost estimate for a research run, USD.
pub fn estimate_cost_usd(max_subagents: usize) -> f64 {
    max_subagents as f64 * ESTIMATED_USD_PER_RESEARCHER + ESTIMATED_USD_SYNTHESIS
}

/// Pre-flight budget gate. `None` = no cap. The estimate is deliberately
/// conservative; the hard guards are the subagent count and timeout.
pub fn check_budget_usd(max_budget_usd: Option<f64>, max_subagents: usize) -> Result<(), String> {
    if let Some(cap) = max_budget_usd {
        let est = estimate_cost_usd(max_subagents);
        if est > cap {
            return Err(format!(
                "estimated deep-research cost ${est:.2} exceeds the ${cap:.2} budget \
                 (conservative estimate: ${per:.2}/researcher + ${syn:.2} synthesis). \
                 Lower --max-subagents or raise the budget.",
                est = est,
                cap = cap,
                per = ESTIMATED_USD_PER_RESEARCHER,
                syn = ESTIMATED_USD_SYNTHESIS,
            ));
        }
    }
    Ok(())
}

/// Effective budget: explicit flag wins, then `ZEN_MAX_BUDGET_USD`.
/// (A settings-file `maxBudgetUsd` composes with PR #53's loop-level cap once merged.)
pub fn resolve_budget_usd(flag: Option<f64>) -> Option<f64> {
    if flag.is_some() {
        return flag;
    }
    std::env::var("ZEN_MAX_BUDGET_USD")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|f| *f > 0.0)
}

/// One finished researcher.
#[derive(Debug, Clone)]
pub struct ResearchFinding {
    pub angle_name: String,
    pub angle_title: String,
    pub text: String,
    pub ok: bool,
    pub elapsed_secs: u64,
}

/// Fan out one `Task`-tool call per angle, concurrently. `on_progress` fires in
/// completion order; findings are returned in angle order (stable for reports).
pub async fn fan_out_research(
    task_tool: &Arc<dyn zen_core::Tool>,
    ctx: &zen_core::ToolContext,
    topic: &str,
    opts: &DeepResearchOptions,
    on_progress: &dyn Fn(String),
) -> Vec<ResearchFinding> {
    let topic = topic.to_string();
    let angles = research_angles(opts.max_subagents);
    let total = angles.len();
    let mut set = tokio::task::JoinSet::new();
    for (idx, angle) in angles.iter().enumerate() {
        let task = Arc::clone(task_tool);
        let ctx = ctx.clone();
        let prompt = researcher_prompt(&topic, angle);
        let name = angle.name.to_string();
        let title = angle.title.to_string();
        let description = format!("Research {name}: {topic}");
        let timeout = Duration::from_secs(opts.researcher_timeout_secs);
        set.spawn(async move {
            let start = Instant::now();
            let args = json!({
                "description": description,
                "prompt": prompt,
                "subagent_type": "general-purpose",
            });
            let outcome = tokio::time::timeout(timeout, task.call(args, &ctx)).await;
            let elapsed_secs = start.elapsed().as_secs();
            let (text, ok) = match outcome {
                Ok(out) => (out.content, !out.is_error),
                Err(_) => (
                    format!("(researcher timed out after {}s)", timeout.as_secs()),
                    false,
                ),
            };
            (
                idx,
                ResearchFinding {
                    angle_name: name,
                    angle_title: title,
                    text,
                    ok,
                    elapsed_secs,
                },
            )
        });
    }
    let mut indexed: Vec<(usize, ResearchFinding)> = Vec::new();
    while let Some(res) = set.join_next().await {
        match res {
            Ok((idx, finding)) => {
                on_progress(format!(
                    "[{}/{}] '{}' {} ({}s)",
                    indexed.len() + 1,
                    total,
                    finding.angle_title,
                    if finding.ok { "done" } else { "FAILED" },
                    finding.elapsed_secs,
                ));
                indexed.push((idx, finding));
            }
            Err(e) => {
                on_progress(format!("researcher task panicked: {e}"));
            }
        }
    }
    indexed.sort_by_key(|(i, _)| *i);
    indexed.into_iter().map(|(_, f)| f).collect()
}

/// Prompt for the parent model to synthesize the researchers' reports into the
/// final cited report.
pub fn synthesis_prompt(topic: &str, findings: &[ResearchFinding]) -> String {
    let mut p = format!(
        "Synthesize the following {n} parallel research reports on \"{topic}\" into ONE cited markdown report.\n\n",
        n = findings.len(),
        topic = topic,
    );
    p.push_str(
        "Write markdown with EXACTLY these sections, in this order:\n\
         ## Summary\n\
         ## Findings\n\
         ## Sources\n\
         ## Confidence\n\
         ## Open questions\n\
         \n\
         Rules:\n\
         - ## Summary: 3-6 sentences with the essential answer.\n\
         - ## Findings: group by theme (not by researcher); merge duplicates.\n\
         - ## Sources: every distinct [source: ...] citation from the reports, deduplicated.\n\
         - ## Confidence: the key claims with High/Medium/Low and one line why.\n\
         - ## Open questions: what the research could not answer.\n\
         - Drop researcher sections that failed or timed out; note it under Open questions if material.\n\
         \n\
         --- researcher reports ---\n\n",
    );
    for f in findings {
        p.push_str(&format!(
            "### {} ({})\n{}\n\n",
            f.angle_title,
            if f.ok { "ok" } else { "FAILED" },
            truncate(&f.text, MAX_FINDING_CHARS),
        ));
    }
    p
}

/// Truncate to a char boundary, noting the original length.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…\n\n(truncated from {} chars)", &s[..end], s.len())
}

/// Assemble the final markdown: metadata header + model synthesis + raw appendix.
pub fn render_report(
    topic: &str,
    model: &str,
    opts: &DeepResearchOptions,
    findings: &[ResearchFinding],
    synthesis: &str,
) -> String {
    let mut out = format!(
        "# Deep Research: {topic}\n\n_Generated {ts} · {n} researcher(s) · model `{model}` · \
         {t}s timeout/researcher_\n\n",
        topic = topic,
        ts = Utc::now().format("%Y-%m-%d %H:%M UTC"),
        n = findings.len(),
        model = model,
        t = opts.researcher_timeout_secs,
    );
    out.push_str(synthesis.trim());
    out.push_str("\n\n---\n\n## Appendix: raw researcher output\n");
    for f in findings {
        out.push_str(&format!(
            "\n### {} — `{}` ({}s){}\n\n{}\n",
            f.angle_title,
            f.angle_name,
            f.elapsed_secs,
            if f.ok { "" } else { " — FAILED" },
            truncate(&f.text, MAX_FINDING_CHARS),
        ));
    }
    out
}

/// Mechanical fallback when the synthesis turn fails or is interrupted: the
/// findings are still delivered, unmerged, in the same section skeleton.
pub fn render_fallback_report(
    topic: &str,
    model: &str,
    opts: &DeepResearchOptions,
    findings: &[ResearchFinding],
    why: &str,
) -> String {
    let body = format!(
        "## Summary\n\n_Automated synthesis unavailable ({why}); findings below are unmerged._\n\n\
         ## Findings\n\n(See the per-researcher reports in the appendix.)\n\n\
         ## Sources\n\nSee per-researcher citations in the appendix.\n\n\
         ## Confidence\n\nUnassessed — synthesis did not run.\n\n\
         ## Open questions\n\n- Re-run synthesis to merge these findings into a single narrative.\n"
    );
    render_report(topic, model, opts, findings, &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn test_opts() -> DeepResearchOptions {
        DeepResearchOptions {
            topic: "prompt caching".into(),
            max_subagents: 4,
            researcher_timeout_secs: 600,
            max_budget_usd: None,
        }
    }

    #[test]
    fn parse_defaults() {
        let o = parse_deep_research_args("rust async").unwrap();
        assert_eq!(o.topic, "rust async");
        assert_eq!(o.max_subagents, DEFAULT_MAX_SUBAGENTS);
        assert_eq!(o.researcher_timeout_secs, DEFAULT_RESEARCHER_TIMEOUT_SECS);
        assert_eq!(o.max_budget_usd, None);
    }

    #[test]
    fn parse_quoted_topic_and_flags() {
        let o = parse_deep_research_args("\"prompt caching\" --max-subagents 2 --timeout-secs 120 --max-budget-usd 1.5")
            .unwrap();
        assert_eq!(o.topic, "prompt caching");
        assert_eq!(o.max_subagents, 2);
        assert_eq!(o.researcher_timeout_secs, 120);
        assert_eq!(o.max_budget_usd, Some(1.5));
    }

    #[test]
    fn parse_clamps_huge_fanout() {
        let o = parse_deep_research_args("x --max-subagents 99").unwrap();
        assert_eq!(o.max_subagents, MAX_SUBAGENTS_HARD_CAP);
    }

    #[test]
    fn parse_rejects_bad_input() {
        assert!(parse_deep_research_args("").is_err());
        assert!(parse_deep_research_args("--max-subagents 2").is_err()); // no topic
        assert!(parse_deep_research_args("x --max-subagents 0").is_err());
        assert!(parse_deep_research_args("x --max-subagents abc").is_err());
        assert!(parse_deep_research_args("x --bogus-flag 1").is_err());
        assert!(parse_deep_research_args("x --max-budget-usd -2").is_err());
        assert!(parse_deep_research_args("x --max-budget-usd 0").is_err());
    }

    #[test]
    fn parse_enforces_min_timeout() {
        let o = parse_deep_research_args("x --timeout-secs 5").unwrap();
        assert_eq!(o.researcher_timeout_secs, MIN_RESEARCHER_TIMEOUT_SECS);
    }

    #[test]
    fn angles_are_distinct_and_capped() {
        let four = research_angles(4);
        assert_eq!(four.len(), 4);
        let names: Vec<&str> = four.iter().map(|a| a.name).collect();
        let mut dedup = names.clone();
        dedup.sort_unstable();
        dedup.dedup();
        assert_eq!(names.len(), dedup.len(), "angle names must be distinct");
        assert_eq!(research_angles(2).len(), 2);
        assert_eq!(research_angles(99).len(), RESEARCH_ANGLES.len());
    }

    #[test]
    fn researcher_prompt_covers_topic_and_citations() {
        let angle = &RESEARCH_ANGLES[0];
        let p = researcher_prompt("vector databases", angle);
        assert!(p.contains("vector databases"));
        assert!(p.contains(angle.title));
        assert!(p.contains("[source:"), "researchers must be told the citation format");
        assert!(p.contains("do not ask follow-up questions"));
    }

    #[test]
    fn budget_gate_math() {
        assert!((estimate_cost_usd(4) - (4.0 * ESTIMATED_USD_PER_RESEARCHER + ESTIMATED_USD_SYNTHESIS)).abs() < 1e-9);
        assert!(check_budget_usd(None, 4).is_ok());
        assert!(check_budget_usd(Some(10.0), 4).is_ok());
        let err = check_budget_usd(Some(0.5), 4).unwrap_err();
        assert!(err.contains("exceeds"), "unexpected: {err}");
        // exact boundary is allowed
        let exact = estimate_cost_usd(4);
        assert!(check_budget_usd(Some(exact), 4).is_ok());
    }

    #[test]
    fn synthesis_prompt_has_required_sections() {
        let findings = vec![ResearchFinding {
            angle_name: "web".into(),
            angle_title: "Web research".into(),
            text: "fact [source: https://example.com]".into(),
            ok: true,
            elapsed_secs: 3,
        }];
        let p = synthesis_prompt("topic", &findings);
        for section in ["## Summary", "## Findings", "## Sources", "## Confidence", "## Open questions"] {
            assert!(p.contains(section), "missing {section}");
        }
        assert!(p.contains("topic"));
        assert!(p.contains("https://example.com"));
    }

    #[test]
    fn report_structure() {
        let opts = test_opts();
        let findings = vec![ResearchFinding {
            angle_name: "docs".into(),
            angle_title: "Official docs & specs".into(),
            text: "some finding [source: https://docs.example]".into(),
            ok: true,
            elapsed_secs: 12,
        }];
        let r = render_report("my topic", "test-model", &opts, &findings, "## Summary\n\nAll good.");
        assert!(r.starts_with("# Deep Research: my topic"));
        assert!(r.contains("test-model"));
        assert!(r.contains("## Summary\n\nAll good."));
        assert!(r.contains("## Appendix: raw researcher output"));
        assert!(r.contains("Official docs & specs"));
        let f = render_fallback_report("my topic", "test-model", &opts, &findings, "boom");
        assert!(f.contains("Automated synthesis unavailable (boom)"));
        assert!(f.contains("## Open questions"));
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let s = "héllo wörld — emoji 🎉 done";
        let t = truncate(s, 12);
        assert!(t.len() < s.len() + 60);
        assert!(t.contains("truncated from"));
        assert_eq!(truncate("short", 100), "short");
    }

    /// Fake `Task` tool: counts calls, tracks max concurrency, can fail per angle.
    struct FakeTask {
        calls: Arc<AtomicUsize>,
        inflight: Arc<AtomicUsize>,
        max_inflight: Arc<AtomicUsize>,
        delay_ms: u64,
        fail_substr: Option<String>,
    }

    #[async_trait::async_trait]
    impl zen_core::Tool for FakeTask {
        fn name(&self) -> &str {
            "Task"
        }
        fn description(&self) -> &str {
            "fake task tool for tests"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn is_read_only(&self) -> bool {
            true
        }
        async fn call(&self, args: serde_json::Value, _ctx: &zen_core::ToolContext) -> zen_core::ToolOutput {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let cur = self.inflight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_inflight.fetch_max(cur, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
            self.inflight.fetch_sub(1, Ordering::SeqCst);
            let desc = args.get("description").and_then(|v| v.as_str()).unwrap_or("");
            if self.fail_substr.as_ref().is_some_and(|f| desc.contains(f)) {
                return zen_core::ToolOutput::err("boom");
            }
            // prompt must carry the citation instruction through
            let prompt = args.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
            assert!(prompt.contains("[source:"), "researcher prompt lost citation format");
            zen_core::ToolOutput::ok(format!("finding #{n} [source: https://example.com/{n}]"))
        }
    }

    fn fake_tool(delay_ms: u64, fail_substr: Option<String>) -> (Arc<dyn zen_core::Tool>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let inflight = Arc::new(AtomicUsize::new(0));
        let max_inflight = Arc::new(AtomicUsize::new(0));
        let tool: Arc<dyn zen_core::Tool> = Arc::new(FakeTask {
            calls: calls.clone(),
            inflight: inflight.clone(),
            max_inflight: max_inflight.clone(),
            delay_ms,
            fail_substr,
        });
        (tool, calls, max_inflight)
    }

    fn test_ctx() -> zen_core::ToolContext {
        zen_core::ToolContext::new(std::env::temp_dir(), "deep-research-test")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn fan_out_runs_all_angles_concurrently_in_order() {
        use std::sync::Mutex;
        let (tool, calls, max_inflight) = fake_tool(120, None);
        let mut opts = test_opts();
        opts.max_subagents = 3;
        let progress: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let progress_cb = {
            let progress = progress.clone();
            move |s: String| {
                progress.lock().unwrap().push(s);
            }
        };
        let findings = fan_out_research(&tool, &test_ctx(), "topic", &opts, &progress_cb).await;
        assert_eq!(calls.load(Ordering::SeqCst), 3, "one Task call per angle");
        assert_eq!(findings.len(), 3);
        assert!(findings.iter().all(|f| f.ok));
        // angle order, not completion order
        let names: Vec<&str> = findings.iter().map(|f| f.angle_name.as_str()).collect();
        assert_eq!(names, vec!["docs", "code", "web"]);
        assert_eq!(progress.lock().unwrap().len(), 3, "progress fires per researcher");
        assert!(
            max_inflight.load(Ordering::SeqCst) >= 2,
            "researchers should overlap (max inflight = {})",
            max_inflight.load(Ordering::SeqCst)
        );
        assert!(findings[0].text.contains("[source: https://example.com/"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fan_out_marks_failures_without_aborting() {
        let (tool, calls, _) = fake_tool(10, Some("code".into()));
        let mut opts = test_opts();
        opts.max_subagents = 2; // docs + code; code fails
        let findings = fan_out_research(&tool, &test_ctx(), "topic", &opts, &|_| {}).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(findings.len(), 2);
        assert!(findings[0].ok, "docs should succeed");
        assert!(!findings[1].ok, "code should be marked failed");
        assert!(findings[1].text.contains("boom"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fan_out_timeout_marks_finding_failed() {
        let (tool, _, _) = fake_tool(30_000, None);
        let mut opts = test_opts();
        opts.max_subagents = 1;
        opts.researcher_timeout_secs = 1; // bypass parse-time minimum on purpose
        let findings = fan_out_research(&tool, &test_ctx(), "topic", &opts, &|_| {}).await;
        assert_eq!(findings.len(), 1);
        assert!(!findings[0].ok);
        assert!(findings[0].text.contains("timed out"));
    }

    #[test]
    fn resolve_budget_prefers_flag_then_env() {
        assert_eq!(resolve_budget_usd(Some(2.0)), Some(2.0));
        // env fallback: set a temp var via a scoped guard
        std::env::set_var("ZEN_MAX_BUDGET_USD", "3.25");
        assert_eq!(resolve_budget_usd(None), Some(3.25));
        std::env::remove_var("ZEN_MAX_BUDGET_USD");
        assert_eq!(resolve_budget_usd(None), None);
        std::env::set_var("ZEN_MAX_BUDGET_USD", "not-a-number");
        assert_eq!(resolve_budget_usd(None), None);
        std::env::remove_var("ZEN_MAX_BUDGET_USD");
    }
}
