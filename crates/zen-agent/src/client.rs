


































use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zen_core::*;
use crate::safety::{self, ErrorClass, RateLimiter};


const MAX_ATTEMPTS: u32 = 6;


pub const MID_RESPONSE_MAX_RETRIES: u32 = 3;

const ERROR_BODY_CHARS: usize = 300;

pub const DEFAULT_MAX_TOTAL_WAIT_MS: u64 = 300_000;

pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 300;


pub const LOCAL_REQUEST_TIMEOUT_SECS: u64 = 900;

pub const CONNECT_TIMEOUT_SECS: u64 = 10;






#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpTimeouts {

    pub request: Option<Duration>,



    pub idle: Option<Duration>,

    pub connect: Duration,
}

impl HttpTimeouts {

    pub fn for_url(base_url: &str) -> Self {
        Self::from_secs(base_url, None, None)
    }


    pub fn from_secs(base_url: &str, request_secs: Option<u64>, idle_secs: Option<u64>) -> Self {
        let default = if is_local_url(base_url) || base_url.to_ascii_lowercase().contains("ollama") {
            LOCAL_REQUEST_TIMEOUT_SECS
        } else {
            DEFAULT_REQUEST_TIMEOUT_SECS
        };
        let secs = |v: u64| (v > 0).then(|| Duration::from_secs(v));
        Self {
            request: secs(request_secs.unwrap_or(default)),
            idle: idle_secs.and_then(secs),
            connect: Duration::from_secs(CONNECT_TIMEOUT_SECS),
        }
    }

    fn apply(&self, mut b: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
        b = b.connect_timeout(self.connect);
        if let Some(t) = self.request {
            b = b.timeout(t);
        }
        if let Some(t) = self.idle {
            b = b.read_timeout(t);
        }
        b
    }


    fn describe(&self, elapsed: Duration) -> String {
        match (self.request, self.idle) {
            (Some(r), _) if elapsed + Duration::from_millis(50) >= r => {
                format!("request timed out after {} (settings api.timeoutSecs)", fmt_secs(r))
            }
            (_, Some(i)) => format!("no data from the server for {} (settings api.idleTimeoutSecs)", fmt_secs(i)),
            _ => format!("request timed out after {}", fmt_secs(elapsed)),
        }
    }
}

fn fmt_secs(d: Duration) -> String {
    if d.as_millis().is_multiple_of(1000) {
        format!("{} s", d.as_secs())
    } else {
        format!("{:.1} s", d.as_secs_f64())
    }
}



const REQUEST_ID_HEADERS: [&str; 5] =
    ["x-request-id", "request-id", "x-amzn-requestid", "x-amz-request-id", "x-correlation-id"];


const REQUEST_ID_MARK: &str = "[request id: ";


fn clean_request_id(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()
        && s.len() <= 128
        && s.chars().all(|c| c.is_ascii_graphic() && c != '[' && c != ']'))
    .then(|| s.to_string())
}



pub(crate) fn request_id(headers: &reqwest::header::HeaderMap, body: &str) -> Option<String> {
    for h in REQUEST_ID_HEADERS {
        if let Some(v) = headers.get(h).and_then(|v| v.to_str().ok()).and_then(clean_request_id) {
            return Some(v);
        }
    }
    let v: Value = serde_json::from_str(body).ok()?;
    ["/request_id", "/error/request_id", "/id"]
        .iter()
        .find_map(|p| v.pointer(p).and_then(|x| x.as_str()).and_then(clean_request_id))
}


fn with_request_id(msg: String, id: Option<&str>) -> String {
    match id {
        Some(id) => format!("{} {}{}]", msg, REQUEST_ID_MARK, id),
        None => msg,
    }
}



pub fn request_id_from_error(msg: &str) -> Option<String> {
    let start = msg.rfind(REQUEST_ID_MARK)? + REQUEST_ID_MARK.len();
    let end = msg[start..].find(']')? + start;
    clean_request_id(&msg[start..end])
}


pub(crate) fn build_http(base_url: &str, timeouts: &HttpTimeouts, user_agent: bool) -> reqwest::Client {
    let mut builder = timeouts.apply(with_extra_roots(reqwest::Client::builder()));
    if user_agent {
        builder = builder.user_agent(safety::user_agent());
    }
    if is_local_url(base_url) {
        builder = builder.no_proxy();
    }
    builder.build().unwrap_or_else(|_| reqwest::Client::new())
}






