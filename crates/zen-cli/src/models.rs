//! `zen models` — free-model auto-discovery (roadmap v0.6.0 "The Free Agent").
//!
//! Free-model catalogs (OpenRouter `:free`, etc.) rotate constantly, so a static
//! "best free model" list goes stale on publish day. This module probes the
//! configured provider's candidates with tiny micro-tasks and caches which
//! models actually answer *right now*.
//!
//! Design notes:
//! - Probing is written against `zen_core::LlmBackend` so unit tests use a mock
//!   (no network). The real [`ProbeClient`] is a thin reqwest wrapper with a
//!   short timeout and **no retries** — a probe must fail fast, not hang.
//! - Probing is deliberately sequential with a small gap between models: free
//!   tiers are rate-limited and hammering them would be rude (and get us
//!   429'd, which proves nothing).
//! - A model is only *recommended* when every micro-task succeeded. A model
//!   that failed any probe is shown as failed and never recommended.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use zen_core::{ChatMessage, ChatRequest, ChatResponse, LlmBackend, Usage};

// ───────────────────────── static catalog ─────────────────────────

/// A model known to be free on a major free gateway (Oct 2026). Best-effort
/// metadata; the live `/models` endpoint is the source of truth for what the
/// configured provider actually serves.
pub struct CatalogModel {
    pub id: &'static str,
    pub provider: &'static str,
    pub context_window: u32,
}

pub const FREE_MODEL_CATALOG: &[CatalogModel] = &[
    CatalogModel { id: "google/gemini-4-argon:free", provider: "experientiallabs", context_window: 1_000_000 },
    CatalogModel { id: "qwen/qwen3-coder:free", provider: "openrouter", context_window: 262_144 },
    CatalogModel { id: "deepseek/deepseek-r1:free", provider: "openrouter", context_window: 131_072 },
    CatalogModel { id: "nvidia/nemotron-3-ultra-550b-a55b:free", provider: "openrouter", context_window: 131_072 },
    CatalogModel { id: "cohere/north-mini-code:free", provider: "openrouter", context_window: 131_072 },
];

// ───────────────────────── probe micro-tasks ─────────────────────────

/// One micro-task: a tiny prompt plus a quality heuristic over the reply.
/// Heuristics are deliberately lenient (partial credit) because free models
/// are chatty; what matters is whether the model *followed the instruction*.
struct ProbeTask {
    prompt: &'static str,
    check: fn(&str) -> f64,
}

fn check_exact_ok(out: &str) -> f64 {
    let t = out.trim();
    if t == "OK" {
        1.0
    } else if t.contains("OK") {
        0.5
    } else {
        0.15
    }
}

fn check_arithmetic(out: &str) -> f64 {
    let t = out.trim();
    if t == "4" {
        1.0
    } else if t.contains('4') {
        0.5
    } else {
        0.15
    }
}

fn check_code_completion(out: &str) -> f64 {
    if out.contains("a + b") || out.contains("a+b") {
        1.0
    } else if out.contains("return") {
        0.5
    } else {
        0.15
    }
}

/// 3 micro-tasks, each a few dozen tokens at most. Total probe cost per model
/// is well under 1k tokens.
const PROBE_TASKS: &[ProbeTask] = &[
    ProbeTask { prompt: "Reply with exactly: OK", check: check_exact_ok },
    ProbeTask { prompt: "What is 2+2? Reply with just the number.", check: check_arithmetic },
    ProbeTask {
        prompt: "Complete this Rust function, reply with code only:\nfn add(a: i32, b: i32) -> i32 {",
        check: check_code_completion,
    },
];

/// Small `max_tokens` keeps probes cheap; 32 tokens is plenty for "OK".
pub const PROBE_MAX_TOKENS: u32 = 32;
/// Per-request timeout for a probe. Free tiers can be slow; 30 s is generous
/// without letting one dead model stall the whole refresh.
pub const PROBE_TIMEOUT_SECS: u64 = 30;
/// Gap between models so we don't hammer rate-limited free tiers.
pub const PROBE_GAP: Duration = Duration::from_secs(1);

// ───────────────────────── results & cache ─────────────────────────

