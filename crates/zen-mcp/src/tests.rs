


use crate::*;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zen_core::*;

const TEST_TIMEOUT: Duration = Duration::from_secs(30);

fn tmpdir(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("zen_mcp_{}_{}", tag, uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&p).unwrap();
    p
}



#[test]
fn untrusted_folder_skips_project_mcp_files() {
    let cwd = tmpdir("trust_cwd");
    let home = tmpdir("trust_home");
    std::fs::write(cwd.join(".mcp.json"), r#"{"mcpServers":{"proj":{"command":"echo"}}}"#).unwrap();
    std::fs::create_dir_all(cwd.join(".zen")).unwrap();
    std::fs::write(cwd.join(".zen/mcp.json"), r#"{"mcpServers":{"local":{"command":"echo"}}}"#).unwrap();
    std::fs::create_dir_all(home.join(".zen")).unwrap();
    std::fs::write(home.join(".zen/mcp.json"), r#"{"mcpServers":{"user":{"command":"cat"}}}"#).unwrap();
    let cli = vec![r#"{"mcpServers":{"cli":{"command":"cat"}}}"#.to_string()];

    let un = crate::load_mcp_configs_scoped(&cwd, Some(&home), &cli, false, false).unwrap();
    assert_eq!(un.keys().cloned().collect::<Vec<_>>(), vec!["cli", "user"]);
    let tr = crate::load_mcp_configs_scoped(&cwd, Some(&home), &cli, false, true).unwrap();
    assert_eq!(tr.keys().cloned().collect::<Vec<_>>(), vec!["cli", "local", "proj", "user"]);

    let _ = std::fs::remove_dir_all(&cwd);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn load_configs_project_user_and_cli() {
    let cwd = tmpdir("cfg_cwd");
    let home = tmpdir("cfg_home");

    std::fs::write(
        cwd.join(".mcp.json"),
        r#"{"mcpServers":{"proj":{"command":"echo","args":["a"]}}}"#,
    )
    .unwrap();
    std::fs::create_dir_all(home.join(".zen")).unwrap();
    std::fs::write(
        home.join(".zen").join("mcp.json"),
        r#"{"mcpServers":{"user":{"command":"cat"},"proj":{"command":"overridden"}}}"#,
    )
    .unwrap();

    let cfg = load_mcp_configs(&cwd, Some(&home), &[], false).unwrap();
    assert_eq!(cfg.len(), 2);
    assert_eq!(cfg["user"].command.as_deref(), Some("cat"));

    assert_eq!(cfg["proj"].command.as_deref(), Some("echo"));


    let cli = vec![r#"{"mcpServers":{"cli":{"type":"http","url":"http://x"}}}"#.to_string()];
    let cfg = load_mcp_configs(&cwd, Some(&home), &cli, false).unwrap();
    assert_eq!(cfg.len(), 3);
    assert_eq!(cfg["cli"].url.as_deref(), Some("http://x"));
    assert!(cfg["cli"].is_http());


    let cfg = load_mcp_configs(&cwd, Some(&home), &cli, true).unwrap();
    assert_eq!(cfg.len(), 1);
    assert!(cfg.contains_key("cli"));

    let _ = std::fs::remove_dir_all(&cwd);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn c7_scope_paths_match_spec_and_are_loaded() {
    let cwd = tmpdir("scope_cwd");
    let home = tmpdir("scope_home");
    let local = scope_path("local", &cwd, Some(&home)).unwrap();
    let project = scope_path("project", &cwd, Some(&home)).unwrap();
    let user = scope_path("user", &cwd, Some(&home)).unwrap();
    assert_eq!(local, cwd.join(".zen").join("mcp.json"));
    assert_eq!(project, cwd.join(".mcp.json"));
    assert_eq!(user, home.join(".zen").join("mcp.json"));
    assert!(scope_path("global", &cwd, Some(&home)).is_err());


    mcp_add(&user, "s", McpServerConfig { command: Some("u".into()), ..empty_cfg() }).unwrap();
    mcp_add(&user, "only_user", McpServerConfig { command: Some("u".into()), ..empty_cfg() }).unwrap();
    mcp_add(&project, "s", McpServerConfig { command: Some("p".into()), ..empty_cfg() }).unwrap();
    mcp_add(&local, "s", McpServerConfig { command: Some("l".into()), ..empty_cfg() }).unwrap();
    let cfg = load_mcp_configs(&cwd, Some(&home), &[], false).unwrap();
    assert_eq!(cfg["s"].command.as_deref(), Some("l"));
    assert_eq!(cfg["only_user"].command.as_deref(), Some("u"));
    mcp_remove(&local, "s").unwrap();
    let cfg = load_mcp_configs(&cwd, Some(&home), &[], false).unwrap();
    assert_eq!(cfg["s"].command.as_deref(), Some("p"));
    let (ex, _) = load_mcp_configs_ex(&cwd, Some(&home), &[], false);
    assert_eq!(ex["s"].command.as_deref(), Some("p"));

    let _ = std::fs::remove_dir_all(&cwd);
    let _ = std::fs::remove_dir_all(&home);
}

fn empty_cfg() -> McpServerConfig {
    McpServerConfig {
        kind: None,
        command: None,
        args: vec![],
        env: BTreeMap::new(),
        url: None,
        headers: BTreeMap::new(),
    }
}

#[tokio::test]
async fn c7_3_invalid_entries_are_skipped_with_reason() {
    let mut configs = BTreeMap::new();
    configs.insert(
        "nourltype".to_string(),
        McpServerConfig { url: Some("http://127.0.0.1:9".into()), ..empty_cfg() },
    );
    configs.insert(
        "weird".to_string(),
        McpServerConfig { kind: Some("ws".into()), url: Some("ws://x".into()), ..empty_cfg() },
    );
    let mgr = tokio::time::timeout(TEST_TIMEOUT, McpManager::start(configs, 2_000))
        .await
        .unwrap();

    assert!(mgr.status().is_empty(), "{:?}", mgr.status());
    let mut errs: Vec<(String, String)> =
        mgr.errors().into_iter().map(|e| (e.server_name, e.reason)).collect();
    errs.sort();
    assert_eq!(
        errs,
        vec![
            ("nourltype".to_string(), "url_missing_type".to_string()),
            ("weird".to_string(), "unknown_type".to_string()),
        ]
    );
}

#[test]
fn load_configs_from_cli_file() {
    let cwd = tmpdir("cfg_file");
    let home = tmpdir("cfg_file_home");
    let file = cwd.join("extra.json");
    std::fs::write(&file, r#"{"mcpServers":{"fromfile":{"command":"node","args":["s.js"]}}}"#).unwrap();

    let cli = vec![file.to_string_lossy().to_string()];
    let cfg = load_mcp_configs(&cwd, Some(&home), &cli, true).unwrap();
    assert_eq!(cfg["fromfile"].command.as_deref(), Some("node"));
    assert_eq!(cfg["fromfile"].args, vec!["s.js".to_string()]);

    let _ = std::fs::remove_dir_all(&cwd);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn load_configs_bare_map_and_bad_json() {
    let cwd = tmpdir("cfg_bare");
    let home = tmpdir("cfg_bare_home");
    let cli = vec![r#"{"srv":{"command":"x"}}"#.to_string()];
    let cfg = load_mcp_configs(&cwd, Some(&home), &cli, true).unwrap();
    assert_eq!(cfg["srv"].command.as_deref(), Some("x"));

    let bad = vec!["{not json".to_string()];
    assert!(load_mcp_configs(&cwd, Some(&home), &bad, true).is_err());

    let _ = std::fs::remove_dir_all(&cwd);
    let _ = std::fs::remove_dir_all(&home);
}



#[test]
fn tool_name_sanitization() {
    assert_eq!(crate::manager::mcp_tool_name("my server", "do.thing"), "mcp__my_server__do_thing");
    assert_eq!(crate::manager::mcp_tool_name("a/b", "c:d"), "mcp__a_b__c_d");
    assert_eq!(crate::manager::sanitize_component("ok-name_1"), "ok-name_1");
}




fn python_exe() -> Option<String> {
    for cand in ["python3", "python"] {
        if std::process::Command::new(cand)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return Some(cand.to_string());
        }
    }
    None
}

const SERVER_SCRIPT: &str = r#"
import sys, json

mode = sys.argv[1] if len(sys.argv) > 1 else "ok"
calls = 0

def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        msg = json.loads(line)
    except Exception:
        # ignore unknown/garbage lines
        continue
    method = msg.get("method")
    mid = msg.get("id")
    if method == "initialize":
        send({"jsonrpc":"2.0","id":mid,"result":{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"fake","version":"1"}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        if mode == "paged" and not msg.get("params", {}).get("cursor"):
            send({"jsonrpc":"2.0","id":mid,"result":{"tools":[{"name":"alpha","description":"first tool","inputSchema":{"type":"object","properties":{"x":{"type":"string"}}}}],"nextCursor":"page2"}})
        elif mode == "paged":
            send({"jsonrpc":"2.0","id":mid,"result":{"tools":[{"name":"beta","description":"second tool"}]}})
        else:
            send({"jsonrpc":"2.0","id":mid,"result":{"tools":[{"name":"echo","description":"echoes input","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}},{"name":"boom","description":"fails"}]}})
    elif method == "tools/call":
        name = msg.get("params", {}).get("name")
        args = msg.get("params", {}).get("arguments", {})
        if name == "echo":
            send({"jsonrpc":"2.0","id":mid,"result":{"content":[{"type":"text","text":"echo: " + str(args.get("text"))},{"type":"text","text":"second part"}]}})
        elif name == "boom":
            send({"jsonrpc":"2.0","id":mid,"result":{"isError":True,"content":[{"type":"text","text":"boom happened"}]}})
        elif name == "image":
            send({"jsonrpc":"2.0","id":mid,"result":{"content":[{"type":"image","data":"..."}]}})
        else:
            send({"jsonrpc":"2.0","id":mid,"error":{"code":-32601,"message":"no such tool"}})
    else:
        if mid is not None:
            send({"jsonrpc":"2.0","id":mid,"error":{"code":-32601,"message":"unknown method"}})
"#;

fn write_fake_server(dir: &Path) -> PathBuf {
    let p = dir.join("fake_mcp.py");
    std::fs::write(&p, SERVER_SCRIPT).unwrap();
    p
}

fn stdio_config(py: &str, script: &Path, mode: &str) -> McpServerConfig {
    McpServerConfig {
        kind: None,
        command: Some(py.to_string()),
        args: vec![script.to_string_lossy().to_string(), mode.to_string()],
        env: BTreeMap::new(),
        url: None,
        headers: BTreeMap::new(),
    }
}

#[tokio::test]
async fn mcp_stdio_handshake_list_and_call() {
    let py = match python_exe() {
        Some(p) => p,
        None => return,
    };
    let dir = tmpdir("stdio");
    let script = write_fake_server(&dir);

    let mut configs = BTreeMap::new();
    configs.insert("srv".to_string(), stdio_config(&py, &script, "ok"));

    let mgr = tokio::time::timeout(TEST_TIMEOUT, McpManager::start(configs, 10_000))
        .await
        .expect("start did not time out");

    let status = mgr.status();
    assert_eq!(status, vec![("srv".to_string(), "connected".to_string())]);

    let tools = mgr.tools();
    let mut names: Vec<String> = tools.iter().map(|t| t.name().to_string()).collect();
    names.sort();
    assert_eq!(names, vec!["mcp__srv__boom".to_string(), "mcp__srv__echo".to_string()]);

    let echo = tools.iter().find(|t| t.name() == "mcp__srv__echo").unwrap();
    assert_eq!(echo.description(), "echoes input");
    assert!(!echo.is_read_only());
    let schema = echo.parameters_schema();
    assert_eq!(schema["properties"]["text"]["type"], "string");

    let ctx = ToolContext::new(dir.clone(), "sess");
    let out = tokio::time::timeout(
        TEST_TIMEOUT,
        echo.call(json!({"text": "hi"}), &ctx),
    )
    .await
    .unwrap();
    assert!(!out.is_error, "got {:?}", out);
    assert_eq!(out.content, "echo: hi\nsecond part");

    let boom = tools.iter().find(|t| t.name() == "mcp__srv__boom").unwrap();
    let out = tokio::time::timeout(TEST_TIMEOUT, boom.call(json!({}), &ctx))
        .await
        .unwrap();
    assert!(out.is_error);
    assert_eq!(out.content, "boom happened");

    tokio::time::timeout(TEST_TIMEOUT, mgr.shutdown()).await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn mcp_stdio_pagination() {
    let py = match python_exe() {
        Some(p) => p,
        None => return,
    };
    let dir = tmpdir("paged");
    let script = write_fake_server(&dir);

    let mut configs = BTreeMap::new();
    configs.insert("p".to_string(), stdio_config(&py, &script, "paged"));

    let mgr = tokio::time::timeout(TEST_TIMEOUT, McpManager::start(configs, 10_000))
        .await
        .unwrap();
    assert_eq!(mgr.status()[0].1, "connected");
    let mut names: Vec<String> = mgr.tools().iter().map(|t| t.name().to_string()).collect();
    names.sort();
    assert_eq!(names, vec!["mcp__p__alpha".to_string(), "mcp__p__beta".to_string()]);
    tokio::time::timeout(TEST_TIMEOUT, mgr.shutdown()).await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn mcp_server_that_fails_to_start() {
    let mut configs = BTreeMap::new();
    configs.insert(
        "bad".to_string(),
        McpServerConfig {
            kind: None,
            command: Some("/nonexistent/definitely/not/a/binary".to_string()),
            args: vec![],
            env: BTreeMap::new(),
            url: None,
            headers: BTreeMap::new(),
        },
    );

    let mgr = tokio::time::timeout(TEST_TIMEOUT, McpManager::start(configs, 3_000))
        .await
        .unwrap();
    assert_eq!(mgr.status(), vec![("bad".to_string(), "failed".to_string())]);
    assert!(mgr.tools().is_empty());


    let py = match python_exe() {
        Some(p) => p,
        None => return,
    };
    let dir = tmpdir("exits");
    let script = dir.join("exit.py");
    std::fs::write(&script, "import sys\nsys.exit(0)\n").unwrap();
    let mut configs = BTreeMap::new();
    configs.insert("quit".to_string(), stdio_config(&py, &script, "ok"));
    let mgr = tokio::time::timeout(TEST_TIMEOUT, McpManager::start(configs, 2_000))
        .await
        .unwrap();
    assert_eq!(mgr.status(), vec![("quit".to_string(), "failed".to_string())]);
    let _ = std::fs::remove_dir_all(&dir);
}




async fn spawn_http_mock(sse: bool) -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{}/mcp", addr);
    let handle = tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(x) => x,
                Err(_) => break,
            };
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            loop {
                let n = match sock.read(&mut tmp).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                buf.extend_from_slice(&tmp[..n]);
                let s = String::from_utf8_lossy(&buf);
                if let Some(hdr_end) = s.find("\r\n\r\n") {
                    let content_len = s
                        .lines()
                        .find_map(|l| {
                            let low = l.to_ascii_lowercase();
                            low.strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if buf.len() >= hdr_end + 4 + content_len {
                        break;
                    }
                }
            }
            let req = String::from_utf8_lossy(&buf).to_string();
            let body_start = req.find("\r\n\r\n").map(|i| i + 4).unwrap_or(0);
            let body = &req[body_start..];
            let msg: Value = serde_json::from_str(body).unwrap_or(json!({}));
            let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
            let id = msg.get("id").cloned().unwrap_or(Value::Null);

            let result = match method {
                "initialize" => json!({"protocolVersion":"2025-06-18","capabilities":{}}),
                "notifications/initialized" => Value::Null,
                "tools/list" => json!({"tools":[{"name":"http_echo","description":"http echo","inputSchema":{"type":"object"}}]}),
                "tools/call" => json!({"content":[{"type":"text","text":"http-ok"}]}),
                _ => Value::Null,
            };
            let payload = if method == "notifications/initialized" {
                json!({"jsonrpc":"2.0"})
            } else {
                json!({"jsonrpc":"2.0","id":id,"result":result})
            };

            let response = if method == "notifications/initialized" {
                "HTTP/1.1 202 Accepted\r\nMcp-Session-Id: sess-123\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
            } else if sse {
                let data = serde_json::to_string(&payload).unwrap();
                let body = format!("event: message\ndata: {}\n\n", data);
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nMcp-Session-Id: sess-123\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
            } else {
                let data = serde_json::to_string(&payload).unwrap();
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: sess-123\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    data.len(),
                    data
                )
            };
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    (url, handle)
}

async fn run_http_test(sse: bool) {
    let (url, handle) = spawn_http_mock(sse).await;
    let mut configs = BTreeMap::new();
    configs.insert(
        "web".to_string(),
        McpServerConfig {
            kind: Some("http".to_string()),
            command: None,
            args: vec![],
            env: BTreeMap::new(),
            url: Some(url),
            headers: BTreeMap::new(),
        },
    );

    let mgr = tokio::time::timeout(TEST_TIMEOUT, McpManager::start(configs, 10_000))
        .await
        .unwrap();
    assert_eq!(mgr.status(), vec![("web".to_string(), "connected".to_string())]);
    let tools = mgr.tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name(), "mcp__web__http_echo");

    let dir = tmpdir("http_call");
    let ctx = ToolContext::new(dir.clone(), "sess");
    let out = tokio::time::timeout(TEST_TIMEOUT, tools[0].call(json!({}), &ctx))
        .await
        .unwrap();
    assert!(!out.is_error, "got {:?}", out);
    assert_eq!(out.content, "http-ok");

    tokio::time::timeout(TEST_TIMEOUT, mgr.shutdown()).await.unwrap();
    handle.abort();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn mcp_http_json_transport() {
    run_http_test(false).await;
}

#[tokio::test]
async fn mcp_http_sse_transport() {
    run_http_test(true).await;
}



#[test]
fn load_agents_from_dirs_and_cli() {
    let cwd = tmpdir("agents_cwd");
    let home = tmpdir("agents_home");

    std::fs::create_dir_all(home.join(".zen").join("agents")).unwrap();
    std::fs::write(
        home.join(".zen").join("agents").join("user-agent.md"),
        "---\nname: researcher\ndescription: Finds things\ntools: Read, Grep\nmodel: gpt-x\n---\nYou research.\n",
    )
    .unwrap();

    std::fs::create_dir_all(cwd.join(".zen").join("agents")).unwrap();
    std::fs::write(
        cwd.join(".zen").join("agents").join("proj.md"),
        "---\ndescription: Project helper\n---\nProject body.\n",
    )
    .unwrap();

    std::fs::write(
        cwd.join(".zen").join("agents").join("researcher.md"),
        "---\nname: researcher\ndescription: Project researcher\n---\nProject researcher body.\n",
    )
    .unwrap();

    let agents = load_agents(&cwd, Some(&home), None);
    let by_name: std::collections::HashMap<_, _> =
        agents.iter().map(|a| (a.name.clone(), a)).collect();

    assert!(by_name.contains_key("general-purpose"));
    assert_eq!(by_name["proj"].description, "Project helper");
    assert_eq!(by_name["proj"].system_prompt, "Project body.");
    assert_eq!(by_name["researcher"].system_prompt, "Project researcher body.");

    assert!(agents.iter().any(|a| a.name == "proj"));


    let only_user = load_agents(&tmpdir("empty_cwd"), Some(&home), None);
    let u = only_user.iter().find(|a| a.name == "researcher").unwrap();
    assert_eq!(u.description, "Finds things");
    assert_eq!(u.tools, Some(vec!["Read".to_string(), "Grep".to_string()]));
    assert_eq!(u.model.as_deref(), Some("gpt-x"));

    let _ = std::fs::remove_dir_all(&cwd);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn load_agents_cli_json_and_file() {
    let cwd = tmpdir("agents_cli");
    let home = tmpdir("agents_cli_home");

    let inline = r#"{"helper":{"description":"inline helper","prompt":"Do it","tools":["Read","Bash"],"model":"inherit"}}"#;
    let agents = load_agents(&cwd, Some(&home), Some(inline));
    let h = agents.iter().find(|a| a.name == "helper").unwrap();
    assert_eq!(h.description, "inline helper");
    assert_eq!(h.system_prompt, "Do it");
    assert_eq!(h.tools, Some(vec!["Read".to_string(), "Bash".to_string()]));
    assert_eq!(h.model, None);


    let file = cwd.join("agents.json");
    std::fs::write(&file, r#"{"fromfile":{"description":"d","prompt":"p"}}"#).unwrap();
    let agents = load_agents(&cwd, Some(&home), Some(&file.to_string_lossy()));
    assert!(agents.iter().any(|a| a.name == "fromfile"));


    let agents = load_agents(&cwd, Some(&home), None);
    assert_eq!(agents.iter().filter(|a| a.name == "general-purpose").count(), 1);
    let gp = agents.iter().find(|a| a.name == "general-purpose").unwrap();
    assert!(gp.tools.is_none());
    assert!(!gp.system_prompt.is_empty());

    let _ = std::fs::remove_dir_all(&cwd);
    let _ = std::fs::remove_dir_all(&home);
}



#[derive(Default)]
struct RecordingRunner {
    calls: Mutex<Vec<(String, String, String)>>,
    fail: bool,
}

#[async_trait]
impl SubAgentRunner for RecordingRunner {
    async fn run_subagent(
        &self,
        def: &AgentDefinition,
        prompt: &str,
        parent_tool_use_id: &str,
    ) -> anyhow::Result<String> {
        self.calls.lock().unwrap().push((
            def.name.clone(),
            prompt.to_string(),
            parent_tool_use_id.to_string(),
        ));
        if self.fail {
            return Err(anyhow::anyhow!("runner exploded"));
        }
        Ok(format!("[{}] {}", def.name, prompt))
    }
}

fn test_agents() -> Vec<AgentDefinition> {
    vec![
        AgentDefinition {
            name: "general-purpose".to_string(),
            description: "gp".to_string(),
            tools: None,
            model: None,
            system_prompt: "gp prompt".to_string(),
            ..Default::default()
        },
        AgentDefinition {
            name: "researcher".to_string(),
            description: "research".to_string(),
            tools: Some(vec!["Read".to_string()]),
            model: None,
            system_prompt: "research prompt".to_string(),
            ..Default::default()
        },
    ]
}

#[tokio::test]
async fn task_tool_default_type_and_parent_id() {
    let dir = tmpdir("task");
    let runner = Arc::new(RecordingRunner::default());
    let tool = TaskTool::new(test_agents(), runner.clone());
    assert_eq!(tool.name(), "Task");
    assert!(tool.is_read_only());

    let mut ctx = ToolContext::new(dir.clone(), "sess");
    ctx.current_tool_use_id = Some("call_42".to_string());

    let out = tokio::time::timeout(
        TEST_TIMEOUT,
        tool.call(json!({"description": "d", "prompt": "do the thing"}), &ctx),
    )
    .await
    .unwrap();
    assert!(!out.is_error);
    assert_eq!(out.content, "[general-purpose] do the thing");

    let calls = runner.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "general-purpose");
    assert_eq!(calls[0].2, "call_42");


    let mut ctx2 = ToolContext::new(dir.clone(), "sess");
    ctx2.current_tool_use_id = None;
    let out = tokio::time::timeout(
        TEST_TIMEOUT,
        tool.call(
            json!({"description": "d", "prompt": "p", "subagent_type": "researcher"}),
            &ctx2,
        ),
    )
    .await
    .unwrap();
    assert_eq!(out.content, "[researcher] p");
    let calls = runner.calls.lock().unwrap().clone();
    assert_eq!(calls[1].2, "task");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn task_tool_unknown_type_and_errors() {
    let dir = tmpdir("task_err");
    let runner = Arc::new(RecordingRunner::default());
    let tool = TaskTool::new(test_agents(), runner.clone());
    let ctx = ToolContext::new(dir.clone(), "sess");

    let out = tokio::time::timeout(
        TEST_TIMEOUT,
        tool.call(json!({"description": "d", "prompt": "p", "subagent_type": "nope"}), &ctx),
    )
    .await
    .unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("unknown subagent_type"));
    assert!(out.content.contains("general-purpose"));
    assert!(out.content.contains("researcher"));


    let out = tokio::time::timeout(TEST_TIMEOUT, tool.call(json!({"description": "d"}), &ctx))
        .await
        .unwrap();
    assert!(out.is_error);


    let failing = Arc::new(RecordingRunner { calls: Mutex::new(vec![]), fail: true });
    let tool2 = TaskTool::new(test_agents(), failing);
    let out = tokio::time::timeout(
        TEST_TIMEOUT,
        tool2.call(json!({"description": "d", "prompt": "p"}), &ctx),
    )
    .await
    .unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("runner exploded"));

    let _ = std::fs::remove_dir_all(&dir);
}



fn mcpcap_ctx(tag: &str) -> ToolContext {
    let dir = tmpdir(tag);
    let mut c = ToolContext::new(dir.clone(), format!("mcpcap-{}", tag));
    c.session_dir = Some(dir.join("session"));
    c.current_tool_use_id = Some(format!("toolu_{}", tag));
    c
}

fn mcpcap_spill_path(reply: &str) -> PathBuf {
    let start = reply.find("Full output saved to ").expect(reply) + "Full output saved to ".len();
    let rest = &reply[start..];
    let end = rest.find(" (lines 1-").expect(reply);
    PathBuf::from(&rest[..end])
}

#[test]
fn mcpcap_small_text_result_unchanged() {
    let c = mcpcap_ctx("small");
    let out = crate::manager::render_result(
        json!({"content":[{"type":"text","text":"hello"},{"type":"text","text":"world"}]}),
        &c,
    );
    assert!(!out.is_error);
    assert_eq!(out.content, "hello\nworld");
    assert!(!c.session_dir.as_ref().unwrap().join("tool-output").exists(), "no spill file for small output");
}

#[test]
fn mcpcap_huge_text_result_truncated_and_spilled_in_full() {
    let c = mcpcap_ctx("huge");
    let text: String = (1..=20_000).map(|i| format!("row {i}\n")).collect();
    assert!(text.chars().count() > crate::manager::MCP_MAX_CHARS);
    let out = crate::manager::render_result(json!({"content":[{"type":"text","text":text}]}), &c);
    assert!(!out.is_error);
    assert!(out.content.chars().count() < crate::manager::MCP_MAX_CHARS + 400, "preview stays near the cap");
    assert!(out.content.starts_with("row 1\nrow 2\n"), "head kept");
    assert!(out.content.ends_with("row 20000\n"), "tail kept");
    assert!(!out.content.contains("row 10000\n"), "middle only in the file");
    let path = mcpcap_spill_path(&out.content);
    assert!(path.starts_with(c.session_dir.as_ref().unwrap().join("tool-output")), "{}", path.display());
    assert_eq!(path.file_name().unwrap(), "toolu_huge.txt");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "spill file holds the full text");
}

#[test]
fn mcpcap_non_text_content_untouched_and_error_flag_kept() {
    let c = mcpcap_ctx("image");
    let img = json!({"content":[
        {"type":"image","data":"iVBORw0KGgo=","mimeType":"image/png"},
        {"type":"text","text":"caption"}
    ]});
    let out = crate::manager::render_result(img, &c);
    assert!(!out.is_error);
    assert_eq!(out.content, "[image content]\ncaption");
    assert!(!c.session_dir.as_ref().unwrap().join("tool-output").exists());


    let c2 = mcpcap_ctx("err");
    let big = "e".repeat(crate::manager::MCP_MAX_CHARS * 3);
    let out = crate::manager::render_result(json!({"isError":true,"content":[{"type":"text","text":big}]}), &c2);
    assert!(out.is_error);
    assert_eq!(std::fs::read_to_string(mcpcap_spill_path(&out.content)).unwrap(), big);
}