pub struct OpenAiClient {


    pub base_url: String,
    api_key: Option<String>,


    pub sink: Arc<dyn EventSink>,

    pub base_retry_delay_ms: u64,


    pub max_retry_delay_ms: u64,



    pub max_total_wait_ms: u64,

    timeouts: HttpTimeouts,
    http: reqwest::Client,


    pacer: Arc<Mutex<Option<RateLimiter>>>,

    recent_429s: Arc<Mutex<Vec<Instant>>>,

    provider_name: String,
}



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


fn is_local_url(url: &str) -> bool {
    let b = url.to_ascii_lowercase();
    b.contains("://127.0.0.1") || b.contains("://localhost") || b.contains("://[::1]")
}


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


    pub fn new(base_url: impl Into<String>, api_key: Option<String>, sink: Arc<dyn EventSink>) -> Self {
        let base_url = base_url.into();
        let timeouts = HttpTimeouts::for_url(&base_url);
        let http = build_http(&base_url, &timeouts, true);


        let pacer = RateLimiter::for_provider(&base_url);
        let provider_name = provider_name_for_url(&base_url);
        Self {
            base_url,
            api_key,
            sink,
            base_retry_delay_ms: 1000,
            max_retry_delay_ms: 60_000,
            max_total_wait_ms: DEFAULT_MAX_TOTAL_WAIT_MS,
            timeouts,
            http,
            pacer: Arc::new(Mutex::new(pacer)),
            recent_429s: Arc::new(Mutex::new(Vec::new())),
            provider_name,
        }
    }


    pub fn set_timeouts(&mut self, timeouts: HttpTimeouts) {
        self.http = build_http(&self.base_url, &timeouts, true);
        self.timeouts = timeouts;
    }


    pub fn timeouts(&self) -> HttpTimeouts {
        self.timeouts
    }


    pub fn with_provider_name(mut self, name: impl Into<String>) -> Self {
        self.provider_name = name.into();
        self
    }


    pub fn recent_429_count(&self) -> usize {
        let mut v = self.recent_429s.lock().unwrap();
        let cutoff = Instant::now() - Duration::from_secs(5 * 60);
        v.retain(|t| *t >= cutoff);
        v.len()
    }

    fn note_429(&self) {
        self.recent_429s.lock().unwrap().push(Instant::now());
    }



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


    #[allow(dead_code)]
    fn key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }


    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }


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
    refusal: Option<String>,
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

    #[serde(default)]
    arguments: Option<Value>,
}





pub(crate) fn normalize_arguments(v: Option<Value>) -> String {
    match v {
        None | Some(Value::Null) => "{}".to_string(),
        Some(Value::String(s)) if s.trim().is_empty() => "{}".to_string(),
        Some(Value::String(s)) => s,
        Some(other) => other.to_string(),
    }
}




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



fn ratelimit_reset_hint_ms(header: Option<&str>, now_ms: u64) -> Option<u64> {
    let v = header?.trim().parse::<f64>().ok()?;
    if !v.is_finite() || v < 0.0 {
        return None;
    }
    let v = v as u64;
    Some(if v >= 1_000_000_000_000 {
        v.saturating_sub(now_ms)
    } else if v >= 1_000_000_000 {
        (v * 1000).saturating_sub(now_ms)
    } else {
        v * 1000
    })
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}






#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MidResponseError {

    pub code: Option<u16>,

    pub error_type: Option<String>,
    pub message: String,
}

impl MidResponseError {


    pub fn class(&self) -> ErrorClass {
        let ty = self.error_type.as_deref().unwrap_or("");
        match (self.code, ty) {
            (Some(429), _) | (_, "rate_limit_exceeded") => ErrorClass::RateLimit,
            (Some(402), _) | (_, "payment_required") => ErrorClass::QuotaExhausted,
            (_, "content_policy_violation" | "refusal") => ErrorClass::Fatal,
            (Some(c), _) if (400..500).contains(&c) && c != 408 => ErrorClass::Fatal,
            _ => ErrorClass::Transient,
        }
    }