/// Outcome of probing one model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelProbe {
    pub model: String,
    pub provider: String,
    pub context_window: Option<u32>,
    /// True when the model is known to cost $0 (free tier / `:free` suffix).
    pub free: bool,
    /// Mean latency across the micro-tasks (ms). 0 when probing failed.
    pub latency_ms: u64,
    /// True only when *every* micro-task succeeded.
    pub success: bool,
    /// 0.0–1.0. 0.0 when probing failed.
    pub score: f64,
    pub last_error: Option<String>,
}

/// What `~/.zen/models-cache.json` holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsCache {
    /// RFC 3339 timestamp of the probe run.
    pub verified_at: String,
    /// Base URL the probes ran against (a cache from another provider is useless).
    pub base_url: String,
    pub results: Vec<ModelProbe>,
}

/// A cache older than this is shown with a staleness warning.
pub const CACHE_STALE_SECS: i64 = 24 * 3600;

pub fn cache_path(home: Option<&Path>) -> Option<PathBuf> {
    home.map(|h| h.join(".zen").join("models-cache.json"))
}

pub fn load_cache(home: Option<&Path>) -> Option<ModelsCache> {
    let path = cache_path(home)?;
    let text = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn save_cache(home: Option<&Path>, cache: &ModelsCache) -> Result<()> {
    let path = cache_path(home).context("home directory not available")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Write atomically-ish: temp file + rename, so a crash can't corrupt the cache.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(cache)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Seconds since `verified_at`; `None` when the timestamp doesn't parse.
pub fn cache_age_secs(cache: &ModelsCache) -> Option<i64> {
    let verified = chrono::DateTime::parse_from_rfc3339(&cache.verified_at).ok()?;
    Some(chrono::Utc::now().signed_duration_since(verified).num_seconds())
}

pub fn cache_is_stale(cache: &ModelsCache) -> bool {
    cache_age_secs(cache).map(|a| a > CACHE_STALE_SECS).unwrap_or(true)
}

// ───────────────────────── scoring ─────────────────────────

/// Combine per-task quality into one 0.0–1.0 score, penalising slow models.
/// A model answering in ~0 s keeps its full quality; at 30 s mean latency the
/// score halves. Speed matters for an interactive agent.
pub fn aggregate_score(qualities: &[f64], avg_latency_ms: u64) -> f64 {
    if qualities.is_empty() {
        return 0.0;
    }
    let mean_q = qualities.iter().sum::<f64>() / qualities.len() as f64;
    let secs = avg_latency_ms as f64 / 1000.0;
    (mean_q / (1.0 + secs / 30.0)).clamp(0.0, 1.0)
}

/// The model to recommend: highest score among models that passed every probe.
/// Failed models are never recommended.
pub fn best_model(results: &[ModelProbe]) -> Option<&ModelProbe> {
    results
        .iter()
        .filter(|r| r.success)
        .max_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(Ordering::Equal))
}

// ───────────────────────── probing ─────────────────────────

/// Probe one model with every micro-task, sequentially.
///
/// `provider`, `context_window` and `free` are metadata about the candidate;
/// the probe itself only measures "does it answer, how well, how fast".
/// Any task error (timeout, 429, 5xx, bad response) fails the whole model —
/// strict, because a "verified" badge must mean something.
pub async fn probe_model(
    backend: &dyn LlmBackend,
    model_id: &str,
    provider: &str,
    context_window: Option<u32>,
    free: bool,
) -> ModelProbe {
    let mut qualities = Vec::with_capacity(PROBE_TASKS.len());
    let mut latencies = Vec::with_capacity(PROBE_TASKS.len());

    for task in PROBE_TASKS {
        let req = ChatRequest {
            model: model_id.to_string(),
            messages: vec![ChatMessage::user(task.prompt)],
            tools: vec![],
            max_tokens: Some(PROBE_MAX_TOKENS),
        };
        let start = Instant::now();
        match backend.chat(req).await {
            Ok(resp) => {
                let text = resp.message.content.unwrap_or_default();
                latencies.push(start.elapsed().as_millis() as u64);
                qualities.push((task.check)(&text));
            }
            Err(e) => {
                return ModelProbe {
                    model: model_id.to_string(),
                    provider: provider.to_string(),
                    context_window,
                    free,
                    latency_ms: 0,
                    success: false,
                    score: 0.0,
                    last_error: Some(short_error(&e.to_string())),
                };
            }
        }
    }

    let avg_latency = latencies.iter().sum::<u64>() / latencies.len().max(1) as u64;
    ModelProbe {
        model: model_id.to_string(),
        provider: provider.to_string(),
        context_window,
        free,
        latency_ms: avg_latency,
        success: true,
        score: aggregate_score(&qualities, avg_latency),
        last_error: None,
    }
}

/// Keep error strings short for the table; never leak more than the message.
fn short_error(msg: &str) -> String {
    let first_line = msg.lines().next().unwrap_or(msg);
    let mut s: String = first_line.chars().take(120).collect();
    if first_line.chars().count() > 120 {
        s.push('…');
    }
    s
}

// ───────────────────────── real HTTP backend ─────────────────────────

/// Minimal `LlmBackend` for probing: one POST to `{base_url}/chat/completions`
/// with a short timeout and **no retries**. Retries would turn a dead model
/// into a minutes-long stall; a probe must fail fast.
pub struct ProbeClient {
    base_url: String,
    api_key: Option<String>,
    http: reqwest::Client,
}

impl ProbeClient {
    pub fn new(base_url: &str, api_key: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(PROBE_TIMEOUT_SECS))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { base_url: base_url.trim_end_matches('/').to_string(), api_key, http }
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }
}

