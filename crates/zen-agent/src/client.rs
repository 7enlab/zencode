//! OpenAI-compatible chat-completions client.
//!
//! This module owns the only place in zen-agent that speaks HTTP. It implements
//! [`zen_core::LlmBackend`] by POSTing to `{base_url}/chat/completions` and turning the
//! wire response into a core [`ChatResponse`]. Everything above it (the agent loop, compaction,
//! sub-agents) talks to the `LlmBackend` trait, so tests can swap in a scripted backend and the
//! CLI can swap in a mock gateway without touching the loop.
//!
//! Key design decisions:
//!
//! - **Retry policy lives here, not in the loop.** Failures are classified *before* retrying
//!   ([`crate::safety::classify_error`]): `rate_limit_exceeded`/429, 529-overloaded and
//!   transient 5xx/network errors retry with exponential backoff + +-25% jitter
//!   (`base_retry_delay_ms * 2^(attempt-1)`), always raised to the server's `Retry-After` /
//!   `retry_after_seconds` hint, up to 5 retries or the total wait budget `max_total_wait_ms`
//!   (settings `retry.maxWaitSecs`, default 300 s). `insufficient_quota`/billing errors are
//!   NEVER retried (hard stop with a clear message); other 4xx fail fast. Each retry emits
//!   [`AgentEvent::ApiRetry`] so stream-json users can see the retry, matching SPEC 3.6.
//!   A 200 carrying an `error` object, no choices or an empty message is resampled the
//!   same way (r7 C3).
//! - **Request pacing.** A per-provider token bucket ([`crate::safety::RateLimiter`]) enforces
//!   a 100 ms inter-request floor and proactive slowdown at 80% of the detected ceiling.
//! - **Client identity.** Every request carries `User-Agent: ZenCode/<version>
//!   (+https://zencodego.com)` so providers can identify the client.
//! - **Model fallback lives in the agent loop** ([`crate::agent`]): once this client has
//!   exhausted its retries it returns `Err`, and the loop tries the next configured model.
//! - **Lenient parsing.** Free gateways commonly send `content: null` alongside `tool_calls`,
//!   an empty-string `arguments`, or `arguments` as an already-decoded JSON value rather than a
//!   string. All of those are normalized here so the loop only ever sees the canonical shape.
//! - **Never leak the API key.** The key is only ever put in the `Authorization` header; error
//!   strings contain the status and (truncated) response body, never request headers.
//! - **Local gateways must not go through a proxy.** When `base_url` points at localhost the
//!   reqwest client is built with `no_proxy()`, so a developer's `HTTP_PROXY` cannot intercept
//!   requests to test servers.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zen_core::*;
use crate::safety::{self, ErrorClass, RateLimiter};

/// Total attempts for one `chat` call: 1 try + 5 retries (see `safety::MAX_RETRY_ATTEMPTS`).
const MAX_ATTEMPTS: u32 = 6;
/// How much of an error body is quoted back in an error message.
const ERROR_BODY_CHARS: usize = 300;
/// Default total backoff budget for one `chat` call (settings `retry.maxWaitSecs`).
pub const DEFAULT_MAX_TOTAL_WAIT_MS: u64 = 300_000;

/// HTTP client for any OpenAI-compatible `/chat/completions` endpoint.
///
/// Cheap to construct and safe to share: it owns a pooled [`reqwest::Client`]. The default
/// retry base delay is 1 s; tests override [`OpenAiClient::base_retry_delay_ms`] to keep the
/// suite fast.
pub struct OpenAiClient {
    /// Base URL; `/chat/completions` is appended (a trailing `/` is tolerated).
    /// Public so the CLI can print it in `/status`.
    pub base_url: String,
    api_key: Option<String>,
    /// Event sink used for [`AgentEvent::ApiRetry`].
    /// Public so callers can swap in an output formatter after construction.
    pub sink: Arc<dyn EventSink>,
    /// Base backoff delay; attempt N waits `base_retry_delay_ms * 2^(N-1)`. Public for tests.
    pub base_retry_delay_ms: u64,
    /// Upper bound for a server-requested wait (`Retry-After` / `retry_after_seconds`). Public
    /// for tests.
    pub max_retry_delay_ms: u64,
    /// Total time one `chat` call may spend sleeping between retries (settings
    /// `retry.maxWaitSecs`, default 300 s). Headless runs never hang longer than this on a
    /// rate-limited or flapping gateway. Public for tests and the CLI.
    pub max_total_wait_ms: u64,
    http: reqwest::Client,
    /// Per-provider request pacer (token bucket + 100 ms floor). `None` disables pacing
    /// (e.g., local models with no provider to anger). Shared across clones.
    pacer: Arc<Mutex<Option<RateLimiter>>>,
    /// Timestamps of recent HTTP 429s (for the repeated-429 user warning).
    recent_429s: Arc<Mutex<Vec<Instant>>>,
    /// Human-readable provider name for warnings (e.g. "OpenAI", "Anthropic").
    provider_name: String,
}

/// Add PEM roots from `SSL_CERT_FILE` so zen works behind a TLS-inspecting proxy
/// (see `zen_core::extra_root_pem`).
fn with_extra_roots(mut b: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    if let Some(pem) = zen_core::extra_root_pem() {
        if let Ok(certs) = reqwest::Certificate::from_pem_bundle(&pem) {
            for c in certs {
                b = b.add_root_certificate(c);
            }
        }
    }
    b
}

/// True when the URL targets the local machine, where a proxy would usually break the request.
fn is_local_url(url: &str) -> bool {
    let b = url.to_ascii_lowercase();
    b.contains("://127.0.0.1") || b.contains("://localhost") || b.contains("://[::1]")
}

/// Best-effort human-readable provider name from a base URL (for warnings).
fn provider_name_for_url(url: &str) -> String {
    let l = url.to_ascii_lowercase();
    if l.contains("anthropic") {
        "Anthropic".to_string()
    } else if l.contains("openai") {
        "OpenAI".to_string()
    } else if l.contains("google") || l.contains("generativelanguage") {
        "Google".to_string()
    } else if l.contains("openrouter") {
        "OpenRouter".to_string()
    } else if l.contains("groq") {
        "Groq".to_string()
    } else if l.contains("ollama") || is_local_url(url) {
        "local".to_string()
    } else {
        "provider".to_string()
    }
}

