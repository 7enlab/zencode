mod common;

use common::*;

fn fake_mcp_server_script() -> String {
    r#"import sys, json, asyncio

async def main():
    # Read JSON-RPC requests from stdin, one per line
    while True:
        line = sys.stdin.readline()
        if not line:
            break
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError:
            continue

        method = req.get("method")
        id = req.get("id")

        if method == "initialize":
            # Respond to initialize
            resp = {
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "fake", "version": "1.0.0"}
                }
            }
        elif method == "initialized":
            # Notification, no response needed
            continue
        elif method == "tools/list":
            resp = {
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": [{
                        "name": "echo",
                        "description": "Echo the input",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "message": {"type": "string"}
                            },
                            "required": ["message"]
                        }
                    }]
                }
            }
        elif method == "tools/call":
            params = req.get("params", {})
            name = params.get("name")
            args = params.get("arguments", {})
            if name == "echo":
                message = args.get("message", "")
                resp = {
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{"type": "text", "text": f"ECHO: {message}"}],
                        "isError": False
                    }
                }
            else:
                resp = {
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32601, "message": f"Unknown tool: {name}"}
                }
        else:
            resp = {
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": f"Unknown method: {method}"}
            }

        print(json.dumps(resp), flush=True)

if __name__ == "__main__":
    asyncio.run(main())
"#
    .to_string()
}

fn write_mcp_server_script(dir: &TempDir) -> std::path::PathBuf {
    dir.write("mcp_fake_server.py", &fake_mcp_server_script())
}

