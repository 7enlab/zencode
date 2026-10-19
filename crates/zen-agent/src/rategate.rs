//! RateGate: per-provider daily request quota for `:free` LLM tiers.
//!
//! Free LLM gateways (e.g. OpenRouter `:free` models) cap usage at a small number of
//! requests per day (typically 50). [`RateGate`] is an [`LlmBackend`] wrapper that counts
//! every `chat` call per provider and, once the daily quota is exhausted, fails fast with
//! a clear error **before** hitting the network. Because the agent loop's
//! [`chat_with_fallback`](crate::client::chat_with_fallback) treats any backend error as
//! "try the next model", an exhausted quota automatically moves the run to the next
//! configured fallback model — no loop changes needed.
//!
//! Design notes (see atzenn/zencode#10):
//! - Counts live in `~/.zen/rategate.json` (`{"date":"2026-10-17","counts":{"openrouter.ai":12}}`);
//!   the date rolls over on first use each UTC day. Writes are atomic (temp file + rename).
//! - Every `chat` call counts as one request, **including failures** (a 429 that the inner
//!   client retried internally still counts once — per-attempt counting would burn the quota
//!   on a single flaky call).
//! - Local gateways (`localhost`, `127.0.0.1`, `::1`) are never gated: they have no caps.
//! - The provider key is the base URL's host (`https://openrouter.ai/api/v1` → `openrouter.ai`).
//! - Quota state can be inspected without a gate instance via [`quota_report_for`], which is
//!   what `zen doctor` and the REPL `/quota` command use.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use zen_core::{ChatRequest, ChatResponse, LlmBackend};

/// Default daily request limit per provider (matches the common OpenRouter `:free` cap).
pub const DEFAULT_DAILY_LIMIT: u64 = 50;
/// File name (under `~/.zen/`) holding the quota counts.
pub const STORE_FILE_NAME: &str = "rategate.json";

/// Resolved runtime settings for [`RateGate`].
#[derive(Debug, Clone)]
pub struct RateGateSettings {
    /// Master switch. When false the gate delegates directly (no counting, no limits).
    pub enabled: bool,
    /// Default daily limit applied to every provider without an explicit override.
    pub daily_limit: u64,
    /// Per-provider overrides, keyed by provider host (e.g. `"openrouter.ai"`).
    pub limits: HashMap<String, u64>,
}

impl Default for RateGateSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            daily_limit: DEFAULT_DAILY_LIMIT,
            limits: HashMap::new(),
        }
    }
}

/// One provider's quota line, for status displays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaStatus {
    pub provider: String,
    pub used: u64,
    pub limit: u64,
    pub date: String,
}

impl QuotaStatus {
    pub fn remaining(&self) -> u64 {
        self.limit.saturating_sub(self.used)
    }
    pub fn exhausted(&self) -> bool {
        self.used >= self.limit
    }
}

/// On-disk shape of the quota store.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct GateStoreFile {
    #[serde(default)]
    date: String,
    #[serde(default)]
    counts: HashMap<String, u64>,
}

/// An [`LlmBackend`] that enforces a per-provider daily request quota.
///
/// Construct with [`RateGate::new`] and use anywhere an `Arc<dyn LlmBackend>` is expected;
/// `Arc<RateGate>` coerces directly.
pub struct RateGate {
    inner: Arc<dyn LlmBackend>,
    base_url: String,
    settings: RateGateSettings,
    /// `None` → in-memory only (no persistence; useful for tests and `--bare` runs).
    store_path: Option<PathBuf>,
    /// Serializes our own read-modify-write cycles against the store file.
    io: Mutex<()>,
}

impl RateGate {
    /// Wrap `inner`. `base_url` is the inner backend's base URL (used to derive the
    /// provider key); `store_path` is the `rategate.json` location or `None` for
    /// memory-only counting.
    pub fn new(
        inner: Arc<dyn LlmBackend>,
        base_url: String,
        settings: RateGateSettings,
        store_path: Option<PathBuf>,
    ) -> Self {
        Self {
            inner,
            base_url,
            settings,
            store_path,
            io: Mutex::new(()),
        }
    }