impl OpenAiClient {
    /// Build a client. `api_key` may be `None` (local gateways often need no auth); an empty
    /// string is treated as absent. `sink` receives retry events.
    pub fn new(base_url: impl Into<String>, api_key: Option<String>, sink: Arc<dyn EventSink>) -> Self {
        let base_url = base_url.into();
        let mut builder = with_extra_roots(
            reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .user_agent(safety::user_agent()),
        );
        if is_local_url(&base_url) {
            builder = builder.no_proxy();
        }
        let http = builder.build().unwrap_or_else(|_| reqwest::Client::new());
        // Pacing is skipped for local endpoints (nothing to anger) and enabled
        // otherwise, using conservative per-provider defaults.
        let pacer = RateLimiter::for_provider(&base_url);
        let provider_name = provider_name_for_url(&base_url);
        Self {
            base_url,
            api_key,
            sink,
            base_retry_delay_ms: 1000,
            max_retry_delay_ms: 60_000,
            max_total_wait_ms: DEFAULT_MAX_TOTAL_WAIT_MS,
            http,
            pacer: Arc::new(Mutex::new(pacer)),
            recent_429s: Arc::new(Mutex::new(Vec::new())),
            provider_name,
        }
    }

    /// Override the human-readable provider name used in warnings. Public for tests.
    pub fn with_provider_name(mut self, name: impl Into<String>) -> Self {
        self.provider_name = name.into();
        self
    }

    /// Number of 429s seen in the last 5 minutes (for the repeated-429 warning).
    pub fn recent_429_count(&self) -> usize {
        let mut v = self.recent_429s.lock().unwrap();
        let cutoff = Instant::now() - Duration::from_secs(5 * 60);
        v.retain(|t| *t >= cutoff);
        v.len()
    }

    fn note_429(&self) {
        self.recent_429s.lock().unwrap().push(Instant::now());
    }

    /// Feed live `x-ratelimit-*` / `anthropic-ratelimit-*` headers into the pacer.
    /// Live headers always override the conservative defaults upward.
    fn update_pacer_from_headers(&self, headers: &reqwest::header::HeaderMap) {
        let mut guard = self.pacer.lock().unwrap();
        let Some(pacer) = guard.as_mut() else { return };
        for name in [
            "x-ratelimit-limit-requests",
            "anthropic-ratelimit-requests-limit",
            "x-ratelimit-limit",
        ] {
            if let Some(v) = headers.get(name).and_then(|h| h.to_str().ok()) {
                if let Ok(rpm) = v.trim().parse::<u64>() {
                    if rpm > 0 {
                        pacer.update_ceiling(rpm);
                        return;
                    }
                }
            }
        }
    }

    /// The API key, if any. Kept private-ish so no code path accidentally formats it.
    #[allow(dead_code)]
    fn key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    /// `{base_url}/chat/completions` with a normalized single slash.
    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    /// Attach `Authorization: Bearer <key>` when a usable key exists.
    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_key {
            Some(k) if !k.is_empty() => {
                rb.header(reqwest::header::AUTHORIZATION, format!("Bearer {}", k))
            }
            _ => rb,
        }
    }
}

#[derive(Debug, Deserialize)]
struct WireResp {
    #[serde(default)]
    choices: Vec<WireChoice>,
    #[serde(default)]
    usage: Usage,
}
#[derive(Debug, Deserialize)]
struct WireChoice {
    message: WireMsg,
    #[serde(default)]
    finish_reason: Option<String>,
}
#[derive(Debug, Deserialize)]
struct WireMsg {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<WireCall>>,
}
#[derive(Debug, Deserialize)]
struct WireCall {
    id: String,
    function: WireFunc,
}
#[derive(Debug, Deserialize)]
struct WireFunc {
    name: String,
    /// Free gateways sometimes send `null`, `""`, or an already-decoded object.
    #[serde(default)]
    arguments: Option<Value>,
}

/// Normalize a wire `arguments` value into the JSON-encoded string form used internally.
///
/// `null`/missing/blank become `"{}"`; a JSON value is re-encoded; a string is passed through.
/// A leading zero-length string is a real pattern in OpenRouter-style responses for no-arg tools.
pub(crate) fn normalize_arguments(v: Option<Value>) -> String {
    match v {
        None | Some(Value::Null) => "{}".to_string(),
        Some(Value::String(s)) if s.trim().is_empty() => "{}".to_string(),
        Some(Value::String(s)) => s,
        Some(other) => other.to_string(),
    }
}

/// Wait the server asked for, in ms: a `Retry-After: <seconds>` header, else an
/// `error.retry_after_seconds` / `retry_after_seconds` field in a JSON error body (free gateways
/// rate-limit in 20–60 s windows, longer than our own backoff).
fn server_retry_hint_ms(header: Option<&str>, body: &str) -> Option<u64> {
    if let Some(secs) = header.and_then(|h| h.trim().parse::<f64>().ok()) {
        return Some((secs.max(0.0) * 1000.0) as u64);
    }
    let v: Value = serde_json::from_str(body).ok()?;
    let secs = v
        .pointer("/error/retry_after_seconds")
        .or_else(|| v.get("retry_after_seconds"))
        .and_then(|s| s.as_f64().or_else(|| s.as_str().and_then(|t| t.parse().ok())))?;
    Some((secs.max(0.0) * 1000.0) as u64)
}