fn mcp_config_json(server_path: &str) -> String {
    format!(
        r#"{{"mcpServers":{{"fake":{{"command":"python3","args":["{}"]}}}}}}"#,
        server_path
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_server_connected_and_tool_works() {
    let home = TempDir::new("mcp_ok_home");
    let cwd = TempDir::new("mcp_ok_cwd");

    let server_script = write_mcp_server_script(&cwd);
    let server_path = server_script.to_string_lossy().to_string();
    let mcp_config = mcp_config_json(&server_path);

    let server = MockServer::start(vec![
        Reply::with_tool_calls(
            "",
            Some(serde_json::json!([{
                "id": "call_1",
                "type": "function",
                "function": {"name": "mcp__fake__echo", "arguments": "{\"message\":\"hello mcp\"}"}
            }])),
        ),
        Reply::text("mcp tool worked"),
    ])
    .await;

    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("mcp_ok", &home, &cwd)
            .args([
                "-p",
                "--output-format",
                "stream-json",
                "--bare",
                "--permission-mode",
                "bypassPermissions",
                "--mcp-config",
                &mcp_config,
            ])
            .mock("mock-model", &server)
            .arg("call mcp echo")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let lines = ndjson(&stdout);
    let init = &lines[0];
    assert_eq!(init["type"], "system");
    assert_eq!(init["subtype"], "init");

    let mcp_servers = init["mcp_servers"].as_array().expect("mcp_servers array");
    assert!(
        !mcp_servers.is_empty(),
        "mcp_servers should list our fake server"
    );
    let fake_server = mcp_servers
        .iter()
        .find(|s| s["name"] == "fake")
        .expect("fake server in list");
    assert_eq!(
        fake_server["status"], "connected",
        "fake server should be connected: {}",
        fake_server
    );

    let tools: Vec<String> = init["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t.as_str().map(|s| s.to_string()))
        .collect();
    assert!(
        tools.contains(&"mcp__fake__echo".to_string()),
        "MCP tool should be in tools list: {:?}",
        tools
    );

    let v = last_result(&stdout);
    assert_eq!(v["result"], "mcp tool worked");

    let requests = server.requests().await;
    assert_eq!(requests.len(), 2);
    let second_req = request_messages_text(&requests[1]);
    assert!(
        second_req.contains("ECHO: hello mcp"),
        "MCP tool result should be in model request: {}",
        second_req
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_server_failed_shows_failed_status() {
    let home = TempDir::new("mcp_fail_home");
    let cwd = TempDir::new("mcp_fail_cwd");

    let mcp_config = r#"{"mcpServers":{"broken":{"command":"nonexistent-command-xyz","args":[]}}}"#;

    let server = MockServer::single_text("ok").await;

    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("mcp_fail", &home, &cwd)
            .args([
                "-p",
                "--output-format",
                "stream-json",
                "--bare",
                "--mcp-config",
                mcp_config,
            ])
            .mock("mock-model", &server)
            .arg("hi")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let lines = ndjson(&stdout);
    let init = &lines[0];
    let mcp_servers = init["mcp_servers"].as_array().expect("mcp_servers array");
    let broken_server = mcp_servers
        .iter()
        .find(|s| s["name"] == "broken")
        .expect("broken server in list");
    assert_eq!(
        broken_server["status"], "failed",
        "broken server should show failed: {}",
        broken_server
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multiple_mcp_servers_mixed_status() {
    let home = TempDir::new("mcp_multi_home");
    let cwd = TempDir::new("mcp_multi_cwd");

    let server_script = write_mcp_server_script(&cwd);
    let server_path = server_script.to_string_lossy().to_string();
    let mcp_config = format!(
        r#"{{"mcpServers":{{"good":{{"command":"python3","args":["{}"]}},"bad":{{"command":"nonexistent","args":[]}}}}}}"#,
        server_path
    );

    let server = MockServer::single_text("ok").await;

    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("mcp_multi", &home, &cwd)
            .args([
                "-p",
                "--output-format",
                "stream-json",
                "--bare",
                "--mcp-config",
                &mcp_config,
            ])
            .mock("mock-model", &server)
            .arg("hi")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let lines = ndjson(&stdout);
    let init = &lines[0];
    let mcp_servers = init["mcp_servers"].as_array().expect("mcp_servers array");

    let good = mcp_servers
        .iter()
        .find(|s| s["name"] == "good")
        .expect("good server");
    assert_eq!(good["status"], "connected");

    let bad = mcp_servers
        .iter()
        .find(|s| s["name"] == "bad")
        .expect("bad server");
    assert_eq!(bad["status"], "failed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn strict_mcp_config_ignores_file_configs() {
    let home = TempDir::new("mcp_strict_home");
    let cwd = TempDir::new("mcp_strict_cwd");

    cwd.write(
        ".mcp.json",
        r#"{"mcpServers":{"project":{"command":"python3","args":["/nonexistent.py"]}}}"#,
    );

    let server_script = write_mcp_server_script(&cwd);
    let server_path = server_script.to_string_lossy().to_string();
    let cli_mcp_config = mcp_config_json(&server_path);

    let server = MockServer::single_text("ok").await;

    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("mcp_strict", &home, &cwd)
            .args([
                "-p",
                "--output-format",
                "stream-json",
                "--bare",
                "--strict-mcp-config",
                "--mcp-config",
                &cli_mcp_config,
            ])
            .mock("mock-model", &server)
            .arg("hi")
            .run_output()
    })
    .await
    .expect("timed out");

    assert_eq!(code, 0, "stderr: {}", stderr);
    let lines = ndjson(&stdout);
    let init = &lines[0];
    let mcp_servers = init["mcp_servers"].as_array().expect("mcp_servers array");

    assert_eq!(
        mcp_servers.len(),
        1,
        "strict mode should only have CLI server: {:?}",
        mcp_servers
    );
    assert_eq!(mcp_servers[0]["name"], "fake");
    assert_eq!(mcp_servers[0]["status"], "connected");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c7_3_invalid_entries_reported_in_init() {
    let home = TempDir::new("mcp_inv_home");
    let cwd = TempDir::new("mcp_inv_cwd");
    let script = write_mcp_server_script(&cwd);
    let cfg = format!(
        r#"{{"mcpServers":{{"good":{{"command":"python3","args":["{}"]}},"nourl":{{"url":"http://127.0.0.1:9/mcp"}},"odd":{{"type":"carrier-pigeon","url":"x"}}}}}}"#,
        script.display()
    );
    let server = MockServer::single_text("ok").await;
    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("mcp_inv", &home, &cwd)
            .args([
                "-p",
                "--output-format",
                "stream-json",
                "--bare",
                "--mcp-config",
                &cfg,
            ])
            .mock("mock-model", &server)
            .arg("hi")
            .run_output()
    })
    .await
    .expect("timed out");
    assert_eq!(code, 0, "stderr: {}", stderr);
    let init = &ndjson(&stdout)[0];
    let servers = init["mcp_servers"].as_array().unwrap();
    assert_eq!(servers.len(), 1, "{}", init);
    assert_eq!(servers[0]["name"], "good");
    assert_eq!(servers[0]["status"], "connected");
    let mut errs: Vec<(String, String)> = init["mcp_server_errors"]
        .as_array()
        .expect("mcp_server_errors")
        .iter()
        .map(|e| {
            (
                e["name"].as_str().unwrap().into(),
                e["reason"].as_str().unwrap().into(),
            )
        })
        .collect();
    errs.sort();
    assert_eq!(
        errs,
        vec![
            ("nourl".into(), "url_missing_type".into()),
            ("odd".into(), "unknown_type".into())
        ]
    );

    assert!(!stderr.contains("skipped"), "stderr: {}", stderr);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c7_5_mcp_subcommands_round_trip() {
    let home = TempDir::new("mcp_cmd_home");
    let cwd = TempDir::new("mcp_cmd_cwd");
    let script = write_mcp_server_script(&cwd);
    let run = |args: Vec<String>| ZenRun::new("mcp_cmd", &home, &cwd).args(args).run_output();
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();

    let (_, err, code) = run(s(&[
        "mcp",
        "add",
        "-e",
        "K=V",
        "fake",
        "--",
        "python3",
        "${ZEN_TEST_NOPE:-SCRIPT}",
    ]));
    assert_eq!(code, 0, "{}", err);
    let local: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(cwd.path().join(".zen/mcp.json")).unwrap())
            .unwrap();
    assert_eq!(local["mcpServers"]["fake"]["env"]["K"], "V");

    let text = std::fs::read_to_string(cwd.path().join(".zen/mcp.json"))
        .unwrap()
        .replace("SCRIPT", &script.display().to_string());
    std::fs::write(cwd.path().join(".zen/mcp.json"), text).unwrap();

    let (_, err, code) = run(s(&[
        "mcp",
        "add",
        "--transport",
        "http",
        "-s",
        "project",
        "--header",
        "Authorization: Bearer t",
        "web",
        "http://127.0.0.1:9/mcp",
    ]));
    assert_eq!(code, 0, "{}", err);
    let (out, err, code) = run(s(&["mcp", "get", "web"]));
    assert_eq!(code, 0, "{}", err);
    let web: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(web["type"], "http");
    assert_eq!(web["headers"]["Authorization"], "Bearer t");
    let (_, _, code) = run(s(&["mcp", "get", "nope"]));
    assert_ne!(code, 0);
    let (_, _, code) = run(s(&["mcp", "add-json", "bad", "[1,2]"]));
    assert_ne!(code, 0, "non-object JSON must be rejected");
    let (_, _, code) = run(s(&["mcp", "add", "--transport", "smoke", "x", "y"]));
    assert_ne!(code, 0, "unknown transport must be rejected");

    let server = MockServer::single_text("ok").await;
    let (stdout, stderr, code) = tokio::time::timeout(TEST_TIMEOUT, async {
        ZenRun::new("mcp_cmd", &home, &cwd)
            .args([
                "-p",
                "--output-format",
                "stream-json",
                "--setting-sources",
                "",
            ])
            .env("MCP_TIMEOUT", Some("5000"))
            .mock("mock-model", &server)
            .arg("hi")
            .run_output()
    })
    .await
    .expect("timed out");
    assert_eq!(code, 0, "stderr: {}", stderr);
    let init = &ndjson(&stdout)[0];
    let st: Vec<(String, String)> = init["mcp_servers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["name"].as_str().unwrap().into(),
                e["status"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert!(
        st.contains(&("fake".into(), "connected".into())),
        "{:?}",
        st
    );
    assert!(st.contains(&("web".into(), "failed".into())), "{:?}", st);
    assert!(init["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t == "mcp__fake__echo"));

    let (_, err, code) = run(s(&["mcp", "remove", "fake"]));
    assert_eq!(code, 0, "{}", err);
    let (_, _, code) = run(s(&["mcp", "get", "-s", "local", "fake"]));
    assert_ne!(code, 0);
}
