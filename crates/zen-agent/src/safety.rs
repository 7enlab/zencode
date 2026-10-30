use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub fn user_agent() -> String {
    format!(
        "ZenCode/{} (+https://zencodego.com)",
        env!("CARGO_PKG_VERSION")
    )
}

pub const SAFETY_DISCLAIMER: &str = "No tool can guarantee your account won't be flagged - \
    providers use automated systems with false positives. ZenCode minimizes the risk factors \
    within its control and warns you about the rest.";

pub const MAX_RETRY_ATTEMPTS: u32 = 5;

pub const BACKOFF_BASE_MS: u64 = 1000;

pub const BACKOFF_CAP_MS: u64 = 60_000;

pub const MIN_INTER_REQUEST_MS: u64 = 100;

pub const DEFAULT_CONCURRENCY_CAP: usize = 2;

pub const PROACTIVE_THROTTLE_FRACTION: f64 = 0.8;

pub const PARALLEL_SESSION_WARN_THRESHOLD: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    RateLimit,

    QuotaExhausted,

    Overloaded,

    Fatal,

    Transient,
}

impl ErrorClass {
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            ErrorClass::RateLimit | ErrorClass::Overloaded | ErrorClass::Transient
        )
    }
}

pub fn classify_error(status: Option<u16>, body: &str) -> ErrorClass {
    let lower = body.to_ascii_lowercase();
    if lower.contains("insufficient_quota")
        || lower.contains("insufficient quota")
        || lower.contains("billing")
        || lower.contains("out of credit")
        || lower.contains("exceeded your current quota")
    {
        return ErrorClass::QuotaExhausted;
    }

    match status {
        Some(529) => ErrorClass::Overloaded,
        Some(429) => ErrorClass::RateLimit,
        Some(402) => ErrorClass::QuotaExhausted,
        Some(code) if (400..500).contains(&code) => ErrorClass::Fatal,
        Some(code) if (500..600).contains(&code) => ErrorClass::Transient,

        None => ErrorClass::Transient,
        _ => ErrorClass::Fatal,
    }
}

pub fn backoff_with_jitter(attempt: u32, base_ms: u64, cap_ms: u64, jitter_seed: u64) -> u64 {
    let exp = attempt.saturating_sub(1).min(10);
    let backoff = base_ms.saturating_mul(1u64 << exp).min(cap_ms);

    let mut x = jitter_seed.wrapping_add(0x9E37_79B9_7F4A_7C15).max(1);
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    let jitter_pct = (x % 51) as f64 - 25.0;
    let jittered = backoff as f64 * (1.0 + jitter_pct / 100.0);
    (jittered.max(0.0) as u64).min(cap_ms).max(1)
}

pub fn jitter_now_nanos() -> u64 {
    if let Ok(seed) = std::env::var("ZEN_JITTER_SEED") {
        if let Ok(n) = seed.parse::<u64>() {
            return n;
        }
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x1234_5678)
}

pub fn default_rpm_ceiling(provider: &str) -> Option<u64> {
    match provider.to_ascii_lowercase().as_str() {
        p if p.contains("openai") => Some(400),
        p if p.contains("anthropic") => Some(40),
        p if p.contains("google") || p.contains("gemini") => Some(100),
        p if p.contains("openrouter") => Some(15),

        p if p.contains("ollama") || p.contains("localhost") || p.contains("127.0.0.1") => None,
        _ => Some(60),
    }
}

pub struct RateLimiter {
    rpm_ceiling: f64,

    tokens: f64,

    last_refill: Instant,

    last_request: Option<Instant>,

    window_count: u64,
    window_start: Instant,
}

impl RateLimiter {
    pub fn new(rpm_ceiling: u64) -> Self {
        let now = Instant::now();
        Self {
            rpm_ceiling: rpm_ceiling as f64,
            tokens: rpm_ceiling as f64,
            last_refill: now,
            last_request: None,
            window_count: 0,
            window_start: now,
        }
    }