/// First `n` characters of `s`, used to truncate error bodies.
fn first_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[async_trait]
impl LlmBackend for OpenAiClient {
    async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse> {
        let url = self.endpoint();
        let mut waited_ms: u64 = 0;
        let mut attempt: u32 = 0;
        // Warn once per session on the first 429 (info, not alarming).
        let mut warned_first_429 = false;

        loop {
            attempt += 1;

            // ── Pacing: token bucket + 100 ms floor ──
            let pace_wait = self
                .pacer
                .lock()
                .unwrap()
                .as_mut()
                .map(|p| p.acquire())
                .unwrap_or(Duration::ZERO);
            if !pace_wait.is_zero() {
                tokio::time::sleep(pace_wait).await;
            }

            let rb = self.auth(self.http.post(&url).json(&req));
            let outcome = rb.send().await;

            // Classify the attempt BEFORE deciding to retry. Billing/quota errors
            // are never retried; other 4xx fail fast.
            let mut server_hint_ms: Option<u64> = None;
            enum Outcome {
                Parsed(anyhow::Result<ChatResponse>),
                Classified(ErrorClass, Option<u16>, String),
            }
            let outcome: Outcome = match outcome {
                Ok(resp) => {
                    let code = resp.status().as_u16();
                    // Feed live rate-limit headers into the pacer (they override defaults).
                    self.update_pacer_from_headers(resp.headers());
                    if resp.status().is_success() {
                        let body = resp.text().await?;
                        // A 200 that carries a gateway error, no choices, or an empty message is
                        // a sampling glitch (gpt-oss Harmony header errors): resample (r7 C3).
                        match transient_completion(&body) {
                            Some(why)
                                if attempt < MAX_ATTEMPTS =>
                            {
                                Outcome::Classified(
                                    ErrorClass::Transient,
                                    Some(code),
                                    why,
                                )
                            }
                            _ => Outcome::Parsed(parse_completion(&body)),
                        }
                    } else {
                        let header = resp
                            .headers()
                            .get(reqwest::header::RETRY_AFTER)
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string);
                        let body = resp.text().await.unwrap_or_default();
                        server_hint_ms = server_retry_hint_ms(header.as_deref(), &body);
                        let class = safety::classify_error(Some(code), &body);
                        if code == 429 {
                            self.note_429();
                        }
                        Outcome::Classified(
                            class,
                            Some(code),
                            format!("HTTP {}: {}", code, first_chars(&body, ERROR_BODY_CHARS)),
                        )
                    }
                }
                Err(e) => Outcome::Classified(
                    ErrorClass::Transient,
                    None,
                    format!("network error: {}", e),
                ),
            };

            let (class, status, err_text) = match outcome {
                Outcome::Parsed(r) => return r,
                Outcome::Classified(c, st, e) => (c, st, e),
            };

            // ── Hard stops: never retry these ──
            match class {
                ErrorClass::QuotaExhausted => {
                    return Err(anyhow::anyhow!(safety::quota_exhausted_error(
                        &self.provider_name,
                        &err_text
                    )));
                }
                ErrorClass::Fatal => {
                    return Err(anyhow::anyhow!(err_text));
                }
                ErrorClass::RateLimit | ErrorClass::Overloaded | ErrorClass::Transient => {}
            }

            // ── Retryable: backoff with jitter, max 5 retries ──
            if attempt >= MAX_ATTEMPTS {
                return Err(anyhow::anyhow!(err_text));
            }
            let budget_left = self.max_total_wait_ms.saturating_sub(waited_ms);
            if budget_left == 0 {
                return Err(anyhow::anyhow!(
                    "{} (gave up after {} attempts: retry wait budget of {} s spent; settings retry.maxWaitSecs)",
                    err_text,
                    attempt,
                    self.max_total_wait_ms / 1000
                ));
            }

            // First 429 in this session: one info-level notice.
            if status == Some(429) && !warned_first_429 {
                warned_first_429 = true;
                eprintln!("{}", safety::first_429_notice(&self.provider_name));
            }
            // Repeated 429s: stronger warning.
            let recent = self.recent_429_count();
            if recent > 3 {
                eprintln!("{}", safety::repeated_429_warning(recent as u32, &self.provider_name));
            }

            // Backoff with ±25% jitter, always raised to the server's Retry-After hint,
            // capped per-wait and clipped to the budget left.
            let jittered = safety::backoff_with_jitter(
                attempt,
                self.base_retry_delay_ms,
                self.max_retry_delay_ms,
                safety::jitter_now_nanos(),
            );
            let delay_ms = jittered
                .max(server_hint_ms.unwrap_or(0).min(self.max_retry_delay_ms))
                .min(self.max_retry_delay_ms.max(self.base_retry_delay_ms))
                .min(budget_left);
            // For 529 (provider overloaded), suggest fallback in the event.
            let mut err_for_event = err_text.clone();
            if class == ErrorClass::Overloaded {
                err_for_event.push_str(" (provider overloaded — consider a fallback provider)");
            }
            self.sink.emit(AgentEvent::ApiRetry {
                attempt,
                max_retries: MAX_ATTEMPTS - 1,
                retry_delay_ms: delay_ms,
                error_status: status,
                error: err_for_event,
            });
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            waited_ms = waited_ms.saturating_add(delay_ms);
        }
    }

}

/// Why a 200 body is worth resampling, if it is: an `error` object instead of choices (vLLM
/// returns gpt-oss `HarmonyError: unexpected tokens remaining in message header` this way; the
/// upstream advice is to catch and retry, https://github.com/vllm-project/vllm/issues/23567),
/// no choices at all, or an assistant message with neither text nor tool calls. `None` for a
/// normal completion or a body that is not JSON (that stays a hard parse error).
pub(crate) fn transient_completion(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
        let msg = err.get("message").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| err.to_string());
        return Some(format!("gateway error in 200 response: {}", first_chars(&msg, ERROR_BODY_CHARS)));
    }
    let first = match v.get("choices").and_then(Value::as_array).and_then(|c| c.first()) {
        Some(c) => c,
        None => return Some("model returned no choices".into()),
    };
    let msg = first.get("message")?;
    let has_text = msg.get("content").and_then(Value::as_str).is_some_and(|t| !t.trim().is_empty());
    let has_calls = msg.get("tool_calls").and_then(Value::as_array).is_some_and(|c| !c.is_empty());
    (!has_text && !has_calls).then(|| "model returned an empty message".into())
}

/// Parse a success body into a [`ChatResponse`], normalizing gateway quirks.
///
/// Returns a clear error (including a body excerpt) for malformed JSON or an empty `choices`
/// array, which otherwise would surface as a confusing panic downstream.
pub(crate) fn parse_completion(body: &str) -> anyhow::Result<ChatResponse> {
    let parsed: WireResp = serde_json::from_str(body).map_err(|e| {
        anyhow::anyhow!(
            "invalid completion response: {} (body: {})",
            e,
            first_chars(body, ERROR_BODY_CHARS)
        )
    })?;
    let choice = parsed
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| {
            anyhow::anyhow!("model returned no choices (body: {})", first_chars(body, ERROR_BODY_CHARS))
        })?;
    let tool_calls = choice.message.tool_calls.map(|calls| {
        calls
            .into_iter()
            .map(|c| ToolCall {
                id: c.id,
                call_type: "function".into(),
                function: FunctionCall {
                    name: c.function.name,
                    arguments: normalize_arguments(c.function.arguments),
                },
            })
            .collect::<Vec<_>>()
    });
    let content = match choice.message.content {
        // Some gateways echo an empty string with tool_calls; keep None so the loop's
        // "no text" branch is unambiguous.
        Some(s) if s.is_empty() && tool_calls.is_some() => None,
        other => other,
    };
    let mut usage = parsed.usage;
    // OpenAI's automatic prompt cache reports via
    // `usage.prompt_tokens_details.cached_tokens`; surfacing it here makes the
    // free cache-hit observability that `zen cost` displays.
    usage.cache_read_input_tokens = openai_cached_tokens(body);
    Ok(ChatResponse {
        message: ChatMessage::assistant(content, tool_calls),
        usage,
        finish_reason: choice.finish_reason,
    })
}