    /// Default store location: `~/.zen/rategate.json` (or `home/.zen/rategate.json`).
    pub fn default_store_path(home: &Path) -> PathBuf {
        home.join(".zen").join(STORE_FILE_NAME)
    }

    /// Provider key for a base URL, or `None` for local gateways (never gated).
    pub fn provider_key(base_url: &str) -> Option<String> {
        let lower = base_url.to_ascii_lowercase();
        if lower.contains("://127.0.0.1")
            || lower.contains("://localhost")
            || lower.contains("://[::1]")
        {
            return None;
        }
        let after_scheme = base_url.split("://").nth(1).unwrap_or(base_url);
        let host_port = after_scheme.split('/').next().unwrap_or(after_scheme);
        let host_port = host_port.split('@').next_back().unwrap_or(host_port); // strip userinfo
        let host = host_port.split(':').next().unwrap_or(host_port); // strip port
        let host = host.trim().trim_start_matches('[').trim_end_matches(']');
        // A host cannot contain whitespace; anything else here isn't a URL we understand.
        if host.is_empty() || host.chars().any(|c| c.is_whitespace()) {
            None
        } else {
            Some(host.to_ascii_lowercase())
        }
    }

    /// The daily limit that applies to `model` on `provider`, or `None` when ungated.
    fn gated_limit(&self, provider: &str, model: &str) -> Option<u64> {
        if !model.ends_with(":free") {
            return None;
        }
        if self.settings.limits.contains_key(provider) || provider == "openrouter.ai" {
            Some(self.limit_for(provider))
        } else {
            None
        }
    }

    fn limit_for(&self, provider: &str) -> u64 {
        self.settings
            .limits
            .get(provider)
            .copied()
            .unwrap_or(self.settings.daily_limit)
    }