#[derive(Deserialize)]
struct ProbeChatResponse {
    #[serde(default)]
    choices: Vec<ProbeChoice>,
    #[serde(default)]
    usage: Option<ProbeUsage>,
}

#[derive(Deserialize)]
struct ProbeChoice {
    message: ProbeMessage,
}

#[derive(Deserialize)]
struct ProbeMessage {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize)]
struct ProbeUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    total_tokens: u64,
}

#[async_trait::async_trait]
impl LlmBackend for ProbeClient {
    async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse> {
        let mut r = self.http.post(self.endpoint()).json(&serde_json::json!({
            "model": req.model,
            "messages": req.messages,
            "max_tokens": req.max_tokens,
        }));
        if let Some(key) = self.api_key.as_deref().filter(|k| !k.is_empty()) {
            r = r.bearer_auth(key);
        }
        let resp = r.send().await?.error_for_status()?;
        let body: ProbeChatResponse = resp.json().await?;
        let choice = body.choices.into_iter().next()
            .ok_or_else(|| anyhow::anyhow!("probe: empty choices in response"))?;
        let usage = body.usage.unwrap_or(ProbeUsage { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 });
        Ok(ChatResponse {
            message: ChatMessage::assistant(choice.message.content, None),
            usage: Usage {
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                total_tokens: usage.total_tokens,
                ..Default::default()
            },
            finish_reason: None,
        })
    }
}

// ───────────────────────── /models discovery ─────────────────────────

/// A model id discovered from the provider's `/models` endpoint.
pub struct DiscoveredModel {
    pub id: String,
    pub free: bool,
}

/// Heuristic: an id ending in `:free` (OpenRouter convention) is free.
fn id_looks_free(id: &str) -> bool {
    id.ends_with(":free")
}

/// GET `{base_url}/models` (OpenAI-compatible). Returns an empty vec on any
/// failure — discovery is best-effort; the static catalog is the fallback.
pub async fn fetch_models_list(base_url: &str, api_key: Option<&str>) -> Vec<DiscoveredModel> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build();
    let http = match http {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let mut req = http.get(&url);
    if let Some(key) = api_key.filter(|k| !k.is_empty()) {
        req = req.bearer_auth(key);
    }
    let body: serde_json::Value = match req.send().await {
        Ok(r) => match r.error_for_status() {
            Ok(ok) => match ok.json().await {
                Ok(v) => v,
                Err(_) => return vec![],
            },
            Err(_) => return vec![],
        },
        Err(_) => return vec![],
    };
    let data = body.get("data").and_then(|d| d.as_array()).cloned().unwrap_or_default();
    data.iter()
        .filter_map(|m| m.get("id")?.as_str().map(|s| s.to_string()))
        .map(|id| {
            // OpenRouter-style pricing: {"pricing": {"prompt": "0", "completion": "0"}}
            let priced_zero = body_pointer_free(&body, &id);
            DiscoveredModel { free: id_looks_free(&id) || priced_zero, id }
        })
        .collect()
}