/// Read `usage.prompt_tokens_details.cached_tokens` from an OpenAI-compatible
/// response body (0 when absent). OpenAI's prompt caching is fully automatic:
/// any stable prefix >= 1,024 tokens is served at ~10% of the input price.
pub(crate) fn openai_cached_tokens(body: &str) -> u64 {
    let v: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return 0,
    };
    v.pointer("/usage/prompt_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

/// One `chat` call with a model-fallback chain: primary model first, then each fallback once.
///
pub(crate) async fn chat_with_fallback(
    llm: &Arc<dyn LlmBackend>,
    primary: &str,
    fallbacks: &[String],
    messages: Vec<ChatMessage>,
    tools: Vec<ToolDefinition>,
    max_tokens: Option<u32>,
) -> anyhow::Result<(ChatResponse, u64, String)> {
    let mut candidates = Vec::with_capacity(fallbacks.len() + 1);
    candidates.push(primary.to_string());
    candidates.extend(fallbacks.iter().cloned());

    let mut last_err: Option<String> = None;
    let mut api_ms_total = 0u64;
    for model in candidates {
        let req = ChatRequest {
            model: model.clone(),
            messages: messages.clone(),
            tools: tools.clone(),
            max_tokens,
        };
        let started = Instant::now();
        let res = llm.chat(req).await;
        api_ms_total += started.elapsed().as_millis() as u64;
        match res {
            Ok(resp) => return Ok((resp, api_ms_total, model)),
            Err(e) => last_err = Some(e.to_string()),
        }
    }
    Err(anyhow::anyhow!(
        last_err.unwrap_or_else(|| "model request failed".into())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalize_arguments_handles_gateway_quirks() {
        assert_eq!(normalize_arguments(None), "{}");
        assert_eq!(normalize_arguments(Some(Value::Null)), "{}");
        assert_eq!(normalize_arguments(Some(json!(""))), "{}");
        assert_eq!(normalize_arguments(Some(json!("  "))), "{}");
        assert_eq!(normalize_arguments(Some(json!(r#"{"a":1}"#))), r#"{"a":1}"#);
        assert_eq!(normalize_arguments(Some(json!({"a": 1}))), r#"{"a":1}"#);
    }

    #[test]
    fn parse_completion_nulls_empty_content_with_tool_calls() {
        let body = json!({
            "choices": [{
                "message": {
                    "content": "",
                    "tool_calls": [{ "id": "c", "function": { "name": "Read", "arguments": null } }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7 }
        })
        .to_string();
        let r = parse_completion(&body).unwrap();
        assert!(r.message.content.is_none());
        assert_eq!(r.message.tool_calls.unwrap()[0].function.arguments, "{}");
        assert_eq!(r.usage.total_tokens, 7);
    }

    #[test]
    fn parse_completion_reads_openai_cached_tokens() {
        // OpenAI's automatic prompt cache reports via
        // `usage.prompt_tokens_details.cached_tokens`.
        let body = json!({
            "choices": [{"message": {"content": "hi"}, "finish_reason": "stop"}],
            "usage": {
                "prompt_tokens": 2000,
                "completion_tokens": 2,
                "total_tokens": 2002,
                "prompt_tokens_details": {"cached_tokens": 1500, "audio_tokens": 0}
            }
        })
        .to_string();
        let r = parse_completion(&body).unwrap();
        assert_eq!(r.usage.prompt_tokens, 2000);
        assert_eq!(r.usage.cache_read_input_tokens, 1500);
        assert!((zen_core::cache_hit_rate(r.usage.cache_read_input_tokens, r.usage.prompt_tokens) - 0.75).abs() < 1e-9);
    }

    #[test]
    fn parse_completion_cached_tokens_absent_is_zero() {
        let body = json!({
            "choices": [{"message": {"content": "hi"}, "finish_reason": "stop"}],
            "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
        })
        .to_string();
        let r = parse_completion(&body).unwrap();
        assert_eq!(r.usage.cache_read_input_tokens, 0);
    }

    #[test]
    fn parse_completion_errors_are_actionable() {
        let e = parse_completion("not json").unwrap_err().to_string();
        assert!(e.contains("invalid completion response"));
        assert!(e.contains("not json"));
        let e = parse_completion(r#"{"choices":[]}"#).unwrap_err().to_string();
        assert!(e.contains("no choices"));
    }

    #[tokio::test]
    async fn fallback_chain_reports_answering_model() {
        use std::sync::Mutex;
        struct Backend(Mutex<Vec<&'static str>>);
        #[async_trait]
        impl LlmBackend for Backend {
            async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse> {
                let mut q = self.0.lock().unwrap();
                if q.is_empty() {
                    anyhow::bail!("out of scripted models")
                }
                let expected = q.remove(0);
                assert_eq!(req.model, expected);
                Ok(ChatResponse {
                    message: ChatMessage::assistant(Some("ok".into()), None),
                    usage: Usage::default(),
                    finish_reason: None,
                })
            }
        }
        let backend: Arc<dyn LlmBackend> =
            Arc::new(Backend(Mutex::new(vec!["fallback"])));
        // Primary fails on the first scripted slot.
        struct FailOnce(Mutex<bool>, Arc<dyn LlmBackend>);
        #[async_trait]
        impl LlmBackend for FailOnce {
            async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse> {
                let first = {
                    let mut failed = self.0.lock().unwrap();
                    !std::mem::replace(&mut *failed, true)
                };
                if first {
                    assert_eq!(req.model, "primary");
                    anyhow::bail!("primary down");
                }
                self.1.chat(req).await
            }
        }
        let llm: Arc<dyn LlmBackend> = Arc::new(FailOnce(Mutex::new(false), backend));
        let (resp, _ms, model) = chat_with_fallback(
            &llm,
            "primary",
            &["fallback".to_string()],
            vec![ChatMessage::user("hi")],
            vec![],
            None,
        )
        .await
        .unwrap();
        assert_eq!(model, "fallback");
        assert_eq!(resp.message.content.as_deref(), Some("ok"));
    }

    #[test]
    fn server_retry_hint_is_honoured_and_capped() {
        let body = r#"{"error":{"message":"slow down","retry_after_seconds":23}}"#;
        assert_eq!(server_retry_hint_ms(None, body), Some(23_000));
        assert_eq!(server_retry_hint_ms(Some("5"), body), Some(5_000), "header wins");
        assert_eq!(server_retry_hint_ms(None, r#"{"retry_after_seconds":"2"}"#), Some(2_000));
        assert_eq!(server_retry_hint_ms(None, "not json"), None);
        assert_eq!(server_retry_hint_ms(Some("Wed, 21 Oct 2026 07:28:00 GMT"), "{}"), None);
        // backoff_with_jitter (safety): attempt 1 ~1 s +/-25%, attempt 3 ~4 s +/-25%,
        // always capped at 60 s. Deterministic for a fixed seed.
        use crate::safety::backoff_with_jitter;
        let b1 = backoff_with_jitter(1, 1000, 60_000, 42);
        assert!((750..=1250).contains(&b1), "b1={b1}");
        let b3 = backoff_with_jitter(3, 1000, 60_000, 42);
        assert!((3000..=5000).contains(&b3), "b3={b3}");
        let bcap = backoff_with_jitter(20, 1000, 60_000, 42);
        assert!(bcap <= 60_000, "bcap={bcap}");
    }

    #[test]
    fn local_url_detection() {
        assert!(is_local_url("http://127.0.0.1:8080/v1"));
        assert!(is_local_url("http://LOCALHOST:1234"));
        assert!(is_local_url("http://[::1]:5/v1"));
        assert!(!is_local_url("https://api.example.com"));
    }

    #[test]
    fn c3_transient_completion_classifies_200_bodies() {
        let harmony = r#"{"error":{"message":"unexpected tokens remaining in message header: Some(\"to=functions.Read\")","code":400},"object":"chat.completion"}"#;
        assert!(transient_completion(harmony).unwrap().contains("unexpected tokens remaining"));
        assert!(transient_completion(r#"{"choices":[]}"#).unwrap().contains("no choices"));
        let empty = r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[]},"finish_reason":"stop"}]}"#;
        assert!(transient_completion(empty).unwrap().contains("empty message"));
        let blank = r#"{"choices":[{"message":{"content":"  \n"},"finish_reason":"stop"}]}"#;
        assert!(transient_completion(blank).is_some());
        let text = r#"{"choices":[{"message":{"content":"done"},"finish_reason":"stop"}]}"#;
        assert!(transient_completion(text).is_none());
        let call = r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"a","function":{"name":"Read","arguments":"{}"}}]}}]}"#;
        assert!(transient_completion(call).is_none());
        assert!(transient_completion("not json").is_none(), "malformed JSON stays a hard error");
    }

    /// Real HTTP: a gateway answers 200 + Harmony error, then 200 + empty message, then a real
    /// answer. The client resamples twice (with ApiRetry events) and returns the answer.
    #[tokio::test]
    async fn c3_client_resamples_gateway_error_and_empty_message() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let bodies = [
            r#"{"error":{"message":"unexpected tokens remaining in message header: Some(\"to=functions.Bash\")"}}"#,
            r#"{"choices":[{"message":{"content":null,"tool_calls":[]},"finish_reason":"stop"}]}"#,
            r#"{"choices":[{"message":{"content":"fixed"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#,
        ];
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for b in bodies {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 65536];
                let mut got = Vec::new();
                loop {
                    let n = sock.read(&mut buf).await.unwrap();
                    got.extend_from_slice(&buf[..n]);
                    let txt = String::from_utf8_lossy(&got);
                    if let Some(h) = txt.find("\r\n\r\n") {
                        let len = txt[..h].lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap())).unwrap_or(0);
                        if got.len() >= h + 4 + len { break; }
                    }
                    if n == 0 { break; }
                }
                let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", b.len(), b);
                sock.write_all(resp.as_bytes()).await.unwrap();
            }
        });
        let sink = Arc::new(crate::testutil::CollectSink::default());
        let mut c = OpenAiClient::new(format!("http://{addr}/v1"), None, sink.clone());
        c.base_retry_delay_ms = 1;
        let req = ChatRequest { model: "m".into(), messages: vec![ChatMessage::user("hi")], tools: vec![], max_tokens: None };
        let r = c.chat(req).await.unwrap();
        assert_eq!(r.message.content.as_deref(), Some("fixed"));
        let retries = sink.events.lock().unwrap().iter().filter(|e| matches!(e, AgentEvent::ApiRetry { .. })).count();
        assert_eq!(retries, 2);
    }
}

// ─── Anthropic Messages API client (with prompt caching) ────────────────────
//
// Prompt caching design (see ~/workspace/zencode-token-saving-research.md §1):
// - Breakpoints (`cache_control: {"type": "ephemeral"}`) sit after the *latest
//   stable* content: the end of the tool definitions and the end of the stable
//   system block. They are NEVER placed after the newest message — doing so
//   rewrites the cache from scratch every turn.
// - Cache hierarchy is tools → system → messages, so tool definitions must be a
//   *stable bundle*: the same tools in the same order every turn. Recomposing
//   the tool list per request silently invalidates every cache level after it.
// - Pricing: cache reads = 10% of input price (90% off); cache writes = 1.25x.
//   Real-world agent trajectories: -49% to -80% token cost (LangChain Deep
//   Agents eval). The 5-minute TTL is refreshed by every read, so an active
//   agent loop stays warm; `zen cost` surfaces hit rates so silent misses are
//   visible.

/// Anthropic API version header (pinned).
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Beta header enabling prompt caching on the Messages API.
const ANTHROPIC_CACHE_BETA: &str = "prompt-caching-2024-07-31";
/// Default `max_tokens` when the request does not set one (Anthropic requires it).
const ANTHROPIC_DEFAULT_MAX_TOKENS: u32 = 4096;

/// True when the URL is an Anthropic Messages API endpoint.
pub fn is_anthropic_url(base_url: &str) -> bool {
    let u = base_url.trim().to_ascii_lowercase();
    u.contains("anthropic.com") || u.contains("api.anthropic")
}

/// One prompt-caching breakpoint: `cache_control: {"type": "ephemeral"}`.
fn cache_control() -> Value {
    serde_json::json!({"type": "ephemeral"})
}

/// HTTP client for Anthropic's Messages API (`POST {base_url}/messages`).
///
/// Translates zen-core's OpenAI-shaped types to Anthropic's format and back, so the
/// agent loop and every caller of [`LlmBackend`] work unchanged.
/// Cheap to construct and safe to share: it owns a pooled [`reqwest::Client`].
pub struct AnthropicClient {
    /// Base URL; `/messages` is appended (a trailing `/` is tolerated).
    /// Public so the CLI can print it in `/status`.
    pub base_url: String,
    api_key: Option<String>,
    /// Event sink used for [`AgentEvent::ApiRetry`].
    pub sink: Arc<dyn EventSink>,
    /// Base backoff delay; attempt N waits `base_retry_delay_ms * 2^(N-1)`. Public for tests.
    pub base_retry_delay_ms: u64,
    /// Upper bound for a server-requested wait. Public for tests.
    pub max_retry_delay_ms: u64,
    /// Total time one `chat` call may spend sleeping between retries. Public for tests/CLI.
    pub max_total_wait_ms: u64,
    http: reqwest::Client,
}

impl AnthropicClient {
    /// Build a client. `api_key` may be `None`; an empty string is treated as absent.
    pub fn new(base_url: impl Into<String>, api_key: Option<String>, sink: Arc<dyn EventSink>) -> Self {
        let base_url = base_url.into();
        let mut builder =
            with_extra_roots(reqwest::Client::builder().timeout(Duration::from_secs(120)));
        if is_local_url(&base_url) {
            builder = builder.no_proxy();
        }
        let http = builder.build().unwrap_or_else(|_| reqwest::Client::new());
        Self {
            base_url,
            api_key: api_key.filter(|k| !k.is_empty()),
            sink,
            base_retry_delay_ms: 1000,
            max_retry_delay_ms: 60_000,
            max_total_wait_ms: DEFAULT_MAX_TOTAL_WAIT_MS,
            http,
        }
    }

    fn messages_url(&self) -> String {
        format!("{}/messages", self.base_url.trim_end_matches('/'))
    }
}

/// Split the system prompt at [`zen_core::CACHE_VOLATILE_MARKER`] and build the
/// Anthropic `system` block array.
///
/// The stable prefix gets an explicit `cache_control` breakpoint; the volatile
/// tail (date, git branch/status) rides after it uncached. When the marker is
/// absent the whole prompt is one cached block.
pub(crate) fn anthropic_system_blocks(system_text: &str) -> Vec<Value> {
    let (stable, volatile) = zen_core::split_stable_volatile(system_text);
    let mut blocks = Vec::with_capacity(2);
    if !stable.is_empty() {
        blocks.push(serde_json::json!({
            "type": "text",
            "text": stable,
            "cache_control": cache_control(),
        }));
    }
    if !volatile.is_empty() {
        blocks.push(serde_json::json!({"type": "text", "text": volatile}));
    }
    blocks
}

/// Build the Anthropic request body from a [`ChatRequest`], with prompt-caching
/// breakpoints after the tool definitions and after the stable system block.
///
/// System messages are hoisted to the top-level `system` field; `role: "tool"`
/// messages become `tool_result` blocks inside a `user` message; assistant
/// `tool_calls` become `tool_use` blocks.
pub(crate) fn anthropic_request_body(req: &ChatRequest) -> Value {
    let mut system_parts: Vec<String> = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    for m in &req.messages {
        match m.role.as_str() {
            "system" => {
                if let Some(c) = m.content.as_deref().filter(|c| !c.is_empty()) {
                    system_parts.push(c.to_string());
                }
            }
            "tool" => {
                messages.push(serde_json::json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": m.tool_call_id.as_deref().unwrap_or(""),
                        "content": m.content.as_deref().unwrap_or(""),
                    }],
                }));
            }
            "assistant" => {
                let mut blocks: Vec<Value> = Vec::new();
                if let Some(c) = m.content.as_deref().filter(|c| !c.is_empty()) {
                    blocks.push(serde_json::json!({"type": "text", "text": c}));
                }
                for tc in m.tool_calls.iter().flatten() {
                    let input: Value = serde_json::from_str(&tc.function.arguments).unwrap_or(Value::Null);
                    blocks.push(serde_json::json!({
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.function.name,
                        "input": input,
                    }));
                }
                messages.push(serde_json::json!({"role": "assistant", "content": blocks}));
            }
            _ => {
                // "user" and anything else: plain text.
                messages.push(serde_json::json!({
                    "role": "user",
                    "content": m.content.as_deref().unwrap_or(""),
                }));
            }
        }
    }
    // Cache-aware ordering: tools first (Anthropic caches them before everything
    // else), then system, then messages. The tool bundle must be stable across
    // turns — the breakpoint goes on the LAST tool so the whole block is cached.
    let tools: Vec<Value> = req
        .tools
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut tool = serde_json::json!({
                "name": t.function.name,
                "description": t.function.description,
                "input_schema": t.function.parameters,
            });
            if i + 1 == req.tools.len() {
                tool["cache_control"] = cache_control();
            }
            tool
        })
        .collect();
    let mut body = serde_json::json!({
        "model": req.model,
        "max_tokens": req.max_tokens.unwrap_or(ANTHROPIC_DEFAULT_MAX_TOKENS),
        "messages": messages,
    });
    if !system_parts.is_empty() {
        let blocks = anthropic_system_blocks(&system_parts.join("\n\n"));
        if !blocks.is_empty() {
            body["system"] = Value::Array(blocks);
        }
    }
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
    }
    body
}

