use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use zen_core::*;

use crate::client::{mid_response_error, OpenAiClient};
use crate::rategate::{fetch_key_quota, openrouter_key_url, quota_command_report, KeyQuota};
use crate::safety::ErrorClass;
use crate::testutil::*;

#[derive(Clone)]
struct Answer {
    status: u16,
    headers: String,
    body: String,
}

fn answer(status: u16, headers: &str, body: &str) -> Answer {
    Answer {
        status,
        headers: headers.to_string(),
        body: body.to_string(),
    }
}

async fn fake_server(answers: Vec<Answer>) -> (String, Arc<AtomicUsize>, Arc<Mutex<Vec<String>>>) {
    use tokio::io::AsyncWriteExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (counter, log) = (served.clone(), seen.clone());
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let raw = read_full_request(&mut sock).await;
            log.lock().unwrap().push(raw);
            let i = counter.fetch_add(1, Ordering::SeqCst);
            let a = answers[i.min(answers.len() - 1)].clone();
            let resp = format!(
                "HTTP/1.1 {} X\r\nContent-Type: application/json\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                a.status,
                a.headers,
                a.body.len(),
                a.body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    (format!("http://{addr}/api/v1"), served, seen)
}

const OK_BODY: &str = r#"{"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;

const KEY_BODY: &str = r#"{"data":{"label":"sk-or-v1-abc...xyz","limit":null,"limit_reset":null,"limit_remaining":null,"usage":0.5,"usage_daily":0.0,"is_free_tier":true,"free_model_daily_requests":{"used":12,"limit":50,"remaining":38}}}"#;

fn req() -> ChatRequest {
    ChatRequest {
        model: "m:free".into(),
        messages: vec![ChatMessage::user("hi")],
        tools: vec![],
        max_tokens: None,
    }
}

fn retry_delays(sink: &CollectSink) -> Vec<u64> {
    sink.events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ApiRetry { retry_delay_ms, .. } => Some(*retry_delay_ms),
            _ => None,
        })
        .collect()
}

fn mid_error(code: u16, error_type: &str, msg: &str) -> String {
    serde_json::json!({
        "id": "gen-1",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": ""},
            "finish_reason": "error",
            "error": {"code": code, "message": msg, "metadata": {"error_type": error_type}}
        }]
    })
    .to_string()
}

#[test]
fn p0_4_key_url_only_for_openrouter() {
    assert_eq!(
        openrouter_key_url("https://openrouter.ai/api/v1/").as_deref(),
        Some("https://openrouter.ai/api/v1/key")
    );
    assert_eq!(openrouter_key_url("https://api.openai.com/v1"), None);
    assert_eq!(openrouter_key_url("http://127.0.0.1:11434/v1"), None);
}

#[tokio::test]
async fn p0_4_quota_shows_used_limit_remaining_from_fake_server() {
    let (base, served, seen) = fake_server(vec![answer(200, "", KEY_BODY)]).await;
    let key_url = format!("{base}/key");
    let report = quota_command_report(Some(&key_url), Some("sk-test-123"), None).await;
    assert!(
        report.contains("free-model requests: used 12 / limit 50 (remaining 38)"),
        "{report}"
    );
    assert!(report.contains("key credit limit: none"), "{report}");
    assert!(
        !report.contains("sk-test-123") && !report.contains("sk-or-v1"),
        "never prints the key: {report}"
    );
    assert_eq!(served.load(Ordering::SeqCst), 1);
    let raw = seen.lock().unwrap()[0].clone();
    assert!(raw.starts_with("GET /api/v1/key "), "{raw}");
    assert!(
        raw.to_ascii_lowercase()
            .contains("authorization: bearer sk-test-123"),
        "{raw}"
    );
}

#[tokio::test]
async fn p0_4_quota_lookup_error_is_clear_and_local_counts_still_shown() {
    let (base, served, _) = fake_server(vec![answer(
        401,
        "",
        r#"{"error":{"code":401,"message":"No auth credentials found"}}"#,
    )])
    .await;
    let report = quota_command_report(Some(&format!("{base}/key")), None, None).await;
    assert!(
        report.contains("quota lookup failed: HTTP 401: No auth credentials found"),
        "{report}"
    );
    assert!(report.contains("RateGate"), "{report}");
    assert_eq!(served.load(Ordering::SeqCst), 1, "no retries");
}

#[tokio::test]
async fn p0_4_non_openrouter_quota_makes_no_request() {
    let report = quota_command_report(
        openrouter_key_url("http://127.0.0.1:9/v1").as_deref(),
        None,
        None,
    )
    .await;
    assert!(report.starts_with("RateGate"), "{report}");
}