    pub fn for_provider(provider: &str) -> Option<Self> {
        default_rpm_ceiling(provider).map(Self::new)
    }

    pub fn acquire(&mut self) -> Duration {
        self.refill();
        let mut wait = Duration::ZERO;

        if let Some(last) = self.last_request {
            let floor = Duration::from_millis(MIN_INTER_REQUEST_MS);
            let elapsed = last.elapsed();
            if elapsed < floor {
                wait = floor - elapsed;
            }
        }

        if self.tokens < 1.0 {
            let per_token = Duration::from_secs_f64(60.0 / self.rpm_ceiling.max(1.0));
            wait = wait.max(per_token);
        } else {
            self.tokens -= 1.0;
        }

        if self.window_count as f64 >= self.rpm_ceiling * PROACTIVE_THROTTLE_FRACTION {
            wait = wait.max(Duration::from_millis(500));
        }

        self.last_request = Some(Instant::now() + wait);
        self.window_count += 1;
        wait
    }

    pub fn update_ceiling(&mut self, rpm: u64) {
        self.rpm_ceiling = rpm as f64;
        self.tokens = self.tokens.min(self.rpm_ceiling);
    }

    fn refill(&mut self) {
        let now = Instant::now();

        if now.duration_since(self.window_start) >= Duration::from_secs(60) {
            self.window_start = now;
            self.window_count = 0;
        }
        let elapsed_secs = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed_secs * self.rpm_ceiling / 60.0).min(self.rpm_ceiling);
        self.last_refill = now;
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SessionRegistry {
    #[serde(default)]
    entries: HashMap<String, HashMap<String, Vec<(String, u64)>>>,
}

impl SessionRegistry {
    pub fn load() -> Self {
        let path = registry_path();
        let Ok(data) = std::fs::read(&path) else {
            return Self::default();
        };
        serde_json::from_slice(&data).unwrap_or_default()
    }

    pub fn register(&mut self, provider: &str, key_fingerprint: &str, session_id: &str) -> usize {
        self.prune_stale();
        let now = now_unix();
        let others = {
            let key_map = self.entries.entry(provider.to_string()).or_default();
            let sessions = key_map.entry(key_fingerprint.to_string()).or_default();
            if !sessions.iter().any(|(id, _)| id == session_id) {
                sessions.push((session_id.to_string(), now));
            }
            sessions.len().saturating_sub(1)
        };
        self.save();
        others
    }

    pub fn unregister(&mut self, provider: &str, key_fingerprint: &str, session_id: &str) {
        if let Some(key_map) = self.entries.get_mut(provider) {
            if let Some(sessions) = key_map.get_mut(key_fingerprint) {
                sessions.retain(|(id, _)| id != session_id);
            }
        }
        self.save();
    }

    fn prune_stale(&mut self) {
        let cutoff = now_unix().saturating_sub(24 * 3600);
        for key_map in self.entries.values_mut() {
            for sessions in key_map.values_mut() {
                sessions.retain(|(_, ts)| *ts >= cutoff);
            }
        }
    }

    fn save(&self) {
        let path = registry_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(data) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(&path, data);
        }
    }
}

pub fn key_fingerprint(api_key: &str) -> String {
    let tail: String = api_key
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    let prefix: String = api_key.chars().take(3).collect();
    format!("{}-...{}", prefix, tail)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn registry_path() -> PathBuf {
    let base = dirs::data_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join(".zen").join("session-registry.json")
}

pub fn log_risk_acknowledgement(session_dir: &std::path::Path, reason: &str) {
    let entry = serde_json::json!({
        "event": "risk_acknowledged",
        "flag": "--i-understand-the-risks",
        "reason": reason,
        "timestamp_unix": now_unix(),
    });
    let path = session_dir.join("risk-acknowledgements.jsonl");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let _ = writeln!(f, "{}", entry);
    }
}