    pub fn describe(&self) -> String {
        let code = self.code.map(|c| format!(" HTTP {c}")).unwrap_or_default();
        let ty = self.error_type.as_deref().map(|t| format!(" ({t})")).unwrap_or_default();
        let policy = matches!(self.error_type.as_deref(), Some("content_policy_violation" | "refusal"));
        let lead = if policy {
            "blocked by the provider's content filter mid-response:"
        } else {
            "provider error mid-response:"
        };
        format!("{lead}{code}{ty}: {}", first_chars(&self.message, ERROR_BODY_CHARS))
    }
}


pub(crate) fn mid_response_error(body: &str) -> Option<MidResponseError> {
    let v: Value = serde_json::from_str(body).ok()?;
    let choice = v.get("choices").and_then(Value::as_array).and_then(|c| c.first());
    let finish_error = choice
        .and_then(|c| c.get("finish_reason"))
        .and_then(Value::as_str)
        .is_some_and(|r| r.eq_ignore_ascii_case("error"));
    let err = choice
        .and_then(|c| c.get("error"))
        .filter(|e| e.is_object())
        .or_else(|| v.get("error").filter(|e| e.is_object()));
    let typed = err.and_then(|e| e.pointer("/metadata/error_type")).and_then(Value::as_str);
    if !finish_error && typed.is_none() {
        return None;
    }
    let code = err
        .and_then(|e| e.get("code"))
        .and_then(|c| c.as_u64().or_else(|| c.as_str().and_then(|s| s.parse().ok())))
        .and_then(|c| u16::try_from(c).ok());
    let message = err
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("the provider ended the response with finish_reason \"error\"")
        .to_string();
    Some(MidResponseError { code, error_type: typed.map(str::to_string), message })
}



pub fn is_content_filtered(finish_reason: Option<&str>) -> bool {
    finish_reason.is_some_and(|r| {
        let r = r.to_ascii_lowercase();
        r == "content_filter" || r == "refusal"
    })
}



pub(crate) fn payment_required_reason(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    let meta = v.pointer("/error/metadata")?;
    let get = |k: &str| meta.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
    let what = match (get("reason"), get("limit_source")) {
        (Some("weight_exceeds_budget"), _) => {
            "this single request's estimated cost is larger than the whole credit budget (weight_exceeds_budget); retrying will not help, lower max_tokens or the prompt size"
        }
        (_, Some("openrouter_key_limit")) => "the API key's credit limit is spent (openrouter_key_limit)",
        (_, Some("openrouter_credits")) => "the account balance cannot cover this request (openrouter_credits)",
        (_, Some("openrouter_in_flight_budget")) => {
            "running requests fill the in-flight spending budget (openrouter_in_flight_budget)"
        }
        _ => return get("remedy_hint").map(str::to_string),
    };
    Some(match get("remedy_hint") {
        Some(h) => format!("{what}. {h}"),
        None => what.to_string(),
    })
}


fn is_in_flight_budget_402(code: u16, has_retry_after: bool, body: &str) -> bool {
    code == 402 && has_retry_after && body.contains("openrouter_in_flight_budget")
}