/// Parse an Anthropic Messages API response body into a [`ChatResponse`],
/// including `cache_read_input_tokens` / `cache_creation_input_tokens` from the
/// usage block (0 when the provider omits them).
pub(crate) fn parse_anthropic_response(body: &str) -> anyhow::Result<ChatResponse> {
    let v: Value = serde_json::from_str(body).map_err(|e| {
        anyhow::anyhow!(
            "invalid anthropic response: {} (body: {})",
            e,
            first_chars(body, ERROR_BODY_CHARS)
        )
    })?;
    if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| err.to_string());
        anyhow::bail!("anthropic error: {}", first_chars(&msg, ERROR_BODY_CHARS));
    }
    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    for block in v.get("content").and_then(Value::as_array).into_iter().flatten() {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = block.get("text").and_then(Value::as_str) {
                    text_parts.push(t.to_string());
                }
            }
            Some("tool_use") => {
                let id = block.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                let name = block.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                let input = block.get("input").cloned().unwrap_or(Value::Null);
                tool_calls.push(ToolCall {
                    id,
                    call_type: "function".into(),
                    function: FunctionCall {
                        name,
                        arguments: serde_json::to_string(&input).unwrap_or_else(|_| "{}".into()),
                    },
                });
            }
            _ => {}
        }
    }
    let text = text_parts.join("");
    let content = if text.is_empty() && !tool_calls.is_empty() { None } else { Some(text) };
    let usage = v.get("usage");
    let u = |k: &str| usage.and_then(|u| u.get(k)).and_then(Value::as_u64).unwrap_or(0);
    Ok(ChatResponse {
        message: ChatMessage::assistant(
            content,
            if tool_calls.is_empty() { None } else { Some(tool_calls) },
        ),
        usage: Usage {
            prompt_tokens: u("input_tokens"),
            completion_tokens: u("output_tokens"),
            total_tokens: 0,
            cache_read_input_tokens: u("cache_read_input_tokens"),
            cache_creation_input_tokens: u("cache_creation_input_tokens"),
        },
        finish_reason: v.get("stop_reason").and_then(Value::as_str).map(str::to_string),
    })
}

