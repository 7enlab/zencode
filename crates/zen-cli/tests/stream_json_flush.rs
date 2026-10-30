#![cfg(unix)]

mod common;

use common::*;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::sync::mpsc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

const LINE_WAIT: Duration = Duration::from_secs(20);

async fn gated_server(replies: Vec<Reply>, gates: Vec<oneshot::Receiver<()>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        for (reply, gate) in replies.into_iter().zip(gates) {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let _ = read_request(&mut sock).await;
            let _ = gate.await;
            let resp = format!(
                "HTTP/1.1 {} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                reply.status,
                reply.body.len(),
                reply.body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    format!("http://{addr}/v1")
}

fn next_record(rx: &mpsc::Receiver<String>, what: &str) -> Value {
    let line = rx.recv_timeout(LINE_WAIT).unwrap_or_else(|e| {
        panic!("no stream-json line for {what} while the model call was pending: {e}")
    });
    serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON ({e}): {line}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_json_lines_are_flushed_incrementally() {
    let home = TempDir::new("sjflush_home");
    let cwd = TempDir::new("sjflush_cwd");
    cwd.write("notes.txt", "hi\n");

    let (open1, gate1) = oneshot::channel();
    let (open2, gate2) = oneshot::channel();
    let tool_call = Reply::with_tool_calls(
        "",
        Some(json!([{"id": "call_read_1", "type": "function",
            "function": {"name": "Read", "arguments": json!({"file_path": "notes.txt"}).to_string()}}])),
    );
    let base_url = gated_server(
        vec![tool_call, Reply::text("The file says hi.")],
        vec![gate1, gate2],
    )
    .await;

    let mut child = ZenRun::new("sjflush", &home, &cwd)
        .args([
            "-p",
            "--output-format",
            "stream-json",
            "--bare",
            "--setting-sources",
            "",
        ])
        .args(["--model", "mock-model", "--base-url", base_url.as_str()])
        .arg("hello")
        .spawn();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if !line.trim().is_empty() && tx.send(line).is_err() {
                break;
            }
        }
    });

    let init = next_record(&rx, "init");
    assert_eq!(
        (init["type"].as_str(), init["subtype"].as_str()),
        (Some("system"), Some("init"))
    );
    assert!(
        rx.try_recv().is_err(),
        "nothing but init can exist before the first reply"
    );

    open1.send(()).unwrap();
    let assistant = next_record(&rx, "assistant tool call");
    assert_eq!(assistant["type"], "assistant", "{assistant}");
    let tool_result = next_record(&rx, "tool result");
    assert_eq!(tool_result["type"], "user", "{tool_result}");
    assert_eq!(
        tool_result["message"]["content"][0]["tool_use_id"],
        "call_read_1"
    );

    open2.send(()).unwrap();
    let mut rest = Vec::new();
    while let Ok(line) = rx.recv_timeout(LINE_WAIT) {
        rest.push(serde_json::from_str::<Value>(&line).unwrap());
    }
    let status = child.wait().unwrap();
    assert!(
        status.success(),
        "zen exited with {status:?}; rest: {rest:?}"
    );
    assert_eq!(
        rest.last().map(|r| r["type"].clone()),
        Some(json!("result")),
        "{rest:?}"
    );
    assert_eq!(
        rest.iter().filter(|r| r["type"] == "result").count(),
        1,
        "{rest:?}"
    );
}
