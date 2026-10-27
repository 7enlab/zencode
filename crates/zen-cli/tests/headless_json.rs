use std::process::Command;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn read_request(sock: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        let n = match sock.read(&mut tmp).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        buf.extend_from_slice(&tmp[..n]);
        let s = String::from_utf8_lossy(&buf);
        if let Some(hdr_end) = s.find("\r\n\r\n") {
            let len = s[..hdr_end]
                .lines()
                .find_map(|l| {
                    let low = l.to_ascii_lowercase();
                    low.strip_prefix("content-length:")
                        .and_then(|v| v.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            if buf.len() >= hdr_end + 4 + len {
                break;
            }
        }
    }
    buf
}

async fn write_json(sock: &mut tokio::net::TcpStream, body: &str) {
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = sock.write_all(resp.as_bytes()).await;
    let _ = sock.shutdown().await;
}

fn zen_bin() -> std::path::PathBuf {

    if let Ok(dir) = std::env::var("CARGO_TARGET_DIR") {
        let p = std::path::PathBuf::from(dir).join("debug").join("zen");
        if p.exists() {
            return p;
        }
    }

    let mut p = std::env::current_exe().unwrap();
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.join("zen")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn headless_json_against_mock_server() {
    let body = tokio::time::timeout(Duration::from_secs(30), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let _req = read_request(&mut sock).await;
            let resp = serde_json::json!({
                "id": "chatcmpl-1",
                "object": "chat.completion",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "hello from mock"},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10}
            });
            write_json(&mut sock, &resp.to_string()).await;
        });

        let home = std::env::temp_dir().join(format!("zen_it_home_{}", std::process::id()));
        let cwd = std::env::temp_dir().join(format!("zen_it_cwd_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&cwd);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();

        let bin = zen_bin();
        let out = Command::new(&bin)
            .current_dir(&cwd)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env_remove("ZEN_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("ZEN_MODEL")
            .env_remove("ZEN_BASE_URL")
            .args([
                "-p",
                "--output-format",
                "json",
                "--bare",
                "--setting-sources",
                "",
                "--model",
                "mock-model",
                "--base-url",
                &format!("http://{}/v1", addr),
                "say hi",
            ])
            .output()
            .expect("failed to spawn zen binary");
        let _ = server.await;

        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        (out.status.success(), stdout, stderr)
    })
    .await
    .expect("test timed out after 30s");

    let (success, stdout, stderr) = body;
    let line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON line in stdout: {:?} / stderr: {:?}", stdout, stderr));
    let v: serde_json::Value = serde_json::from_str(line)
        .unwrap_or_else(|e| panic!("bad JSON {:?}: {} (stderr {})", line, e, stderr));

    assert_eq!(v["type"], "result");
    assert_eq!(v["subtype"], "success");
    assert_eq!(v["is_error"], false);
    assert_eq!(v["result"], "hello from mock");
    assert_eq!(v["model"], "mock-model");
    assert!(v["session_id"].as_str().map(|s| !s.is_empty()).unwrap_or(false));
    assert_eq!(v["total_cost_usd"], 0.0);
    assert!(v["permission_denials"].as_array().is_some());
    assert_eq!(v["usage"]["input_tokens"], 7);
    assert_eq!(v["usage"]["output_tokens"], 3);
    assert!(success, "process exited non-zero; stderr: {}", stderr);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn headless_stream_json_against_mock_server() {
    let out = tokio::time::timeout(Duration::from_secs(30), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let _ = read_request(&mut sock).await;
            let resp = serde_json::json!({
                "id": "c2", "object": "chat.completion",
                "choices": [{"index":0,"message":{"role":"assistant","content":"streamed answer"},"finish_reason":"stop"}],
                "usage": {"prompt_tokens": 2, "completion_tokens": 1, "total_tokens": 3}
            });
            write_json(&mut sock, &resp.to_string()).await;
        });

        let home = std::env::temp_dir().join(format!("zen_it2_home_{}", std::process::id()));
        let cwd = std::env::temp_dir().join(format!("zen_it2_cwd_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&cwd);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();

        let out = Command::new(zen_bin())
            .current_dir(&cwd)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env_remove("ZEN_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("ZEN_MODEL")
            .env_remove("ZEN_BASE_URL")
            .args([
                "-p", "--output-format", "stream-json", "--bare", "--setting-sources", "",
                "--model", "mock-model", "--base-url", &format!("http://{}/v1", addr),
                "--session-id", "00000000-0000-4000-8000-000000000001", "say hi",
            ])
            .output()
            .expect("spawn");
        let _ = server.await;
        (out.status.success(), String::from_utf8_lossy(&out.stdout).to_string())
    })
    .await
    .expect("timed out");

    let (success, stdout) = out;
    let lines: Vec<serde_json::Value> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad line {:?}: {}", l, e)))
        .collect();
    assert!(success, "stdout: {}", stdout);
    assert!(lines.len() >= 3, "expected >=3 lines, got {}", stdout);
    assert_eq!(lines[0]["type"], "system");
    assert_eq!(lines[0]["subtype"], "init");
    assert_eq!(lines[0]["session_id"], "00000000-0000-4000-8000-000000000001");
    assert_eq!(lines[0]["model"], "mock-model");
    assert_eq!(lines[0]["permissionMode"], "default");
    assert!(lines[0]["tools"].as_array().is_some());
    assert!(lines[0]["slash_commands"].as_array().is_some());

    let last = lines.last().unwrap();
    assert_eq!(last["type"], "result");
    assert_eq!(last["result"], "streamed answer");
    assert!(lines.iter().any(|l| l["type"] == "assistant"), "no assistant line: {}", stdout);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn headless_text_mode_prints_result_only() {
    let out = tokio::time::timeout(Duration::from_secs(30), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let _ = read_request(&mut sock).await;
            let resp = serde_json::json!({
                "id":"c3","object":"chat.completion",
                "choices":[{"index":0,"message":{"role":"assistant","content":"plain text answer"},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}
            });
            write_json(&mut sock, &resp.to_string()).await;
        });
        let home = std::env::temp_dir().join(format!("zen_it3_home_{}", std::process::id()));
        let cwd = std::env::temp_dir().join(format!("zen_it3_cwd_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&cwd);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let out = Command::new(zen_bin())
            .current_dir(&cwd)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env_remove("ZEN_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("ZEN_MODEL")
            .env_remove("ZEN_BASE_URL")
            .args([
                "-p", "--bare", "--setting-sources", "",
                "--model", "mock-model", "--base-url", &format!("http://{}/v1", addr),
                "hi",
            ])
            .output()
            .expect("spawn");
        let _ = server.await;
        (out.status.success(), String::from_utf8_lossy(&out.stdout).to_string())
    })
    .await
    .expect("timed out");
    let (success, stdout) = out;
    assert!(success, "stdout: {}", stdout);
    assert_eq!(stdout.trim(), "plain text answer");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_replays_prior_messages() {
    let ok = tokio::time::timeout(Duration::from_secs(30), async {
        let home = std::env::temp_dir().join(format!("zen_it4_home_{}", std::process::id()));
        let cwd = std::env::temp_dir().join(format!("zen_it4_cwd_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&cwd);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();

        let l1 = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a1 = l1.local_addr().unwrap();
        let s1 = tokio::spawn(async move {
            let (mut sock, _) = l1.accept().await.unwrap();
            let _ = read_request(&mut sock).await;
            let r = serde_json::json!({
                "id":"r1","object":"chat.completion",
                "choices":[{"index":0,"message":{"role":"assistant","content":"first reply"},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}
            });
            write_json(&mut sock, &r.to_string()).await;
        });
        let o1 = Command::new(zen_bin())
            .current_dir(&cwd)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env_remove("ZEN_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("ZEN_MODEL")
            .env_remove("ZEN_BASE_URL")
            .args([
                "-p", "--bare", "--setting-sources", "", "--session-id", "00000000-0000-4000-8000-000000000002",
                "--model", "m", "--base-url", &format!("http://{}/v1", a1), "remember alpha",
            ])
            .output()
            .expect("spawn turn1");
        let _ = s1.await;
        assert!(o1.status.success(), "turn1 failed: {}", String::from_utf8_lossy(&o1.stderr));

        let l2 = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a2 = l2.local_addr().unwrap();
        let s2 = tokio::spawn(async move {
            let (mut sock, _) = l2.accept().await.unwrap();
            let req = read_request(&mut sock).await;
            let text = String::from_utf8_lossy(&req).to_string();
            let r = serde_json::json!({
                "id":"r2","object":"chat.completion",
                "choices":[{"index":0,"message":{"role":"assistant","content":"second reply"},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}
            });
            write_json(&mut sock, &r.to_string()).await;
            text
        });
        let o2 = Command::new(zen_bin())
            .current_dir(&cwd)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env_remove("ZEN_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("ZEN_MODEL")
            .env_remove("ZEN_BASE_URL")
            .args([
                "-p", "--bare", "--setting-sources", "", "--resume", "00000000-0000-4000-8000-000000000002",
                "--model", "m", "--base-url", &format!("http://{}/v1", a2), "what did I say?",
            ])
            .output()
            .expect("spawn turn2");
        let req_text = s2.await.unwrap();
        assert!(o2.status.success(), "turn2 failed: {}", String::from_utf8_lossy(&o2.stderr));

        let body = req_text.split("\r\n\r\n").nth(1).unwrap_or("");
        let sent: serde_json::Value = serde_json::from_str(body).expect("request body json");
        let msgs = sent["messages"].as_array().expect("messages array");
        let joined = serde_json::to_string(msgs).unwrap();
        assert!(joined.contains("remember alpha"), "prior user message missing: {}", joined);
        assert!(joined.contains("first reply"), "prior assistant message missing: {}", joined);
        assert!(joined.contains("what did I say?"), "new prompt missing: {}", joined);

        assert_eq!(joined.matches("what did I say?").count(), 1, "duplicate prompt: {}", joined);
        true
    })
    .await
    .expect("timed out");
    assert!(ok);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_call_round_trip_and_allowed_tools_filter() {
    let ok = tokio::time::timeout(Duration::from_secs(30), async {
        let home = std::env::temp_dir().join(format!("zen_it5_home_{}", std::process::id()));
        let cwd = std::env::temp_dir().join(format!("zen_it5_cwd_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&cwd);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(cwd.join("sample.txt"), "tool-content-42").unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {

            let (mut sock, _) = listener.accept().await.unwrap();
            let _ = read_request(&mut sock).await;
            let r1 = serde_json::json!({
                "id":"t1","object":"chat.completion",
                "choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":[{
                    "id":"call_1","type":"function",
                    "function":{"name":"Read","arguments":"{\"file_path\":\"sample.txt\"}"}
                }]},"finish_reason":"tool_calls"}],
                "usage":{"prompt_tokens":5,"completion_tokens":5,"total_tokens":10}
            });
            write_json(&mut sock, &r1.to_string()).await;

            let (mut sock2, _) = listener.accept().await.unwrap();
            let req2 = read_request(&mut sock2).await;
            let text2 = String::from_utf8_lossy(&req2).to_string();
            let r2 = serde_json::json!({
                "id":"t2","object":"chat.completion",
                "choices":[{"index":0,"message":{"role":"assistant","content":"done after tool"},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":8,"completion_tokens":2,"total_tokens":10}
            });
            write_json(&mut sock2, &r2.to_string()).await;
            text2
        });

        let out = Command::new(zen_bin())
            .current_dir(&cwd)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env_remove("ZEN_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("ZEN_MODEL")
            .env_remove("ZEN_BASE_URL")
            .args([
                "-p", "--output-format", "stream-json", "--bare", "--setting-sources", "",
                "--permission-mode", "bypassPermissions",
                "--allowedTools", "Read",
                "--model", "m", "--base-url", &format!("http://{}/v1", addr),
                "read the file",
            ])
            .output()
            .expect("spawn");
        let req2 = server.await.unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).to_string(), req2)
    })
    .await
    .expect("timed out");

    let (success, stdout, req2) = ok;
    assert!(success, "stdout: {}", stdout);

    let first: serde_json::Value = serde_json::from_str(stdout.lines().next().unwrap()).unwrap();
    let tools: Vec<String> = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap().to_string())
        .collect();
    assert_eq!(tools, vec!["Read".to_string()], "tools not filtered: {:?}", tools);

    let body = req2.split("\r\n\r\n").nth(1).unwrap_or("");
    let sent: serde_json::Value = serde_json::from_str(body).expect("body json");
    let joined = serde_json::to_string(&sent["messages"]).unwrap();
    assert!(joined.contains("tool-content-42"), "tool result missing: {}", joined);

    let last = stdout.lines().rfind(|l| !l.trim().is_empty()).unwrap();
    let v: serde_json::Value = serde_json::from_str(last).unwrap();
    assert_eq!(v["type"], "result");
    assert_eq!(v["result"], "done after tool");
}