#[async_trait]
impl LlmBackend for AnthropicClient {
    async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse> {
        let url = self.messages_url();
        let body = anthropic_request_body(&req);
        let mut attempt: u32 = 0;
        let mut waited_ms: u64 = 0;
        loop {
            attempt += 1;
            let mut rb = self.http.post(&url).json(&body);
            rb = rb.header("anthropic-version", ANTHROPIC_VERSION);
            rb = rb.header("anthropic-beta", ANTHROPIC_CACHE_BETA);
            if let Some(key) = &self.api_key {
                rb = rb.header("x-api-key", key);
            }
            let res = rb.send().await;
            let (retryable, long, status, err_text): (bool, bool, Option<u16>, String) = match res {
                Ok(resp) => {
                    let code = resp.status().as_u16();
                    let resp_body = resp.text().await.unwrap_or_default();
                    if resp_body.contains("\"type\":\"error\"") || resp_body.contains("\"type\": \"error\"") {
                        // Anthropic-style error object (also on non-2xx).
                        let msg = parse_anthropic_response(&resp_body)
                            .err()
                            .map(|e| e.to_string())
                            .unwrap_or_else(|| first_chars(&resp_body, ERROR_BODY_CHARS));
                        let retryable = matches!(code, 429 | 500 | 502 | 503 | 504 | 529);
                        (retryable, retryable, Some(code), format!("HTTP {}: {}", code, msg))
                    } else if (200..300).contains(&code) {
                        match parse_anthropic_response(&resp_body) {
                            Ok(parsed) => return Ok(parsed),
                            Err(e) => (false, false, Some(code), format!("bad anthropic body: {e}")),
                        }
                    } else {
                        let retryable = matches!(code, 429 | 500 | 502 | 503 | 504 | 529);
                        (
                            retryable,
                            retryable,
                            Some(code),
                            format!("HTTP {}: {}", code, first_chars(&resp_body, ERROR_BODY_CHARS)),
                        )
                    }
                }
                Err(e) => (true, false, None, format!("network error: {e}")),
            };

            if !retryable {
                return Err(anyhow::anyhow!(err_text));
            }
            // Retry discipline (#47): same attempt cap and jittered backoff as the OpenAI client.
            let _ = long;
            let attempt_cap = MAX_ATTEMPTS;
            let budget_left = self.max_total_wait_ms.saturating_sub(waited_ms);
            if attempt >= attempt_cap {
                return Err(anyhow::anyhow!(err_text));
            }
            if budget_left == 0 {
                return Err(anyhow::anyhow!(
                    "{} (gave up after {} attempts: retry wait budget of {} s spent; settings retry.maxWaitSecs)",
                    err_text,
                    attempt,
                    self.max_total_wait_ms / 1000
                ));
            }
            let delay_ms = safety::backoff_with_jitter(
                attempt.min(16),
                self.base_retry_delay_ms,
                self.max_retry_delay_ms,
                safety::jitter_now_nanos(),
            )
            .min(self.max_retry_delay_ms.max(self.base_retry_delay_ms))
            .min(budget_left);
            self.sink.emit(AgentEvent::ApiRetry {
                attempt,
                max_retries: attempt_cap - 1,
                retry_delay_ms: delay_ms,
                error_status: status,
                error: err_text.clone(),
            });
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            waited_ms = waited_ms.saturating_add(delay_ms);
        }
    }
}

