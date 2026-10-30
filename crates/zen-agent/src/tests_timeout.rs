use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use zen_core::*;

use crate::client::{request_id_from_error, AnthropicClient, HttpTimeouts, OpenAiClient};
use crate::testutil::*;

#[derive(Clone)]
struct Reply {
    delay_ms: u64,
    status: u16,
    headers: &'static str,
    body: &'static str,

    stall_after: Option<usize>,
}

fn reply(delay_ms: u64, status: u16, headers: &'static str, body: &'static str) -> Reply {
    Reply {
        delay_ms,
        status,
        headers,
        body,
        stall_after: None,
    }
}

async fn slow_server(replies: Vec<Reply>) -> (String, Arc<AtomicUsize>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = Arc::new(AtomicUsize::new(0));
    let counter = served.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let i = counter.fetch_add(1, Ordering::SeqCst);
            let r = replies[i.min(replies.len() - 1)].clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 65536];
                let mut got = Vec::new();
                loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    got.extend_from_slice(&buf[..n]);
                    let txt = String::from_utf8_lossy(&got);
                    if let Some(h) = txt.find("\r\n\r\n") {
                        let len = txt[..h]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if got.len() >= h + 4 + len {
                            break;
                        }
                    }
                    if n == 0 {
                        return;
                    }
                }
                tokio::time::sleep(Duration::from_millis(r.delay_ms)).await;
                let head = format!(
                    "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n",
                    r.status,
                    r.body.len(),
                    r.headers
                );
                let _ = sock.write_all(head.as_bytes()).await;
                match r.stall_after {
                    Some(n) => {
                        let _ = sock.write_all(&r.body.as_bytes()[..n]).await;
                        let _ = sock.flush().await;
                        tokio::time::sleep(Duration::from_secs(30)).await;
                    }
                    None => {
                        let _ = sock.write_all(r.body.as_bytes()).await;
                        let _ = sock.shutdown().await;
                    }
                }
            });
        }
    });
    (format!("http://{}", addr), served)
}

const OK_BODY: &str = r#"{"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;

fn req() -> ChatRequest {
    ChatRequest {
        model: "m".into(),
        messages: vec![ChatMessage::user("hi")],
        tools: vec![],
        max_tokens: None,
    }
}

fn timeouts(request_ms: Option<u64>, idle_ms: Option<u64>) -> HttpTimeouts {
    HttpTimeouts {
        request: request_ms.map(Duration::from_millis),
        idle: idle_ms.map(Duration::from_millis),
        connect: Duration::from_secs(10),
    }
}

fn retry_errors(sink: &CollectSink) -> Vec<String> {
    sink.events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ApiRetry { error, .. } => Some(error.clone()),
            _ => None,
        })
        .collect()
}

fn client(url: &str, sink: Arc<CollectSink>, t: HttpTimeouts) -> OpenAiClient {
    let mut c = OpenAiClient::new(url.to_string(), Some("sk-test-SECRET".into()), sink);
    c.base_retry_delay_ms = 1;
    c.set_timeouts(t);
    c
}

#[tokio::test]
async fn p0_2_slow_server_times_out_at_the_configured_threshold_then_retries() {
    let (url, served) = slow_server(vec![
        reply(2_000, 200, "", OK_BODY),
        reply(0, 200, "", OK_BODY),
    ])
    .await;
    let sink = Arc::new(CollectSink::default());
    let c = client(&url, sink.clone(), timeouts(Some(300), None));
    let t0 = Instant::now();
    let r = c
        .chat(req())
        .await
        .expect("retry after the timeout should succeed");
    let took = t0.elapsed();
    assert_eq!(r.message.content.as_deref(), Some("ok"));
    assert_eq!(served.load(Ordering::SeqCst), 2);
    let errs = retry_errors(&sink);
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(
        errs[0].contains("timed out after 0.3 s") && errs[0].contains("api.timeoutSecs"),
        "{}",
        errs[0]
    );
    assert!(
        took >= Duration::from_millis(300) && took < Duration::from_millis(1_500),
        "took {took:?}"
    );
}

#[tokio::test]
async fn p0_2_timeouts_count_against_the_wait_budget() {
    let (url, served) = slow_server(vec![reply(2_000, 200, "", OK_BODY)]).await;
    let sink = Arc::new(CollectSink::default());
    let mut c = client(&url, sink.clone(), timeouts(Some(200), None));
    c.max_total_wait_ms = 500;
    let t0 = Instant::now();
    let err = c.chat(req()).await.unwrap_err().to_string();
    let took = t0.elapsed();
    assert!(
        err.contains("timed out") && err.contains("retry.maxWaitSecs"),
        "{err}"
    );

    assert_eq!(served.load(Ordering::SeqCst), 3);
    assert!(took < Duration::from_millis(1_500), "took {took:?}");
}