fn first_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[async_trait]
impl LlmBackend for OpenAiClient {
    async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse> {
        let url = self.endpoint();
        let mut waited_ms: u64 = 0;
        let mut attempt: u32 = 0;

        let mut warned_first_429 = false;

        let mut mid_response_errors: u32 = 0;

        loop {
            attempt += 1;


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
            let started = Instant::now();
            let outcome = rb.send().await;

            let mut timed_out_ms: u64 = 0;
            let mut timeout_text = |e: &reqwest::Error| -> String {
                let elapsed = started.elapsed();
                if e.is_timeout() {
                    timed_out_ms = elapsed.as_millis() as u64;
                    self.timeouts.describe(elapsed)
                } else {
                    format!("network error: {}", e)
                }
            };


            let mut mid_transient = false;


            let mut server_hint_ms: Option<u64> = None;
            enum Outcome {
                Parsed(anyhow::Result<ChatResponse>),
                Classified(ErrorClass, Option<u16>, String),
            }
            let outcome: Outcome = match outcome {
                Ok(resp) => {
                    let code = resp.status().as_u16();

                    self.update_pacer_from_headers(resp.headers());
                    let headers = resp.headers().clone();
                    if resp.status().is_success() {
                        match resp.text().await {

                            Err(e) => Outcome::Classified(ErrorClass::Transient, Some(code), timeout_text(&e)),
                            Ok(body) => {


                                if let Some(mid) = mid_response_error(&body) {
                                    let class = mid.class();
                                    if class == ErrorClass::RateLimit {
                                        self.note_429();
                                    }
                                    if class == ErrorClass::Transient {
                                        mid_response_errors += 1;
                                        mid_transient = true;
                                    }
                                    Outcome::Classified(
                                        class,
                                        mid.code.or(Some(code)),
                                        with_request_id(mid.describe(), request_id(&headers, &body).as_deref()),
                                    )
                                } else {



                                match transient_completion(&body) {
                                    Some(why) if attempt < MAX_ATTEMPTS => Outcome::Classified(
                                        ErrorClass::Transient,
                                        Some(code),
                                        with_request_id(why, request_id(&headers, &body).as_deref()),
                                    ),
                                    _ => Outcome::Parsed(parse_completion(&body)),
                                }
                                }
                            }
                        }
                    } else {
                        let header = headers
                            .get(reqwest::header::RETRY_AFTER)
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string);
                        let reset = headers
                            .get("x-ratelimit-reset")
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string);
                        let body = resp.text().await.unwrap_or_default();
                        server_hint_ms = server_retry_hint_ms(header.as_deref(), &body);
                        if code == 429 && server_hint_ms.is_none() {

                            server_hint_ms = ratelimit_reset_hint_ms(reset.as_deref(), now_unix_ms());
                        }
                        let mut class = safety::classify_error(Some(code), &body);


                        if is_in_flight_budget_402(code, header.is_some(), &body) {
                            class = ErrorClass::RateLimit;
                        }
                        if code == 429 {
                            self.note_429();
                        }
                        let mut text = format!("HTTP {}: {}", code, first_chars(&body, ERROR_BODY_CHARS));
                        if code == 402 {
                            if let Some(why) = payment_required_reason(&body) {
                                text = format!("HTTP 402: {why}");
                            }
                        }
                        Outcome::Classified(
                            class,
                            Some(code),
                            with_request_id(text, request_id(&headers, &body).as_deref()),
                        )
                    }
                }
                Err(e) => Outcome::Classified(ErrorClass::Transient, None, timeout_text(&e)),
            };
            waited_ms = waited_ms.saturating_add(timed_out_ms);

            let (class, status, err_text) = match outcome {
                Outcome::Parsed(r) => return r,
                Outcome::Classified(c, st, e) => (c, st, e),
            };


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


            if attempt >= MAX_ATTEMPTS {
                return Err(anyhow::anyhow!(err_text));
            }
            if mid_response_errors > MID_RESPONSE_MAX_RETRIES {
                return Err(anyhow::anyhow!(
                    "{} (gave up: the provider failed mid-response {} times in a row)",
                    err_text,
                    mid_response_errors
                ));
            }
            let budget_left = self.max_total_wait_ms.saturating_sub(waited_ms);


            if let Some(hint) = server_hint_ms.filter(|h| *h > budget_left) {
                return Err(anyhow::anyhow!(
                    "{} (not retried: the server asked to wait {} s, more than the {} s left of the retry budget; settings retry.maxWaitSecs)",
                    err_text,
                    hint.div_ceil(1000),
                    budget_left / 1000
                ));
            }
            if budget_left == 0 {
                return Err(anyhow::anyhow!(
                    "{} (gave up after {} attempts: retry wait budget of {} s spent; settings retry.maxWaitSecs)",
                    err_text,
                    attempt,
                    self.max_total_wait_ms / 1000
                ));
            }


            if status == Some(429) && !warned_first_429 {
                warned_first_429 = true;
                eprintln!("{}", safety::first_429_notice(&self.provider_name));
            }

            let recent = self.recent_429_count();
            if recent > 3 {
                eprintln!("{}", safety::repeated_429_warning(recent as u32, &self.provider_name));
            }



            let jittered = safety::backoff_with_jitter(
                attempt,
                self.base_retry_delay_ms,
                self.max_retry_delay_ms,
                safety::jitter_now_nanos(),
            );