/// Look for a matching entry's pricing in an OpenRouter-style `/models` body.
fn body_pointer_free(body: &serde_json::Value, id: &str) -> bool {
    let entry = body
        .get("data")
        .and_then(|d| d.as_array())
        .and_then(|arr| arr.iter().find(|m| m.get("id").and_then(|v| v.as_str()) == Some(id)));
    let pricing = match entry.and_then(|e| e.get("pricing")) {
        Some(p) => p,
        None => return false,
    };
    let zero = |v: &serde_json::Value| v.as_str() == Some("0");
    pricing.get("prompt").map(zero).unwrap_or(false)
        && pricing.get("completion").map(zero).unwrap_or(false)
}

/// A probe candidate: model id + metadata.
pub struct Candidate {
    pub id: String,
    pub provider: String,
    pub context_window: Option<u32>,
    pub free: bool,
}

/// Merge the static catalog with `/models` discovery, deduplicated by id.
/// Catalog entries win on metadata (they carry context windows).
pub fn merge_candidates(catalog: &[CatalogModel], discovered: &[DiscoveredModel]) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = catalog
        .iter()
        .map(|c| Candidate {
            id: c.id.to_string(),
            provider: c.provider.to_string(),
            context_window: Some(c.context_window),
            free: true,
        })
        .collect();
    for d in discovered {
        if out.iter().any(|c| c.id == d.id) {
            continue;
        }
        // Only auto-add discovered models that look free; paid models are out
        // of scope for `zen models` (free-model auto-discovery).
        if d.free {
            out.push(Candidate {
                id: d.id.clone(),
                provider: "discovered".to_string(),
                context_window: None,
                free: true,
            });
        }
    }
    out
}

// ───────────────────────── CLI entry point ─────────────────────────

/// `zen models [--refresh] [--best]`
pub async fn models_command(
    cli: &crate::cli::Cli,
    cwd: &Path,
    home: Option<&Path>,
    args: &crate::cli::ModelsArgs,
) -> Result<()> {
    let cfg = crate::settings::resolve_config_with_home(cli, cwd, home)?;

    let cached = load_cache(home);
    let cache_usable = cached.as_ref().is_some_and(|c| c.base_url == cfg.base_url);

    let results: Vec<ModelProbe> = if args.refresh || !cache_usable {
        if !args.best {
            eprintln!("probing free models at {} ...", cfg.base_url);
        }
        let discovered = fetch_models_list(&cfg.base_url, cfg.api_key.as_deref()).await;
        let candidates = merge_candidates(FREE_MODEL_CATALOG, &discovered);
        if candidates.is_empty() {
            anyhow::bail!("no candidate free models found (static catalog is empty and /models discovery failed)");
        }
        let backend = ProbeClient::new(&cfg.base_url, cfg.api_key.clone());
        let mut results = Vec::with_capacity(candidates.len());
        for (i, cand) in candidates.iter().enumerate() {
            if !args.best {
                eprintln!("  [{}/{}] probing {} ...", i + 1, candidates.len(), cand.id);
            }
            let probe = probe_model(&backend, &cand.id, &cand.provider, cand.context_window, cand.free).await;
            if !args.best {
                eprintln!(
                    "           {}",
                    if probe.success {
                        format!("ok  score={:.2} latency={}ms", probe.score, probe.latency_ms)
                    } else {
                        format!("FAIL {}", probe.last_error.as_deref().unwrap_or("unknown error"))
                    }
                );
            }
            results.push(probe);
            if i + 1 < candidates.len() {
                tokio::time::sleep(PROBE_GAP).await;
            }
        }
        let cache = ModelsCache {
            verified_at: chrono::Utc::now().to_rfc3339(),
            base_url: cfg.base_url.clone(),
            results: results.clone(),
        };
        if let Err(e) = save_cache(home, &cache) {
            eprintln!("warning: could not save models cache: {:#}", e);
        }
        results
    } else {
        let cache = cached.unwrap(); // checked by cache_usable
        if cache_is_stale(&cache) {
            let age = cache_age_secs(&cache).unwrap_or(-1);
            eprintln!(
                "warning: models cache is {} old (>24h); run `zen models --refresh` to re-probe",
                human_age(age)
            );
        }
        cache.results
    };

    if args.best {
        match best_model(&results) {
            Some(m) => {
                println!("{}", m.model);
                Ok(())
            }
            None => {
                eprintln!("zen models: no working model found (all {} candidates failed probing)", results.len());
                std::process::exit(1);
            }
        }
    } else {
        print_table(&results, &cfg.base_url);
        Ok(())
    }
}

