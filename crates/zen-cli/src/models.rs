
















use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use zen_core::{ChatMessage, ChatRequest, ChatResponse, LlmBackend, Usage};






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



const PROBE_TASKS: &[ProbeTask] = &[
    ProbeTask { prompt: "Reply with exactly: OK", check: check_exact_ok },
    ProbeTask { prompt: "What is 2+2? Reply with just the number.", check: check_arithmetic },
    ProbeTask {
        prompt: "Complete this Rust function, reply with code only:\nfn add(a: i32, b: i32) -> i32 {",
        check: check_code_completion,
    },
];


pub const PROBE_MAX_TOKENS: u32 = 32;


pub const PROBE_TIMEOUT_SECS: u64 = 30;

pub const PROBE_GAP: Duration = Duration::from_secs(1);




#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelProbe {
    pub model: String,
    pub provider: String,
    pub context_window: Option<u32>,

    pub free: bool,

    pub latency_ms: u64,

    pub success: bool,

    pub score: f64,
    pub last_error: Option<String>,
}


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsCache {

    pub verified_at: String,

    pub base_url: String,
    pub results: Vec<ModelProbe>,
}


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

    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(cache)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}


pub fn cache_age_secs(cache: &ModelsCache) -> Option<i64> {
    let verified = chrono::DateTime::parse_from_rfc3339(&cache.verified_at).ok()?;
    Some(chrono::Utc::now().signed_duration_since(verified).num_seconds())
}

pub fn cache_is_stale(cache: &ModelsCache) -> bool {
    cache_age_secs(cache).map(|a| a > CACHE_STALE_SECS).unwrap_or(true)
}






pub fn aggregate_score(qualities: &[f64], avg_latency_ms: u64) -> f64 {
    if qualities.is_empty() {
        return 0.0;
    }
    let mean_q = qualities.iter().sum::<f64>() / qualities.len() as f64;
    let secs = avg_latency_ms as f64 / 1000.0;
    (mean_q / (1.0 + secs / 30.0)).clamp(0.0, 1.0)
}



pub fn best_model(results: &[ModelProbe]) -> Option<&ModelProbe> {
    results
        .iter()
        .filter(|r| r.success)
        .max_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(Ordering::Equal))
}









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


fn short_error(msg: &str) -> String {
    let first_line = msg.lines().next().unwrap_or(msg);
    let mut s: String = first_line.chars().take(120).collect();
    if first_line.chars().count() > 120 {
        s.push('…');
    }
    s
}






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




pub struct DiscoveredModel {
    pub id: String,
    pub free: bool,
}


fn id_looks_free(id: &str) -> bool {
    id.ends_with(":free")
}



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

            let priced_zero = body_pointer_free(&body, &id);
            DiscoveredModel { free: id_looks_free(&id) || priced_zero, id }
        })
        .collect()
}


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


pub struct Candidate {
    pub id: String,
    pub provider: String,
    pub context_window: Option<u32>,
    pub free: bool,
}



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
        let cache = cached.unwrap();
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
    println!("zen models - {} candidate(s) from {}", results.len(), base_url);
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



#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;


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
            DiscoveredModel { id: "qwen/qwen3-coder:free".to_string(), free: true },
            DiscoveredModel { id: "new/model:free".to_string(), free: true },
            DiscoveredModel { id: "paid/model".to_string(), free: false },
        ];
        let merged = merge_candidates(FREE_MODEL_CATALOG, &discovered);
        assert_eq!(merged.len(), FREE_MODEL_CATALOG.len() + 1);
        let qwen = merged.iter().find(|c| c.id == "qwen/qwen3-coder:free").unwrap();
        assert_eq!(qwen.provider, "openrouter");
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