            let delay_ms = jittered
                .min(self.max_retry_delay_ms.max(self.base_retry_delay_ms))
                .max(server_hint_ms.unwrap_or(0))
                .min(budget_left);

            let mut err_for_event = err_text.clone();
            if class == ErrorClass::Overloaded {
                err_for_event.push_str(" (provider overloaded - consider a fallback provider)");
            }
            self.sink.emit(AgentEvent::ApiRetry {
                attempt,
                max_retries: if mid_transient { MID_RESPONSE_MAX_RETRIES } else { MAX_ATTEMPTS - 1 },
                retry_delay_ms: delay_ms,
                error_status: status,
                error: err_for_event,
            });
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            waited_ms = waited_ms.saturating_add(delay_ms);
        }
    }

}






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

    if is_content_filtered(first.get("finish_reason").and_then(Value::as_str)) {
        return None;
    }
    let msg = first.get("message")?;
    let has_text = msg.get("content").and_then(Value::as_str).is_some_and(|t| !t.trim().is_empty());
    let has_calls = msg.get("tool_calls").and_then(Value::as_array).is_some_and(|c| !c.is_empty());
    (!has_text && !has_calls).then(|| "model returned an empty message".into())
}





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


        Some(s) if s.is_empty() && tool_calls.is_some() => None,
        other => other,
    };

    let content = match (content, choice.message.refusal) {
        (c, Some(r)) if c.as_deref().is_none_or(|t| t.trim().is_empty()) && !r.trim().is_empty() => Some(r),
        (c, _) => c,
    };
    let mut usage = parsed.usage;



    usage.cache_read_input_tokens = openai_cached_tokens(body);
    Ok(ChatResponse {
        message: ChatMessage::assistant(content, tool_calls),
        usage,
        finish_reason: choice.finish_reason,
    })
}




pub(crate) fn openai_cached_tokens(body: &str) -> u64 {
    let v: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return 0,
    };
    v.pointer("/usage/prompt_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0)
}



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


















const ANTHROPIC_VERSION: &str = "2023-06-01";

const ANTHROPIC_CACHE_BETA: &str = "prompt-caching-2024-07-31";

const ANTHROPIC_DEFAULT_MAX_TOKENS: u32 = 4096;


pub fn is_anthropic_url(base_url: &str) -> bool {
    let u = base_url.trim().to_ascii_lowercase();
    u.contains("anthropic.com") || u.contains("api.anthropic")
}


fn cache_control() -> Value {
    serde_json::json!({"type": "ephemeral"})
}






pub struct AnthropicClient {


    pub base_url: String,
    api_key: Option<String>,

    pub sink: Arc<dyn EventSink>,

    pub base_retry_delay_ms: u64,

    pub max_retry_delay_ms: u64,

    pub max_total_wait_ms: u64,
    timeouts: HttpTimeouts,
    http: reqwest::Client,
}

impl AnthropicClient {

    pub fn new(base_url: impl Into<String>, api_key: Option<String>, sink: Arc<dyn EventSink>) -> Self {
        let base_url = base_url.into();
        let timeouts = HttpTimeouts::for_url(&base_url);
        let http = build_http(&base_url, &timeouts, false);
        Self {
            base_url,
            api_key: api_key.filter(|k| !k.is_empty()),
            sink,
            base_retry_delay_ms: 1000,
            max_retry_delay_ms: 60_000,
            max_total_wait_ms: DEFAULT_MAX_TOTAL_WAIT_MS,
            timeouts,
            http,
        }
    }


    pub fn set_timeouts(&mut self, timeouts: HttpTimeouts) {
        self.http = build_http(&self.base_url, &timeouts, false);
        self.timeouts = timeouts;
    }


    pub fn timeouts(&self) -> HttpTimeouts {
        self.timeouts
    }