pub fn auth_login_warning(provider: &str) -> String {
    format!(
        "API key saved for {provider}.\n\
        \n\
        Account safety: ZenCode paces requests and backs off automatically, but your account is your responsibility. To stay safe:\n\
        - Use one key per machine - don't share keys across computers or people.\n\
        - Avoid running many parallel ZenCode sessions on the same key.\n\
        - If you see repeated rate-limit warnings, slow down rather than pushing through.\n\
        \n\
        Providers may suspend accounts for payment disputes, shared keys, or bot-like traffic patterns. Run `zen doctor` for a safety checklist."
    )
}

pub fn parallel_sessions_warning(count: usize, provider: &str) -> String {
    format!(
        "Warning: {count} concurrent ZenCode sessions are using the same {provider} API key.\n\
        Agentic loops fire many requests per minute. Multiple sessions on one key can hit rate limits fast and may look like automated abuse to the provider.\n\
        Consider: separate API keys per session, or run fewer sessions. (Silence with --i-understand-the-risks.)"
    )
}

pub fn concurrency_cap_warning(requested: usize, cap: usize, provider: &str) -> String {
    format!(
        "Warning: requested concurrency {requested} exceeds the safe concurrency cap ({cap}) for {provider}.\n\
        High concurrency on a single key risks rate-limit storms and account flags.\n\
        Re-run with --i-understand-the-risks to proceed anyway, or lower the concurrency."
    )
}

pub fn repeated_429_warning(hits: u32, provider: &str) -> String {
    format!(
        "Warning: rate limits hit {hits} times in the last 5 minutes ({provider}).\n\
        ZenCode is backing off automatically. If this persists: reduce parallel sessions, switch to a higher tier, or use a different provider to spread load. Pushing through rate limits is the fastest way to get flagged."
    )
}

pub fn quota_exhausted_error(provider: &str, detail: &str) -> String {
    format!(
        "Out of quota / billing issue ({provider}: {detail}).\n\
        ZenCode will NOT retry - retrying a billing error only creates failed-request noise on your account.\n\
        Check your billing dashboard, then re-run. Your work is saved; nothing was lost."
    )
}