#[test]
fn p0_4_key_body_parsing_and_exhausted_marker() {
    let q = KeyQuota::from_body(
        r#"{"data":{"limit":5.0,"limit_remaining":1.25,"usage_daily":0.1,"free_model_daily_requests":{"used":50,"limit":50,"remaining":0}}}"#,
    )
    .unwrap();
    assert_eq!(
        (q.free_used, q.free_limit, q.free_remaining),
        (Some(50), Some(50), Some(0))
    );
    let r = q.report();
    assert!(
        r.contains("used 50 / limit 50 (remaining 0) - EXHAUSTED"),
        "{r}"
    );
    assert!(r.contains("key credit limit: 5.00 (remaining 1.25)"), "{r}");
    assert!(KeyQuota::from_body("{}").is_err());
    assert!(KeyQuota::from_body("nope").is_err());
}

#[tokio::test]
async fn p0_4_chat_never_calls_the_key_endpoint() {
    let (base, served, seen) = fake_server(vec![answer(200, "", OK_BODY)]).await;
    let c = OpenAiClient::new(base, None, Arc::new(CollectSink::default()));
    c.chat(req()).await.unwrap();
    assert_eq!(served.load(Ordering::SeqCst), 1);
    assert!(seen.lock().unwrap().iter().all(|r| !r.contains("/key")));

    let _ = fetch_key_quota;
}

#[tokio::test]
async fn p0_4_429_waits_until_x_ratelimit_reset() {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let reset = format!(
        "X-RateLimit-Limit: 20\r\nX-RateLimit-Remaining: 0\r\nX-RateLimit-Reset: {}\r\n",
        now_ms + 1500
    );
    let (base, served, _) = fake_server(vec![
        answer(
            429,
            &reset,
            r#"{"error":{"code":429,"message":"Rate limit exceeded: free-models-per-min"}}"#,
        ),
        answer(200, "", OK_BODY),
    ])
    .await;
    let sink = Arc::new(CollectSink::default());
    let mut c = OpenAiClient::new(base, None, sink.clone());
    c.base_retry_delay_ms = 1;
    let t0 = Instant::now();
    let r = c.chat(req()).await.unwrap();
    assert_eq!(r.message.content.as_deref(), Some("ok"));
    assert_eq!(served.load(Ordering::SeqCst), 2);
    let d = retry_delays(&sink);
    assert_eq!(d.len(), 1);
    assert!(
        (1000..=1600).contains(&d[0]),
        "waited {d:?}, expected ~1.5 s until reset"
    );
    assert!(t0.elapsed() >= Duration::from_millis(1000));
}