    fn read_store(&self) -> GateStoreFile {
        let Some(path) = self.store_path.as_deref() else {
            return GateStoreFile::default();
        };
        let Ok(bytes) = std::fs::read(path) else {
            return GateStoreFile::default();
        };
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    fn write_store(&self, state: &GateStoreFile) {
        let Some(path) = self.store_path.as_deref() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Atomic write: temp file + rename, so a crash never leaves a half-written file.
        let tmp = path.with_extension("json.tmp");
        let Ok(bytes) = serde_json::to_vec_pretty(state) else {
            return;
        };
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// Load the store, rolling the date over to today when stale.
    fn fresh_state(&self) -> GateStoreFile {
        let mut state = self.read_store();
        let today = today_utc();
        if state.date != today {
            state.date = today;
            state.counts.clear();
        }
        state
    }

    fn used_today(&self, provider: &str) -> u64 {
        let _guard = self.io.lock().unwrap_or_else(|e| e.into_inner());
        self.fresh_state().counts.get(provider).copied().unwrap_or(0)
    }

    fn record(&self, provider: &str) {
        let _guard = self.io.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = self.fresh_state();
        *state.counts.entry(provider.to_string()).or_insert(0) += 1;
        self.write_store(&state);
    }

    /// Current quota lines for every provider seen today (plus the current provider when
    /// it has no usage yet, so `zen doctor` shows the active gate).
    pub fn quota_status(&self) -> Vec<QuotaStatus> {
        let _guard = self.io.lock().unwrap_or_else(|e| e.into_inner());
        let state = self.fresh_state();
        let mut out: Vec<QuotaStatus> = state
            .counts
            .iter()
            .map(|(provider, &used)| {
                let limit = self.limit_for(provider);
                QuotaStatus {
                    provider: provider.clone(),
                    used,
                    limit,
                    date: state.date.clone(),
                }
            })
            .collect();
        if let Some(provider) = Self::provider_key(&self.base_url) {
            if !out.iter().any(|q| q.provider == provider) {
                out.push(QuotaStatus {
                    used: 0,
                    limit: self.limit_for(&provider),
                    provider,
                    date: state.date.clone(),
                });
            }
        }
        out.sort_by(|a, b| a.provider.cmp(&b.provider));
        out
    }

    /// Human-readable quota report for this gate's store.
    pub fn quota_report(&self) -> String {
        format_quota_report(&self.quota_status(), self.settings.enabled)
    }
}

/// Human-readable quota report for an arbitrary store path (no gate instance needed).
/// Used by `zen doctor` and the REPL `/quota` command.
pub fn quota_report_for(store_path: Option<&Path>) -> String {
    let Some(path) = store_path else {
        return "RateGate: no home directory; quota tracking is memory-only for this run.".into();
    };
    let state: GateStoreFile = std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let today = today_utc();
    let status: Vec<QuotaStatus> = if state.date == today {
        let mut v: Vec<QuotaStatus> = state
            .counts
            .iter()
            .map(|(provider, &used)| QuotaStatus {
                provider: provider.clone(),
                used,
                // Without a live gate we don't know per-provider overrides; show the default.
                limit: DEFAULT_DAILY_LIMIT,
                date: state.date.clone(),
            })
            .collect();
        v.sort_by(|a, b| a.provider.cmp(&b.provider));
        v
    } else {
        Vec::new()
    };
    if status.is_empty() {
        format!("RateGate: no usage recorded today ({today} UTC).")
    } else {
        format_quota_report(&status, true)
    }
}

fn format_quota_report(status: &[QuotaStatus], enabled: bool) -> String {
    let mut lines = Vec::new();
    let date = status.first().map(|s| s.date.as_str()).unwrap_or("");
    lines.push(format!(
        "RateGate quota ({} UTC){}:",
        date,
        if enabled { "" } else { " [disabled]" }
    ));
    for q in status {
        lines.push(format!(
            "  {}: {}/{} used ({} remaining){}",
            q.provider,
            q.used,
            q.limit,
            q.remaining(),
            if q.exhausted() { " — EXHAUSTED" } else { "" }
        ));
    }
    lines.join("\n")
}

// ─── Live OpenRouter quota (roadmap P0-4) ───────────────────────────────────
//
// OpenRouter reports the real free-model counter on `GET /api/v1/key`
// (`data.free_model_daily_requests {used, limit, remaining}`, UTC day), see
// https://openrouter.ai/docs/api-reference/limits. It is fetched only when the user asks
// (`/quota`); no run ever calls it in the background.

/// The key-info endpoint for an OpenRouter base URL (`https://openrouter.ai/api/v1` →
/// `https://openrouter.ai/api/v1/key`), or `None` for any other provider.
pub fn openrouter_key_url(base_url: &str) -> Option<String> {
    let lower = base_url.trim().to_ascii_lowercase();
    lower
        .contains("openrouter.ai")
        .then(|| format!("{}/key", base_url.trim().trim_end_matches('/')))
}

/// The parts of OpenRouter's `GET /api/v1/key` that `/quota` shows.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyQuota {
    /// Free-model requests used / allowed / left in the current UTC day.
    pub free_used: Option<u64>,
    pub free_limit: Option<u64>,
    pub free_remaining: Option<u64>,
    /// The key's credit limit and what is left of it (`None` = unlimited).
    pub credit_limit: Option<f64>,
    pub credit_remaining: Option<f64>,
    /// Credits used today (UTC).
    pub usage_daily: Option<f64>,
    pub is_free_tier: Option<bool>,
}

impl KeyQuota {
    /// Parse a `GET /api/v1/key` body.
    pub fn from_body(body: &str) -> anyhow::Result<Self> {
        let v: serde_json::Value = serde_json::from_str(body)
            .map_err(|e| anyhow::anyhow!("invalid /key response: {e}"))?;
        let d = v
            .get("data")
            .filter(|d| d.is_object())
            .ok_or_else(|| anyhow::anyhow!("invalid /key response: no `data` object"))?;
        let free = d.get("free_model_daily_requests");
        let fu = |k: &str| free.and_then(|f| f.get(k)).and_then(serde_json::Value::as_u64);
        let ff = |k: &str| d.get(k).and_then(serde_json::Value::as_f64);
        Ok(Self {
            free_used: fu("used"),
            free_limit: fu("limit"),
            free_remaining: fu("remaining"),
            credit_limit: ff("limit"),
            credit_remaining: ff("limit_remaining"),
            usage_daily: ff("usage_daily"),
            is_free_tier: d.get("is_free_tier").and_then(serde_json::Value::as_bool),
        })
    }