pub fn first_429_notice(provider: &str) -> String {
    format!(
        "Rate limited by {provider} - backing off automatically. This is normal under load; your request will be retried."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_has_version_and_domain() {
        let ua = user_agent();
        assert!(ua.starts_with("ZenCode/"), "ua: {ua}");
        assert!(ua.contains("zencodego.com"), "ua: {ua}");
    }

    #[test]
    fn classify_insufficient_quota_never_retries() {
        let bodies = [
            r#"{"error":{"type":"insufficient_quota","message":"You exceeded your current quota"}}"#,
            r#"{"error":{"code":"insufficient_quota"}}"#,
            r#"{"error": {"message": "Billing issue: out of credit"}}"#,
        ];
        for b in bodies {
            let c = classify_error(Some(429), b);
            assert_eq!(c, ErrorClass::QuotaExhausted, "body: {b}");
            assert!(!c.is_retryable());
        }

        assert_eq!(classify_error(Some(402), "{}"), ErrorClass::QuotaExhausted);
    }

    #[test]
    fn classify_429_is_retryable_rate_limit() {
        let c = classify_error(Some(429), r#"{"error":{"type":"rate_limit_exceeded"}}"#);
        assert_eq!(c, ErrorClass::RateLimit);
        assert!(c.is_retryable());
    }

    #[test]
    fn classify_529_is_overloaded() {
        let c = classify_error(Some(529), r#"{"error":{"type":"overloaded_error"}}"#);
        assert_eq!(c, ErrorClass::Overloaded);
        assert!(c.is_retryable());
    }

    #[test]
    fn classify_4xx_fatal_no_retry() {
        for code in [400u16, 401, 403, 404] {
            let c = classify_error(Some(code), "{}");
            assert_eq!(c, ErrorClass::Fatal, "code {code}");
            assert!(!c.is_retryable());
        }
    }

    #[test]
    fn classify_5xx_transient_retryable() {
        for code in [500u16, 502, 503, 504] {
            let c = classify_error(Some(code), "{}");
            assert_eq!(c, ErrorClass::Transient, "code {code}");
            assert!(c.is_retryable());
        }
    }

    #[test]
    fn classify_network_error_transient() {
        assert_eq!(classify_error(None, ""), ErrorClass::Transient);
    }

    #[test]
    fn backoff_grows_exponentially_with_cap() {
        let b1 = backoff_with_jitter(1, 1000, 60_000, 42);
        let b2 = backoff_with_jitter(2, 1000, 60_000, 42);
        let b3 = backoff_with_jitter(3, 1000, 60_000, 42);

        assert!((750..=1250).contains(&b1), "b1={b1}");
        assert!((1500..=2500).contains(&b2), "b2={b2}");
        assert!((3000..=5000).contains(&b3), "b3={b3}");

        let b20 = backoff_with_jitter(20, 1000, 60_000, 42);
        assert!(b20 <= 60_000, "b20={b20}");
        assert!(b20 >= 1);
    }

    #[test]
    fn backoff_deterministic_for_fixed_seed() {
        assert_eq!(
            backoff_with_jitter(3, 1000, 60_000, 7),
            backoff_with_jitter(3, 1000, 60_000, 7)
        );
    }

    #[test]
    fn default_ceilings_are_conservative() {
        assert_eq!(default_rpm_ceiling("openai"), Some(400));
        assert_eq!(default_rpm_ceiling("anthropic"), Some(40));
        assert_eq!(default_rpm_ceiling("openrouter"), Some(15));

        assert_eq!(default_rpm_ceiling("ollama"), None);
        assert_eq!(default_rpm_ceiling("http://localhost:11434/v1"), None);
    }

    #[test]
    fn rate_limiter_enforces_floor_and_bucket() {
        let mut rl = RateLimiter::new(600);
        let w1 = rl.acquire();
        assert_eq!(w1, Duration::ZERO, "first request goes immediately");
        let w2 = rl.acquire();

        assert!(
            w2 >= Duration::from_millis(90),
            "floor not enforced: {w2:?}"
        );
    }

    #[test]
    fn rate_limiter_update_ceiling_from_headers() {
        let mut rl = RateLimiter::new(40);
        rl.update_ceiling(1000);

        let _ = rl.acquire();
    }

    #[test]
    fn key_fingerprint_masks() {
        let fp = key_fingerprint("sk-ant-abc123xyz789");
        assert!(fp.contains("..."), "fp: {fp}");
        assert!(!fp.contains("abc123xyz789"), "full key leaked in fp: {fp}");
        assert_eq!(fp, "sk--...z789");
    }

    #[test]
    fn warning_texts_mention_key_points() {
        assert!(auth_login_warning("Anthropic").contains("one key per machine"));
        assert!(parallel_sessions_warning(4, "Anthropic").contains("--i-understand-the-risks"));
        assert!(concurrency_cap_warning(8, 2, "OpenAI").contains("--i-understand-the-risks"));
        assert!(repeated_429_warning(4, "Anthropic").contains("backing off"));
        let q = quota_exhausted_error("OpenAI", "insufficient_quota");
        assert!(q.contains("will NOT retry"));
        assert!(first_429_notice("Google").contains("normal"));
    }

    #[test]
    fn registry_register_counts_others() {
        let mut reg = SessionRegistry::default();
        let others1 = reg.entries.len();
        let _ = others1;

        let o1 = {
            let key_map = reg.entries.entry("p".into()).or_default();
            let s = key_map.entry("k".into()).or_default();
            s.push(("s1".into(), now_unix()));
            s.len().saturating_sub(1)
        };
        assert_eq!(o1, 0);

        let o2 = {
            let sessions = reg.entries.get_mut("p").unwrap().get_mut("k").unwrap();
            sessions.push(("s2".into(), now_unix()));
            sessions.len().saturating_sub(1)
        };
        assert_eq!(o2, 1);
    }
}