    fn messages_url(&self) -> String {
        format!("{}/messages", self.base_url.trim_end_matches('/'))
    }
}







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

                messages.push(serde_json::json!({
                    "role": "user",
                    "content": m.content.as_deref().unwrap_or(""),
                }));
            }
        }
    }



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
            let started = Instant::now();
            let res = rb.send().await;
            let mut timed_out_ms: u64 = 0;
            let mut timeout_text = |e: &reqwest::Error| -> String {
                let elapsed = started.elapsed();
                if e.is_timeout() {
                    timed_out_ms = elapsed.as_millis() as u64;
                    self.timeouts.describe(elapsed)
                } else {
                    format!("network error: {e}")
                }
            };
            let (retryable, long, status, err_text): (bool, bool, Option<u16>, String) = match res {
                Ok(resp) => 'resp: {
                    let code = resp.status().as_u16();
                    let headers = resp.headers().clone();
                    let resp_body = match resp.text().await {
                        Ok(b) => b,
                        Err(e) => break 'resp (true, false, Some(code), timeout_text(&e)),
                    };
                    let rid = request_id(&headers, &resp_body);
                    if resp_body.contains("\"type\":\"error\"") || resp_body.contains("\"type\": \"error\"") {

                        let msg = parse_anthropic_response(&resp_body)
                            .err()
                            .map(|e| e.to_string())
                            .unwrap_or_else(|| first_chars(&resp_body, ERROR_BODY_CHARS));
                        let retryable = matches!(code, 429 | 500 | 502 | 503 | 504 | 529);
                        (
                            retryable,
                            retryable,
                            Some(code),
                            with_request_id(format!("HTTP {}: {}", code, msg), rid.as_deref()),
                        )
                    } else if (200..300).contains(&code) {
                        match parse_anthropic_response(&resp_body) {
                            Ok(parsed) => return Ok(parsed),
                            Err(e) => (
                                false,
                                false,
                                Some(code),
                                with_request_id(format!("bad anthropic body: {e}"), rid.as_deref()),
                            ),
                        }
                    } else {
                        let retryable = matches!(code, 429 | 500 | 502 | 503 | 504 | 529);
                        (
                            retryable,
                            retryable,
                            Some(code),
                            with_request_id(
                                format!("HTTP {}: {}", code, first_chars(&resp_body, ERROR_BODY_CHARS)),
                                rid.as_deref(),
                            ),
                        )
                    }
                }
                Err(e) => (true, false, None, timeout_text(&e)),
            };
            waited_ms = waited_ms.saturating_add(timed_out_ms);

            if !retryable {
                return Err(anyhow::anyhow!(err_text));
            }

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




pub fn make_backend(
    base_url: impl Into<String>,
    api_key: Option<String>,
    sink: Arc<dyn EventSink>,
    max_total_wait_ms: u64,
) -> Arc<dyn LlmBackend> {
    let base_url = base_url.into();
    let timeouts = HttpTimeouts::for_url(&base_url);
    make_backend_with(base_url, api_key, sink, max_total_wait_ms, timeouts)
}


pub fn make_backend_with(
    base_url: impl Into<String>,
    api_key: Option<String>,
    sink: Arc<dyn EventSink>,
    max_total_wait_ms: u64,
    timeouts: HttpTimeouts,
) -> Arc<dyn LlmBackend> {
    let base_url = base_url.into();
    if is_anthropic_url(&base_url) {
        let mut c = AnthropicClient::new(base_url, api_key, sink);
        c.max_total_wait_ms = max_total_wait_ms;
        if c.timeouts != timeouts {
            c.set_timeouts(timeouts);
        }
        Arc::new(c)
    } else {
        let mut c = OpenAiClient::new(base_url, api_key, sink);
        c.max_total_wait_ms = max_total_wait_ms;
        if c.timeouts != timeouts {
            c.set_timeouts(timeouts);
        }
        Arc::new(c)
    }
}



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


        assert!(tools[0].get("cache_control").is_none());
        assert_eq!(tools[1]["cache_control"]["type"], "ephemeral");
        assert_eq!(tools[1]["name"], "Bash");
    }

    #[test]
    fn request_body_is_deterministic_across_builds() {




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

        assert!(req_text.contains("prompt-caching-2024-07-31"), "missing beta header");

        assert!(req_text.contains("POST /messages"));


        let body_start = req_text.find("\r\n\r\n").unwrap() + 4;
        let body: Value = serde_json::from_str(&req_text[body_start..]).unwrap();
        let system = body["system"].as_array().unwrap();
        assert_eq!(system[0]["cache_control"]["type"], "ephemeral");
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools[0]["cache_control"]["type"], "ephemeral");
    }
}