    /// Lines for `/quota`. Never prints the key or its label.
    pub fn report(&self) -> String {
        let mut lines = vec![format!("OpenRouter quota ({} UTC, live from /api/v1/key):", today_utc())];
        match (self.free_used, self.free_limit, self.free_remaining) {
            (Some(u), Some(l), r) => {
                let r = r.unwrap_or_else(|| l.saturating_sub(u));
                lines.push(format!(
                    "  free-model requests: used {u} / limit {l} (remaining {r}){}",
                    if r == 0 { " — EXHAUSTED" } else { "" }
                ));
            }
            _ => lines.push("  free-model requests: not reported for this key".into()),
        }
        match (self.credit_limit, self.credit_remaining) {
            (Some(l), Some(r)) => lines.push(format!("  key credit limit: {l:.2} (remaining {r:.2})")),
            _ => lines.push("  key credit limit: none".into()),
        }
        if let Some(u) = self.usage_daily {
            lines.push(format!("  credits used today: {u:.4}"));
        }
        lines.join("\n")
    }
}

/// `GET {key_url}` with the API key. One request, no retries, 15 s timeout; errors name the
/// HTTP status (a 401 means the key is wrong) and never include the key.
pub async fn fetch_key_quota(key_url: &str, api_key: Option<&str>) -> anyhow::Result<KeyQuota> {
    let timeouts = crate::client::HttpTimeouts::from_secs(key_url, Some(15), None);
    let http = crate::client::build_http(key_url, &timeouts, true);
    let mut rb = http.get(key_url);
    if let Some(k) = api_key.filter(|k| !k.is_empty()) {
        rb = rb.header(reqwest::header::AUTHORIZATION, format!("Bearer {k}"));
    }
    let resp = rb.send().await.map_err(|e| anyhow::anyhow!("quota lookup failed: {e}"))?;
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        let msg = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.pointer("/error/message").and_then(|m| m.as_str()).map(str::to_string))
            .unwrap_or_else(|| body.chars().take(200).collect());
        return Err(anyhow::anyhow!("quota lookup failed: HTTP {status}: {msg}"));
    }
    KeyQuota::from_body(&body)
}

/// The `/quota` text: the live OpenRouter numbers when `key_url` is set (see
/// [`openrouter_key_url`]), then the local RateGate counts.
pub async fn quota_command_report(key_url: Option<&str>, api_key: Option<&str>, store_path: Option<&Path>) -> String {
    let local = quota_report_for(store_path);
    match key_url {
        None => local,
        Some(url) => match fetch_key_quota(url, api_key).await {
            Ok(q) => format!("{}\n{}", q.report(), local),
            Err(e) => format!("{e}\n{local}"),
        },
    }
}