#[tokio::test]
async fn p0_4_429_reset_beyond_budget_is_not_retried() {
    let (base, served, _) = fake_server(vec![answer(
        429,
        "Retry-After: 3600\r\n",
        r#"{"error":{"code":429,"message":"Rate limit exceeded: free-models-per-day"}}"#,
    )])
    .await;
    let sink = Arc::new(CollectSink::default());
    let mut c = OpenAiClient::new(base, None, sink.clone());
    c.max_total_wait_ms = 5_000;
    let t0 = Instant::now();
    let err = c.chat(req()).await.unwrap_err().to_string();
    assert!(
        err.contains("HTTP 429") && err.contains("asked to wait 3600 s"),
        "{err}"
    );
    assert_eq!(served.load(Ordering::SeqCst), 1);
    assert!(t0.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn p0_4_402_weight_exceeds_budget_is_never_retried() {
    let body = r#"{"error":{"code":402,"message":"Request too expensive","metadata":{"reason":"weight_exceeds_budget","limit_source":"openrouter_credits","remedy_hint":"Lower max_tokens or add credits."}}}"#;
    let (base, served, _) =
        fake_server(vec![answer(402, "", body), answer(200, "", OK_BODY)]).await;
    let sink = Arc::new(CollectSink::default());
    let mut c = OpenAiClient::new(base, None, sink.clone());
    c.base_retry_delay_ms = 1;
    let err = c.chat(req()).await.unwrap_err().to_string();
    assert_eq!(served.load(Ordering::SeqCst), 1, "402 must not be retried");
    assert!(retry_delays(&sink).is_empty());
    assert!(
        err.contains("weight_exceeds_budget") && err.contains("will NOT retry"),
        "{err}"
    );
}

#[tokio::test]
async fn p0_4_402_in_flight_budget_with_retry_after_is_retried() {
    let body = r#"{"error":{"code":402,"message":"in-flight","metadata":{"reason":"in_flight_budget_exhausted","limit_source":"openrouter_in_flight_budget"}}}"#;
    let (base, served, _) = fake_server(vec![
        answer(402, "Retry-After: 0\r\n", body),
        answer(200, "", OK_BODY),
    ])
    .await;
    let mut c = OpenAiClient::new(base, None, Arc::new(CollectSink::default()));
    c.base_retry_delay_ms = 1;
    assert_eq!(
        c.chat(req()).await.unwrap().message.content.as_deref(),
        Some("ok")
    );
    assert_eq!(served.load(Ordering::SeqCst), 2);
}

#[test]
fn p0_5_mid_response_error_classification() {
    let e = mid_response_error(&mid_error(
        429,
        "rate_limit_exceeded",
        "Rate limit exceeded",
    ))
    .unwrap();
    assert_eq!(e.class(), ErrorClass::RateLimit);
    let e = mid_response_error(&mid_error(
        502,
        "provider_unavailable",
        "Provider disconnected",
    ))
    .unwrap();
    assert_eq!(e.class(), ErrorClass::Transient);
    assert_eq!(
        e.describe(),
        "provider error mid-response: HTTP 502 (provider_unavailable): Provider disconnected"
    );
    let e = mid_response_error(&mid_error(403, "content_policy_violation", "flagged")).unwrap();
    assert_eq!(e.class(), ErrorClass::Fatal);
    assert!(e.describe().contains("content filter"));

    let top = r#"{"id":"gen-2","error":{"code":503,"message":"overloaded","metadata":{"error_type":"provider_overloaded"}}}"#;
    assert_eq!(mid_response_error(top).unwrap().code, Some(503));

    assert!(mid_response_error(
        r#"{"error":{"message":"unexpected tokens remaining","code":400}}"#
    )
    .is_none());
    assert!(mid_response_error(OK_BODY).is_none());
}

#[tokio::test]
async fn p0_5_mid_response_429_is_retried_then_succeeds() {
    let (base, served, _) = fake_server(vec![
        answer(
            200,
            "",
            &mid_error(429, "rate_limit_exceeded", "Rate limit exceeded"),
        ),
        answer(200, "", OK_BODY),
    ])
    .await;
    let sink = Arc::new(CollectSink::default());
    let mut c = OpenAiClient::new(base, None, sink.clone());
    c.base_retry_delay_ms = 1;
    assert_eq!(
        c.chat(req()).await.unwrap().message.content.as_deref(),
        Some("ok")
    );
    assert_eq!(served.load(Ordering::SeqCst), 2);
    let ev = sink.events.lock().unwrap();
    assert!(ev.iter().any(|e| matches!(
        e,
        AgentEvent::ApiRetry {
            error_status: Some(429),
            ..
        }
    )));
}

#[tokio::test]
async fn p0_5_mid_response_errors_stop_after_three_retries() {
    let (base, served, _) = fake_server(vec![answer(
        200,
        "",
        &mid_error(
            502,
            "provider_unavailable",
            "Provider disconnected mid-stream",
        ),
    )])
    .await;
    let sink = Arc::new(CollectSink::default());
    let mut c = OpenAiClient::new(base, None, sink.clone());
    c.base_retry_delay_ms = 1;
    let err = c.chat(req()).await.unwrap_err().to_string();
    assert_eq!(served.load(Ordering::SeqCst), 4, "1 try + 3 retries");
    assert_eq!(retry_delays(&sink).len(), 3);
    assert!(
        err.contains("HTTP 502 (provider_unavailable)")
            && err.contains("failed mid-response 4 times"),
        "{err}"
    );
}

#[tokio::test]
async fn p0_5_content_filter_is_one_request_not_a_resample() {
    let body = r#"{"choices":[{"message":{"role":"assistant","content":null,"refusal":"I can't help with that."},"finish_reason":"content_filter"}]}"#;
    let (base, served, _) =
        fake_server(vec![answer(200, "", body), answer(200, "", OK_BODY)]).await;
    let mut c = OpenAiClient::new(base, None, Arc::new(CollectSink::default()));
    c.base_retry_delay_ms = 1;
    let r = c.chat(req()).await.unwrap();
    assert_eq!(served.load(Ordering::SeqCst), 1);
    assert_eq!(r.finish_reason.as_deref(), Some("content_filter"));
    assert_eq!(
        r.message.content.as_deref(),
        Some("I can't help with that.")
    );
}

fn filtered(text: Option<&str>) -> ChatResponse {
    ChatResponse {
        message: ChatMessage::assistant(text.map(str::to_string), None),
        usage: Usage::default(),
        finish_reason: Some("content_filter".into()),
    }
}

#[tokio::test]
async fn p0_5_agent_reports_content_filter_without_requery() {
    let llm = MockLlm::new(vec![filtered(None), resp("should never be requested")]);
    let (mut agent, _sink, _h) = build_agent(
        llm.clone(),
        registry_with(vec![]),
        FakePerms::new(PermissionDecision::Allow),
        base_cfg(),
    );
    let res = agent.run_turn("hi").await;
    assert!(res.is_error);
    assert_eq!(res.subtype, "error_during_execution");
    assert!(res.result.contains("content filter"), "{}", res.result);
    assert_eq!(llm.requests().len(), 1, "no empty-reply requery");
}

#[tokio::test]
async fn p0_5_agent_keeps_partial_text_of_a_filtered_reply() {
    let llm = MockLlm::new(vec![filtered(Some("Here is the start"))]);
    let (mut agent, sink, _h) = build_agent(
        llm.clone(),
        registry_with(vec![]),
        FakePerms::new(PermissionDecision::Allow),
        base_cfg(),
    );
    let res = agent.run_turn("hi").await;
    assert!(
        res.is_error && res.result.contains("Partial reply: Here is the start"),
        "{}",
        res.result
    );
    let ev = sink.events.lock().unwrap();
    assert!(ev.iter().any(|e| matches!(e, AgentEvent::Assistant { .. })));
}