#[tokio::test]
async fn p0_2_idle_timeout_catches_a_stalled_body() {
    let stalled = Reply {
        stall_after: Some(10),
        ..reply(0, 200, "", OK_BODY)
    };
    let (url, served) = slow_server(vec![stalled, reply(0, 200, "", OK_BODY)]).await;
    let sink = Arc::new(CollectSink::default());

    let c = client(&url, sink.clone(), timeouts(None, Some(300)));
    let r = c
        .chat(req())
        .await
        .expect("retry after the stall should succeed");
    assert_eq!(r.message.content.as_deref(), Some("ok"));
    assert_eq!(served.load(Ordering::SeqCst), 2);
    let errs = retry_errors(&sink);
    assert!(errs[0].contains("api.idleTimeoutSecs"), "{}", errs[0]);
}

#[tokio::test]
async fn p0_2_request_id_header_is_in_the_error_and_never_the_key() {
    let (url, _) = slow_server(vec![reply(
        0,
        400,
        "x-request-id: abc\r\n",
        r#"{"error":{"message":"bad model"}}"#,
    )])
    .await;
    let c = client(
        &url,
        Arc::new(CollectSink::default()),
        HttpTimeouts::for_url(&url),
    );
    let err = c.chat(req()).await.unwrap_err().to_string();
    assert!(
        err.contains("HTTP 400") && err.contains("[request id: abc]"),
        "{err}"
    );
    assert!(!err.contains("SECRET"), "{err}");
    assert_eq!(request_id_from_error(&err).as_deref(), Some("abc"));

    let (url, _) = slow_server(vec![
        reply(0, 503, "request-id: req_503\r\n", "busy"),
        reply(0, 200, "", OK_BODY),
    ])
    .await;
    let sink = Arc::new(CollectSink::default());
    client(&url, sink.clone(), HttpTimeouts::for_url(&url))
        .chat(req())
        .await
        .unwrap();
    assert!(retry_errors(&sink)[0].contains("[request id: req_503]"));

    let (url, _) = slow_server(vec![reply(
        0,
        401,
        "",
        r#"{"error":{"message":"no","request_id":"r-body"}}"#,
    )])
    .await;
    let err = client(
        &url,
        Arc::new(CollectSink::default()),
        HttpTimeouts::for_url(&url),
    )
    .chat(req())
    .await
    .unwrap_err()
    .to_string();
    assert_eq!(
        request_id_from_error(&err).as_deref(),
        Some("r-body"),
        "{err}"
    );
}

#[tokio::test]
async fn p0_2_anthropic_client_times_out_retries_and_reports_request_id() {
    const ANTHROPIC_OK: &str = r#"{"type":"message","role":"assistant","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}"#;
    let (url, served) = slow_server(vec![
        reply(2_000, 200, "", ANTHROPIC_OK),
        reply(0, 200, "", ANTHROPIC_OK),
    ])
    .await;
    let sink = Arc::new(CollectSink::default());
    let mut c = AnthropicClient::new(url.clone(), Some("sk-ant-SECRET".into()), sink.clone());
    c.base_retry_delay_ms = 1;
    c.set_timeouts(timeouts(Some(300), None));
    let r = c
        .chat(req())
        .await
        .expect("retry after the timeout should succeed");
    assert_eq!(r.message.content.as_deref(), Some("ok"));
    assert_eq!(served.load(Ordering::SeqCst), 2);
    assert!(retry_errors(&sink)[0].contains("api.timeoutSecs"));

    let (url, _) = slow_server(vec![reply(
        0,
        400,
        "request-id: req_ant_1\r\n",
        r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad"}}"#,
    )])
    .await;
    let c = AnthropicClient::new(
        url,
        Some("sk-ant-SECRET".into()),
        Arc::new(CollectSink::default()),
    );
    let err = c.chat(req()).await.unwrap_err().to_string();
    assert!(
        err.contains("[request id: req_ant_1]") && !err.contains("SECRET"),
        "{err}"
    );
}

#[test]
fn p0_2_request_id_parsing_rejects_junk() {
    assert_eq!(
        request_id_from_error("HTTP 500: x [request id: a-1_B]"),
        Some("a-1_B".into())
    );
    assert_eq!(request_id_from_error("HTTP 500: x"), None);
    assert_eq!(request_id_from_error("x [request id: has space]"), None);
}