/// Build the right backend for a base URL: Anthropic's Messages API (with prompt
/// caching) when the URL points at Anthropic, OpenAI-compatible
/// `/chat/completions` otherwise.
pub fn make_backend(
    base_url: impl Into<String>,
    api_key: Option<String>,
    sink: Arc<dyn EventSink>,
    max_total_wait_ms: u64,
) -> Arc<dyn LlmBackend> {
    let base_url = base_url.into();
    if is_anthropic_url(&base_url) {
        let mut c = AnthropicClient::new(base_url, api_key, sink);
        c.max_total_wait_ms = max_total_wait_ms;
        Arc::new(c)
    } else {
        let mut c = OpenAiClient::new(base_url, api_key, sink);
        c.max_total_wait_ms = max_total_wait_ms;
        Arc::new(c)
    }
}

// ─── Anthropic client tests ────────────────────────────────────────────────

#[cfg(test)]
mod anthropic_tests {
    use super::*;

    fn req() -> ChatRequest {
        ChatRequest {
            model: "claude-sonnet-4-5".to_string(),
            messages: vec![
                ChatMessage::system("You are helpful."),
                ChatMessage::user("hi"),
            ],
            tools: vec![],
            max_tokens: Some(500),
        }
    }

    #[test]
    fn is_anthropic_url_detection() {
        assert!(is_anthropic_url("https://api.anthropic.com"));
        assert!(is_anthropic_url("https://api.anthropic.com/v1"));
        assert!(!is_anthropic_url("https://openrouter.ai/api/v1"));
        assert!(!is_anthropic_url("https://api.openai.com/v1"));
        assert!(!is_anthropic_url("http://localhost:11434/v1"));
    }

    #[test]
    fn make_backend_routes_by_url() {
        let sink: Arc<dyn EventSink> = Arc::new(NullSink);
        // Anthropic URL → AnthropicClient; anything else → OpenAiClient. We verify
        // routing without network by downcasting the debug-free trait objects via
        // a second construction: make_backend must not panic and must return a
        // backend whose chat would hit the right endpoint (covered by the mock
        // server tests below for the Anthropic side).
        let _ = make_backend("https://api.anthropic.com", None, sink.clone(), 1000);
        let _ = make_backend("https://openrouter.ai/api/v1", None, sink.clone(), 1000);
        let _ = make_backend("http://localhost:11434/v1", None, sink, 1000);
    }

    #[test]
    fn anthropic_client_messages_url() {
        let sink: Arc<dyn EventSink> = Arc::new(NullSink);
        let c = AnthropicClient::new("https://api.anthropic.com/", None, sink);
        assert_eq!(c.messages_url(), "https://api.anthropic.com/messages");
    }

