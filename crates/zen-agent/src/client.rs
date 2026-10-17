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
//! - **Retry policy lives here, not in the loop.** HTTP 429, any 5xx and network errors are
//!   retried with exponential backoff (`base_retry_delay_ms * 2^(attempt-1)`, each wait capped at
//!   60 s), raised to the server's `Retry-After` / `retry_after_seconds` hint (r7). Other 5xx
//!   and network errors stop after 4 attempts; 429/502/503/504 keep retrying until the total wait
//!   budget `max_total_wait_ms` (settings `retry.maxWaitSecs`, default 300 s) is spent (perf p0).
//!   Each retry emits [`AgentEvent::ApiRetry`] so stream-json users can see the retry, matching
//!   SPEC §3.6. A 200 carrying an `error` object, no choices or an empty message is resampled the
//!   same way (r7 C3). Everything else (4xx, malformed JSON) fails immediately.
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
use std::sync::Arc;
use std::time::{Duration, Instant};
use zen_core::*;

/// Number of total attempts made for one `chat` call (1 try + 3 retries).
const MAX_ATTEMPTS: u32 = 4;
/// How much of an error body is quoted back in an error message.
const ERROR_BODY_CHARS: usize = 300;
/// Hard ceiling on attempts for rate limits / gateway errors, which otherwise retry until
/// [`OpenAiClient::max_total_wait_ms`] is spent (perf p0).
const MAX_LONG_ATTEMPTS: u32 = 30;
/// Default total backoff budget for one `chat` call (settings `retry.maxWaitSecs`).
pub const DEFAULT_MAX_TOTAL_WAIT_MS: u64 = 300_000;

/// Statuses that are worth waiting out (rate limit, bad gateway, unavailable, gateway timeout):
/// they keep retrying until the total wait budget is spent. Other 5xx keep [`MAX_ATTEMPTS`].
fn is_long_retry_status(code: u16) -> bool {
    matches!(code, 429 | 502 | 503 | 504)
}

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

impl OpenAiClient {
    /// Build a client. `api_key` may be `None` (local gateways often need no auth); an empty
    /// string is treated as absent. `sink` receives retry events.
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
            api_key,
            sink,
            base_retry_delay_ms: 1000,
            max_retry_delay_ms: 60_000,
            max_total_wait_ms: DEFAULT_MAX_TOTAL_WAIT_MS,
            http,
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

/// Exponential backoff, raised to the server's hint (capped at `cap_ms`).
fn retry_delay_ms(attempt: u32, base_ms: u64, hint_ms: Option<u64>, cap_ms: u64) -> u64 {
    let backoff = base_ms.saturating_mul(1u64 << (attempt - 1));
    backoff.max(hint_ms.unwrap_or(0).min(cap_ms))
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

        loop {
            attempt += 1;
            let rb = self.auth(self.http.post(&url).json(&req));
            let outcome = rb.send().await;

            // Classify the attempt: either a parsed response, a retryable failure, or a
            // fatal failure. `err_text` is what the caller would see in an error message.
            // `long` failures (429/502/503/504) retry until the wait budget is spent.
            let mut server_hint_ms: Option<u64> = None;
            let (retryable, long, status, err_text): (bool, bool, Option<u16>, String) = match outcome {
                Ok(resp) => {
                    let code = resp.status().as_u16();
                    if resp.status().is_success() {
                        let body = resp.text().await?;
                        // A 200 that carries a gateway error, no choices, or an empty message is
                        // a sampling glitch (gpt-oss Harmony header errors): resample (r7 C3).
                        match transient_completion(&body) {
                            Some(why) if attempt < MAX_ATTEMPTS => (true, false, Some(code), why),
                            _ => return parse_completion(&body),
                        }
                    } else {
                        let header = resp
                            .headers()
                            .get(reqwest::header::RETRY_AFTER)
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string);
                        let body = resp.text().await.unwrap_or_default();
                        server_hint_ms = server_retry_hint_ms(header.as_deref(), &body);
                        let retryable = code == 429 || (500..600).contains(&code);
                        (
                            retryable,
                            is_long_retry_status(code),
                            Some(code),
                            format!("HTTP {}: {}", code, first_chars(&body, ERROR_BODY_CHARS)),
                        )
                    }
                }
                Err(e) => (true, false, None, format!("network error: {}", e)),
            };

            if !retryable {
                return Err(anyhow::anyhow!(err_text));
            }
            let attempt_cap = if long { MAX_LONG_ATTEMPTS } else { MAX_ATTEMPTS };
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
            // Backoff doubles per attempt (exponent capped so it cannot overflow), never above
            // the per-wait cap, raised to the server's hint, and clipped to the budget left.
            let delay_ms = retry_delay_ms(
                attempt.min(16),
                self.base_retry_delay_ms,
                server_hint_ms,
                self.max_retry_delay_ms,
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
    Ok(ChatResponse {
        message: ChatMessage::assistant(content, tool_calls),
        usage: parsed.usage,
        finish_reason: choice.finish_reason,
    })
}

/// One `chat` call with a model-fallback chain: primary model first, then each fallback once.
///
/// The primary's own retries ([`OpenAiClient::chat`]) happen inside each attempt, so a fallback
/// only runs after the primary has truly failed. Returns the response, the total wall time spent
/// in `chat` (all attempts and models, used for `duration_api_ms`), and the model that answered.
pub(crate) async fn chat_with_fallback(
    llm: &Arc<dyn LlmBackend>,
    primary: &str,
    fallbacks: &[String],
    messages: Vec<ChatMessage>,
    tools: Vec<ToolDefinition>,
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
            max_tokens: None,
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
        // attempt 1 backoff 1 s; a 23 s hint wins; a 600 s hint is capped at 60 s.
        assert_eq!(retry_delay_ms(1, 1000, None, 60_000), 1000);
        assert_eq!(retry_delay_ms(3, 1000, Some(500), 60_000), 4000);
        assert_eq!(retry_delay_ms(1, 1000, Some(23_000), 60_000), 23_000);
        assert_eq!(retry_delay_ms(1, 1000, Some(600_000), 60_000), 60_000);
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