fn human_age(secs: i64) -> String {
    if secs < 0 {
        return "unknown age".to_string();
    }
    if secs < 90 {
        format!("{}s", secs)
    } else if secs < 5400 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}

fn fmt_ctx(ctx: Option<u32>) -> String {
    match ctx {
        Some(c) if c >= 1_000_000 => format!("{}M", c / 1_000_000),
        Some(c) if c >= 1000 => format!("{}k", c / 1000),
        Some(c) => c.to_string(),
        None => "-".to_string(),
    }
}

fn fmt_latency(ms: u64, success: bool) -> String {
    if !success {
        return "-".to_string();
    }
    if ms >= 1000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}ms", ms)
    }
}

fn print_table(results: &[ModelProbe], base_url: &str) {
    println!("zen models — {} candidate(s) from {}", results.len(), base_url);
    println!("  {:<40} {:<16} {:>6} {:>7} {:>8} {:>6}  STATUS", "MODEL", "PROVIDER", "CTX", "PRICE", "LATENCY", "SCORE");
    let best = best_model(results).map(|m| m.model.clone());
    for r in results {
        let status = if Some(&r.model) == best.as_ref() {
            "✓ recommended"
        } else if r.success {
            "✓"
        } else {
            "✗ failed"
        };
        let detail = if !r.success {
            format!(" ({})", r.last_error.as_deref().unwrap_or("error"))
        } else {
            String::new()
        };
        println!(
            "  {:<40} {:<16} {:>6} {:>7} {:>8} {:>6}  {}{}",
            truncate(&r.model, 40),
            truncate(&r.provider, 16),
            fmt_ctx(r.context_window),
            if r.free { "$0.00" } else { "?" },
            fmt_latency(r.latency_ms, r.success),
            if r.success { format!("{:.2}", r.score) } else { "-".to_string() },
            status,
            detail,
        );
    }
    if best.is_none() {
        println!("  no working model found; run `zen models --refresh` to retry");
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max.saturating_sub(1)])
    }
}