    #[test]
    fn system_blocks_single_stable_block_gets_breakpoint() {
        let blocks = anthropic_system_blocks("You are helpful.");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "You are helpful.");
        assert_eq!(blocks[0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn system_blocks_split_stable_and_volatile() {
        let prompt = format!(
            "stable role prompt{}volatile: 2026-10-17",
            zen_core::CACHE_VOLATILE_MARKER
        );
        let blocks = anthropic_system_blocks(&prompt);
        assert_eq!(blocks.len(), 2);
        // Breakpoint on the stable block only — the volatile tail must not be cached.
        assert_eq!(blocks[0]["cache_control"]["type"], "ephemeral");
        assert_eq!(blocks[0]["text"], "stable role prompt");
        assert!(blocks[1].get("cache_control").is_none());
        assert_eq!(blocks[1]["text"], "volatile: 2026-10-17");
    }

    #[test]
    fn request_body_uses_system_blocks_with_breakpoint() {
        let body = anthropic_request_body(&req());
        assert_eq!(body["model"], "claude-sonnet-4-5");
        assert_eq!(body["max_tokens"], 500);
        let system = body["system"].as_array().unwrap();
        assert_eq!(system.len(), 1);
        assert_eq!(system[0]["text"], "You are helpful.");
        assert_eq!(system[0]["cache_control"]["type"], "ephemeral");
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
        // No tools → no tools key.
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn request_body_last_tool_gets_cache_breakpoint() {
        let mut r = req();
        r.tools = vec![
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "Read".to_string(),
                    description: "Read a file".to_string(),
                    parameters: serde_json::json!({"type": "object"}),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "Bash".to_string(),
                    description: "Run a command".to_string(),
                    parameters: serde_json::json!({"type": "object"}),
                },
            },
        ];
        let body = anthropic_request_body(&r);
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        // Only the LAST tool carries the breakpoint: one breakpoint per block,
        // placed after the latest stable content.
        assert!(tools[0].get("cache_control").is_none());
        assert_eq!(tools[1]["cache_control"]["type"], "ephemeral");
        assert_eq!(tools[1]["name"], "Bash");
    }

    #[test]
    fn request_body_is_deterministic_across_builds() {
        // Anti-pattern guard: the cacheable prefix must be byte-identical across
        // turns, or provider caches silently miss. serde_json without
        // `preserve_order` sorts keys, so two builds of the same request must
        // serialize identically.
        let mut r = req();
        r.messages.push(ChatMessage::system(format!(
            "role{}env",
            zen_core::CACHE_VOLATILE_MARKER
        )));
        r.tools = vec![ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: "Read".to_string(),
                description: "Read a file".to_string(),
                parameters: serde_json::json!({"type": "object", "properties": {"p": {"type": "string"}}}),
            },
        }];
        let a = serde_json::to_string(&anthropic_request_body(&r)).unwrap();
        let b = serde_json::to_string(&anthropic_request_body(&r)).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn request_body_translates_tool_calls_and_results() {
        let mut r = req();
        r.messages.push(ChatMessage::assistant(
            Some("let me check".to_string()),
            Some(vec![ToolCall {
                id: "toolu_1".to_string(),
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: "Read".to_string(),
                    arguments: r#"{"path":"a.rs"}"#.to_string(),
                },
            }]),
        ));
        r.messages.push(ChatMessage::tool("toolu_1", "file contents"));
        r.tools = vec![ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: "Read".to_string(),
                description: "Read a file".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            },
        }];
        let body = anthropic_request_body(&r);
        let msgs = body["messages"].as_array().unwrap();
        // system hoisted, so: user, assistant, user(tool_result)
        assert_eq!(msgs.len(), 3);
        let blocks = msgs[1]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[1]["type"], "tool_use");
        assert_eq!(blocks[1]["id"], "toolu_1");
        assert_eq!(blocks[1]["name"], "Read");
        assert_eq!(blocks[1]["input"]["path"], "a.rs");
        let result_blocks = msgs[2]["content"].as_array().unwrap();
        assert_eq!(result_blocks[0]["type"], "tool_result");
        assert_eq!(result_blocks[0]["tool_use_id"], "toolu_1");
        // Tools use Anthropic's input_schema key.
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools[0]["name"], "Read");
        assert!(tools[0].get("input_schema").is_some());
    }

    #[test]
    fn parse_response_text_and_tool_use() {
        let body = r#"{
            "id": "msg_1", "type": "message", "role": "assistant",
            "content": [
                {"type": "text", "text": "checking "},
                {"type": "tool_use", "id": "toolu_9", "name": "Bash", "input": {"command": "ls"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 120, "output_tokens": 30}
        }"#;
        let resp = parse_anthropic_response(body).unwrap();
        assert_eq!(resp.message.content.as_deref(), Some("checking "));
        let calls = resp.message.tool_calls.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "toolu_9");
        assert_eq!(calls[0].function.name, "Bash");
        assert!(calls[0].function.arguments.contains("ls"));
        assert_eq!(resp.usage.prompt_tokens, 120);
        assert_eq!(resp.usage.completion_tokens, 30);
        assert_eq!(resp.usage.cache_read_input_tokens, 0);
        assert_eq!(resp.usage.cache_creation_input_tokens, 0);
        assert_eq!(resp.finish_reason.as_deref(), Some("tool_use"));
    }

    #[test]
    fn parse_response_reads_cache_tokens() {
        let body = r#"{
            "content": [{"type": "text", "text": "done"}],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 10000,
                "output_tokens": 5,
                "cache_read_input_tokens": 9500,
                "cache_creation_input_tokens": 500
            }
        }"#;
        let resp = parse_anthropic_response(body).unwrap();
        assert_eq!(resp.usage.prompt_tokens, 10000);
        assert_eq!(resp.usage.cache_read_input_tokens, 9500);
        assert_eq!(resp.usage.cache_creation_input_tokens, 500);
        // 95% of the input came from cache.
        assert!((zen_core::cache_hit_rate(resp.usage.cache_read_input_tokens, resp.usage.prompt_tokens) - 0.95).abs() < 1e-9);
    }

    #[test]
    fn parse_response_text_only() {
        let body = r#"{
            "content": [{"type": "text", "text": "done"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        }"#;
        let resp = parse_anthropic_response(body).unwrap();
        assert_eq!(resp.message.content.as_deref(), Some("done"));
        assert!(resp.message.tool_calls.is_none());
    }

    #[test]
    fn parse_response_error_object_fails() {
        let body = r#"{"type": "error", "error": {"type": "authentication_error", "message": "bad key"}}"#;
        assert!(parse_anthropic_response(body).is_err());
    }

    #[test]
    fn parse_response_malformed_json_fails() {
        assert!(parse_anthropic_response("not json").is_err());
    }

    /// Mock-server test: the request the client sends must carry `cache_control`
    /// breakpoints and the prompt-caching beta header.
    #[tokio::test]
    async fn mock_server_receives_cache_control_and_beta_header() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let resp_body = r#"{"content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":100,"output_tokens":5,"cache_read_input_tokens":90,"cache_creation_input_tokens":10}}"#;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 65536];
            let mut got = Vec::new();
            loop {
                let n = sock.read(&mut buf).await.unwrap();
                got.extend_from_slice(&buf[..n]);
                let txt = String::from_utf8_lossy(&got);
                if let Some(h) = txt.find("\r\n\r\n") {
                    let len = txt[..h]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if got.len() >= h + 4 + len {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let req_text = String::from_utf8_lossy(&got).to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                resp_body.len(),
                resp_body
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
            req_text
        });

        let sink: Arc<dyn EventSink> = Arc::new(crate::testutil::CollectSink::default());
        let mut c = AnthropicClient::new(format!("http://{addr}"), None, sink);
        c.base_retry_delay_ms = 1;
        let r = c
            .chat(ChatRequest {
                model: "m".into(),
                messages: vec![ChatMessage::system("stable"), ChatMessage::user("hi")],
                tools: vec![ToolDefinition {
                    tool_type: "function".to_string(),
                    function: FunctionDefinition {
                        name: "Read".to_string(),
                        description: "Read a file".to_string(),
                        parameters: serde_json::json!({"type": "object"}),
                    },
                }],
                max_tokens: None,
            })
            .await
            .unwrap();
        assert_eq!(r.message.content.as_deref(), Some("ok"));
        assert_eq!(r.usage.cache_read_input_tokens, 90);
        assert_eq!(r.usage.cache_creation_input_tokens, 10);

        let req_text = server.await.unwrap();
        // Beta header present.
        assert!(req_text.contains("prompt-caching-2024-07-31"), "missing beta header");
        // Request targets /messages.
        assert!(req_text.contains("POST /messages"));
        // cache_control breakpoints present in the JSON body: one on the system
        // block, one on the (last) tool definition.
        let body_start = req_text.find("\r\n\r\n").unwrap() + 4;
        let body: Value = serde_json::from_str(&req_text[body_start..]).unwrap();
        let system = body["system"].as_array().unwrap();
        assert_eq!(system[0]["cache_control"]["type"], "ephemeral");
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools[0]["cache_control"]["type"], "ephemeral");
    }
}