/// Current UTC date as `YYYY-MM-DD` (no chrono dependency; Howard Hinnant's algorithm).
fn today_utc() -> String {
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0) as i64;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[async_trait]
impl LlmBackend for RateGate {
    async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse> {
        // Local gateways and a disabled gate pass straight through.
        let Some(provider) = Self::provider_key(&self.base_url) else {
            return self.inner.chat(req).await;
        };
        if !self.settings.enabled {
            return self.inner.chat(req).await;
        }
        // Only `:free` model tiers are capped, and only on hosts with a known daily cap
        // (OpenRouter) or an explicit `rateGate.limits` entry. Paid models and other
        // gateways (incl. the default one) are never throttled by a guessed quota.
        let Some(limit) = self.gated_limit(&provider, &req.model) else {
            return self.inner.chat(req).await;
        };
        let used = self.used_today(&provider);
        if used >= limit {
            // Fail fast: the agent loop's fallback logic tries the next model on any error.
            return Err(anyhow::anyhow!(
                "rategate: daily quota exhausted for provider '{provider}' ({used}/{limit} requests used, UTC day); \
                 switch to a fallback model or wait until tomorrow"
            ));
        }
        // Count every attempt once, including failures (a 429 retried internally by the
        // inner client still consumes quota — per-attempt counting would burn the daily
        // budget on a single flaky call).
        let res = self.inner.chat(req).await;
        self.record(&provider);
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zen_core::ChatMessage;

    /// Scripted backend: counts calls, optionally fails.
    struct ScriptBackend {
        calls: AtomicUsize,
        fail: bool,
    }

    impl ScriptBackend {
        fn ok() -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                fail: false,
            })
        }
        fn failing() -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                fail: true,
            })
        }
    }

    #[async_trait]
    impl LlmBackend for ScriptBackend {
        async fn chat(&self, _req: ChatRequest) -> anyhow::Result<ChatResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err(anyhow::anyhow!("boom"));
            }
            Ok(ChatResponse {
                message: ChatMessage::assistant(Some("ok".into()), None),
                usage: Default::default(),
                finish_reason: None,
            })
        }
    }

    fn tmp_store(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "zen-rategate-test-{}-{}.json",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn req() -> ChatRequest {
        req_for("vendor/m:free")
    }

    fn req_for(model: &str) -> ChatRequest {
        ChatRequest {
            model: model.into(),
            messages: vec![],
            tools: vec![],
            max_tokens: None,
        }
    }

    fn gate(inner: Arc<dyn LlmBackend>, url: &str, limit: u64, store: PathBuf) -> RateGate {
        RateGate::new(
            inner,
            url.into(),
            RateGateSettings {
                enabled: true,
                daily_limit: limit,
                limits: HashMap::new(),
            },
            Some(store),
        )
    }

    #[tokio::test]
    async fn paid_models_and_unknown_hosts_are_never_gated() {
        let inner = ScriptBackend::ok();
        let g = gate(inner.clone(), "https://openrouter.ai/api/v1", 1, tmp_store("paid"));
        for _ in 0..3 {
            assert!(g.chat(req_for("openai/gpt-5")).await.is_ok(), "paid model must not be gated");
        }
        let g2 = gate(inner.clone(), "https://gateway.example/v1", 1, tmp_store("gw"));
        for _ in 0..3 {
            assert!(g2.chat(req()).await.is_ok(), "unknown host without a limits entry must not be gated");
        }
    }

    #[test]
    fn provider_key_extracts_host() {
        assert_eq!(
            RateGate::provider_key("https://openrouter.ai/api/v1"),
            Some("openrouter.ai".into())
        );
        assert_eq!(
            RateGate::provider_key("https://api.experientiallabs.ai/api/v1/"),
            Some("api.experientiallabs.ai".into())
        );
        assert_eq!(
            RateGate::provider_key("http://example.com:8080/x"),
            Some("example.com".into())
        );
        // Local gateways are never gated.
        assert_eq!(RateGate::provider_key("http://localhost:1234/v1"), None);
        assert_eq!(RateGate::provider_key("http://127.0.0.1:8080/"), None);
        assert_eq!(RateGate::provider_key("http://[::1]/v1"), None);
        assert_eq!(RateGate::provider_key("not a url at all"), None);
    }

    #[test]
    fn civil_from_days_known_dates() {
        // 2026-10-17 UTC == 20743 days after the epoch.
        assert_eq!(civil_from_days(20743), (2026, 10, 17));
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(18628), (2021, 1, 1));
    }

    #[tokio::test]
    async fn counts_requests_and_blocks_at_limit() {
        let store = tmp_store("block");
        let inner = ScriptBackend::ok();
        let g = gate(inner.clone(), "https://openrouter.ai/api/v1", 2, store);
        assert!(g.chat(req()).await.is_ok());
        assert!(g.chat(req()).await.is_ok());
        assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
        // Third call fails fast without touching the inner backend.
        let err = g.chat(req()).await.unwrap_err().to_string();
        assert!(err.contains("daily quota exhausted"), "err was: {err}");
        assert!(err.contains("openrouter.ai"));
        assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn failures_count_against_quota() {
        let store = tmp_store("fail");
        let inner = ScriptBackend::failing();
        let g = gate(inner, "https://openrouter.ai/api/v1", 1, store);
        assert!(g.chat(req()).await.is_err());
        // Quota now exhausted even though the call failed.
        let err = g.chat(req()).await.unwrap_err().to_string();
        assert!(err.contains("daily quota exhausted"), "err was: {err}");
    }

    #[tokio::test]
    async fn local_gateways_are_not_gated() {
        let store = tmp_store("local");
        let inner = ScriptBackend::ok();
        let g = gate(inner.clone(), "http://localhost:1234/v1", 1, store);
        for _ in 0..5 {
            assert!(g.chat(req()).await.is_ok());
        }
        assert_eq!(inner.calls.load(Ordering::SeqCst), 5);
        // Nothing persisted for local URLs.
        assert!(g.quota_status().is_empty());
    }

    #[tokio::test]
    async fn disabled_gate_passes_through() {
        let store = tmp_store("disabled");
        let inner = ScriptBackend::ok();
        let g = RateGate::new(
            inner.clone(),
            "https://openrouter.ai/api/v1".into(),
            RateGateSettings {
                enabled: false,
                ..Default::default()
            },
            Some(store),
        );
        for _ in 0..5 {
            assert!(g.chat(req()).await.is_ok());
        }
        assert_eq!(inner.calls.load(Ordering::SeqCst), 5);
    }

    #[tokio::test]
    async fn per_provider_limits_override_default() {
        let store = tmp_store("perprovider");
        let inner = ScriptBackend::ok();
        let g = RateGate::new(
            inner,
            "https://openrouter.ai/api/v1".into(),
            RateGateSettings {
                enabled: true,
                daily_limit: 100,
                limits: [("openrouter.ai".to_string(), 1)].into_iter().collect(),
            },
            Some(store),
        );
        assert!(g.chat(req()).await.is_ok());
        let err = g.chat(req()).await.unwrap_err().to_string();
        assert!(err.contains("1/1"), "err was: {err}");
    }

    #[tokio::test]
    async fn stale_date_rolls_over() {
        let store = tmp_store("rollover");
        // Seed yesterday's counts directly.
        std::fs::write(
            &store,
            r#"{"date":"2000-01-01","counts":{"openrouter.ai":49}}"#,
        )
        .unwrap();
        let inner = ScriptBackend::ok();
        let g = gate(inner, "https://openrouter.ai/api/v1", 50, store);
        // Rollover cleared the stale counts, so this succeeds.
        assert!(g.chat(req()).await.is_ok());
        let status = g.quota_status();
        assert_eq!(status.len(), 1);
        assert_eq!(status[0].used, 1);
        assert_eq!(status[0].date, today_utc());
    }

    #[tokio::test]
    async fn quota_status_reports_usage() {
        let store = tmp_store("status");
        let inner = ScriptBackend::ok();
        let g = gate(inner, "https://openrouter.ai/api/v1", 50, store);
        // Before any use the provider still shows up (0/50) so `zen doctor` is informative.
        let s = g.quota_status();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].provider, "openrouter.ai");
        assert_eq!(s[0].used, 0);
        assert_eq!(s[0].remaining(), 50);
        assert!(!s[0].exhausted());
        g.chat(req()).await.unwrap();
        let s = g.quota_status();
        assert_eq!(s[0].used, 1);
        assert!(g.quota_report().contains("openrouter.ai: 1/50"));
    }

    #[test]
    fn quota_report_for_empty_store() {
        let p = tmp_store("empty-report");
        let r = quota_report_for(Some(&p));
        assert!(r.contains("no usage recorded"), "was: {r}");
    }

    #[test]
    fn quota_report_for_none_path() {
        assert!(quota_report_for(None).contains("memory-only"));
    }
}