// ───────────────────────── unit tests ─────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// Scripted `LlmBackend`: pops canned text replies (or errors) in order.
    struct MockBackend {
        queue: Mutex<VecDeque<anyhow::Result<String>>>,
    }

    impl MockBackend {
        fn replies(texts: Vec<&str>) -> Self {
            Self {
                queue: Mutex::new(texts.into_iter().map(|t| Ok(t.to_string())).collect()),
            }
        }
        fn errors(errs: Vec<&str>) -> Self {
            Self {
                queue: Mutex::new(errs.into_iter().map(|e| Err(anyhow::anyhow!("{}", e))).collect()),
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmBackend for MockBackend {
        async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse> {
            assert_eq!(req.max_tokens, Some(PROBE_MAX_TOKENS), "probes must stay cheap");
            let text = self
                .queue
                .lock()
                .unwrap()
                .pop_front()
                .expect("mock: more probe calls than scripted replies")?;
            Ok(ChatResponse {
                message: ChatMessage::assistant(Some(text), None),
                usage: Usage { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15, ..Default::default() },
                finish_reason: Some("stop".into()),
            })
        }
    }

    fn probe(id: &str, success: bool, score: f64) -> ModelProbe {
        ModelProbe {
            model: id.to_string(),
            provider: "test".to_string(),
            context_window: None,
            free: true,
            latency_ms: if success { 1200 } else { 0 },
            success,
            score,
            last_error: if success { None } else { Some("boom".to_string()) },
        }
    }

    #[tokio::test]
    async fn probe_all_tasks_pass_scores_high() {
        let b = MockBackend::replies(vec!["OK", "4", "fn add(a: i32, b: i32) -> i32 {\n    a + b\n}"]);
        let r = probe_model(&b, "m", "p", None, true).await;
        assert!(r.success);
        assert!(r.score > 0.9, "perfect replies should score near 1.0, got {}", r.score);
        assert!(r.last_error.is_none());
    }

    #[tokio::test]
    async fn probe_chatty_but_correct_gets_partial_credit() {
        // Contains the right answers but wrapped in chatter.
        let b = MockBackend::replies(vec!["Sure! OK", "The answer is 4.", "return a+b; // done"]);
        let r = probe_model(&b, "m", "p", None, true).await;
        assert!(r.success);
        assert!(r.score > 0.4 && r.score < 0.9, "partial credit expected, got {}", r.score);
    }

    #[tokio::test]
    async fn probe_error_fails_the_model() {
        let b = MockBackend::errors(vec!["timeout after 30s"]);
        let r = probe_model(&b, "m", "p", None, true).await;
        assert!(!r.success);
        assert_eq!(r.score, 0.0);
        assert_eq!(r.latency_ms, 0);
        assert!(r.last_error.unwrap().contains("timeout"));
    }

    #[tokio::test]
    async fn probe_second_task_error_fails_the_model() {
        // First probe ok, second errors → whole model failed (strict).
        let b = MockBackend {
            queue: Mutex::new(VecDeque::from([
                Ok("OK".to_string()),
                Err(anyhow::anyhow!("429 rate limited")),
            ])),
        };
        let r = probe_model(&b, "m", "p", None, true).await;
        assert!(!r.success);
        assert_eq!(r.score, 0.0);
    }

    #[test]
    fn aggregate_score_penalises_latency() {
        let fast = aggregate_score(&[1.0, 1.0, 1.0], 1_000);
        let slow = aggregate_score(&[1.0, 1.0, 1.0], 30_000);
        assert!(fast > slow);
        assert!((slow - 0.5).abs() < 0.01, "30s latency should halve the score, got {}", slow);
    }

    #[test]
    fn aggregate_score_empty_is_zero() {
        assert_eq!(aggregate_score(&[], 100), 0.0);
    }

    #[test]
    fn best_model_picks_highest_score_and_skips_failures() {
        let results = vec![
            probe("low", true, 0.5),
            probe("dead", false, 0.0),
            probe("high", true, 0.9),
        ];
        assert_eq!(best_model(&results).unwrap().model, "high");
    }

    #[test]
    fn best_model_none_when_all_failed() {
        let results = vec![probe("a", false, 0.0), probe("b", false, 0.0)];
        assert!(best_model(&results).is_none());
    }

    #[test]
    fn merge_candidates_dedupes_and_keeps_catalog_metadata() {
        let discovered = vec![
            DiscoveredModel { id: "qwen/qwen3-coder:free".to_string(), free: true }, // dup of catalog
            DiscoveredModel { id: "new/model:free".to_string(), free: true },
            DiscoveredModel { id: "paid/model".to_string(), free: false }, // not free → skipped
        ];
        let merged = merge_candidates(FREE_MODEL_CATALOG, &discovered);
        assert_eq!(merged.len(), FREE_MODEL_CATALOG.len() + 1);
        let qwen = merged.iter().find(|c| c.id == "qwen/qwen3-coder:free").unwrap();
        assert_eq!(qwen.provider, "openrouter"); // catalog metadata wins
        assert_eq!(qwen.context_window, Some(262_144));
        assert!(merged.iter().any(|c| c.id == "new/model:free"));
        assert!(!merged.iter().any(|c| c.id == "paid/model"));
    }

    #[test]
    fn cache_roundtrip_and_staleness() {
        let dir = std::env::temp_dir().join(format!("zen-models-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let fresh = ModelsCache {
            verified_at: chrono::Utc::now().to_rfc3339(),
            base_url: "https://x".to_string(),
            results: vec![probe("m", true, 0.8)],
        };
        save_cache(Some(&dir), &fresh).unwrap();
        let loaded = load_cache(Some(&dir)).unwrap();
        assert_eq!(loaded.results.len(), 1);
        assert_eq!(loaded.results[0].model, "m");
        assert!(!cache_is_stale(&loaded));

        let old = ModelsCache {
            verified_at: "2020-01-01T00:00:00Z".to_string(),
            ..fresh
        };
        assert!(cache_is_stale(&old));
        assert!(cache_age_secs(&old).unwrap() > CACHE_STALE_SECS);

        // Corrupt cache → None, not a crash.
        std::fs::write(dir.join(".zen").join("models-cache.json"), "not json").unwrap();
        assert!(load_cache(Some(&dir)).is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn short_error_truncates_long_messages() {
        let long = "e".repeat(500);
        assert!(short_error(&long).chars().count() <= 121);
        assert_eq!(short_error("one\ntwo"), "one");
    }

    #[test]
    fn id_looks_free_detection() {
        assert!(id_looks_free("foo/bar:free"));
        assert!(!id_looks_free("foo/bar"));
    }
}
