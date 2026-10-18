//! MCP stdio + HTTP transports and JSON-RPC plumbing (SPEC §8).

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::config::McpServerConfig;

const PROTOCOL_VERSION: &str = "2025-06-18";

/// A JSON-RPC response payload, routed by id.
type Pending = Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Value>>>>;

pub(crate) struct StdioTransport {
    writer: tokio::sync::Mutex<tokio::process::ChildStdin>,
    pending: Pending,
    next_id: AtomicU64,
    child: Mutex<Option<tokio::process::Child>>,
    /// PID of the spawned server; on Unix it leads its own process group (pgid == pid),
    /// so `kill()` can take down grandchildren holding the stdout pipe too.
    child_pid: Mutex<Option<u32>>,
    configured_timeout_ms: AtomicU64,
}

impl StdioTransport {
    pub(crate) fn spawn(cfg: &McpServerConfig, timeout_ms: u64) -> anyhow::Result<Arc<Self>> {
        let command = cfg
            .command
            .clone()
            .ok_or_else(|| anyhow::anyhow!("stdio server has no command"))?;
        let mut cmd = tokio::process::Command::new(&command);
        cmd.args(&cfg.args);
        if !cfg.env.is_empty() {
            cmd.envs(&cfg.env);
        }
        // MEDIUM-1: the API key is only needed in-process; MCP servers inherit the
        // parent environment, so scrub the well-known key var names. (A custom
        // `apiKeyEnv` name is not visible here; it is scrubbed at the Bash/hook
        // spawn sites where the configured name is known.)
        for name in zen_core::api_key_env_names(None) {
            cmd.env_remove(name);
        }
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::null());
        cmd.kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("failed to spawn '{}': {}", command, e))?;
        let child_pid = child.id();
        let stdin = child.stdin.take().ok_or_else(|| anyhow::anyhow!("no stdin pipe"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow::anyhow!("no stdout pipe"))?;

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let reader_pending = pending.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                    {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
                            // Ignore notifications/requests from the server (no matching id).
                            if let Some(id) = val.get("id").and_then(id_to_u64) {
                                let tx = reader_pending.lock().unwrap().remove(&id);
                                if let Some(tx) = tx {
                                    let _ = tx.send(val);
                                }
                            }
                        }
                    }
            }
            // Process ended: fail every outstanding request so callers do not hang.
            let mut map = reader_pending.lock().unwrap();
            for (_, tx) in map.drain() {
                let _ = tx.send(json!({"jsonrpc":"2.0","error":{"code":-32000,"message":"MCP server closed"}}));
            }
        });

        Ok(Arc::new(Self {
            writer: tokio::sync::Mutex::new(stdin),
            pending,
            next_id: AtomicU64::new(1),
            child: Mutex::new(Some(child)),
            child_pid: Mutex::new(child_pid),
            configured_timeout_ms: AtomicU64::new(timeout_ms),
        }))
    }

    pub(crate) async fn request(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let msg = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);

        {
            let mut w = self.writer.lock().await;
            let mut line = serde_json::to_string(&msg)?;
            line.push('\n');
            if let Err(e) = w.write_all(line.as_bytes()).await {
                self.pending.lock().unwrap().remove(&id);
                return Err(anyhow::anyhow!("write to MCP server failed: {}", e));
            }
            if let Err(e) = w.flush().await {
                self.pending.lock().unwrap().remove(&id);
                return Err(anyhow::anyhow!("flush to MCP server failed: {}", e));
            }
        }

        let timeout_ms = self.configured_timeout_ms.load(Ordering::SeqCst).max(1);
        let val = match tokio::time::timeout(Duration::from_millis(timeout_ms), rx).await {
            Ok(Ok(v)) => v,
            Ok(Err(_)) => return Err(anyhow::anyhow!("MCP server dropped response")),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                return Err(anyhow::anyhow!("MCP request '{}' timed out", method));
            }
        };
        if let Some(err) = val.get("error") {
            return Err(anyhow::anyhow!("MCP error: {}", err));
        }
        Ok(val.get("result").cloned().unwrap_or(Value::Null))
    }

    pub(crate) async fn notify(&self, method: &str, params: Value) -> anyhow::Result<()> {
        let msg = json!({"jsonrpc":"2.0","method":method,"params":params});
        let mut w = self.writer.lock().await;
        let mut line = serde_json::to_string(&msg)?;
        line.push('\n');
        w.write_all(line.as_bytes()).await?;
        w.flush().await?;
        Ok(())
    }

    pub(crate) fn kill(&self) {
        // Kill the whole process group, not just the direct child: a grandchild holding
        // the stdout pipe would otherwise keep the reader task's `next_line()` pending
        // forever after the server itself is dead.
        let pid = self.child_pid.lock().ok().and_then(|mut g| g.take());
        if let Ok(mut guard) = self.child.lock() {
            if let Some(mut child) = guard.take() {
                let _ = child.start_kill();
            }
        }
        kill_process_group(pid);
    }
}

/// Best-effort kill of a process group led by `pid` (mirrors `kill_tree` in zen-tools).
fn kill_process_group(pid: Option<u32>) {
    let Some(pid) = pid else { return };
    #[cfg(unix)]
    {
        // The server leads its own process group (pgid == pid).
        let _ = std::process::Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", pid)])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

pub(crate) struct HttpTransport {
    client: reqwest::Client,
    url: String,
    headers: Vec<(String, String)>,
    session_id: Mutex<Option<String>>,
    next_id: AtomicU64,
    timeout_ms: u64,
}

/// Add the PEM roots from `SSL_CERT_FILE` (see `zen_core::extra_root_pem`).
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

impl HttpTransport {
    pub(crate) fn new(cfg: &McpServerConfig, timeout_ms: u64) -> anyhow::Result<Arc<Self>> {
        let url = cfg
            .url
            .clone()
            .ok_or_else(|| anyhow::anyhow!("http server has no url"))?;
        let mut builder = with_extra_roots(reqwest::Client::builder()
            .timeout(Duration::from_millis(timeout_ms.max(1))));
        // Local MCP servers must not be sent through an HTTP(S) proxy from the environment.
        if url_is_loopback(&url) {
            builder = builder.no_proxy();
        }
        let client = builder.build()?;
        Ok(Arc::new(Self {
            client,
            url,
            headers: cfg.headers.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            session_id: Mutex::new(None),
            next_id: AtomicU64::new(1),
            timeout_ms,
        }))
    }

    fn build(&self, msg: &Value) -> reqwest::RequestBuilder {
        let mut rb = self
            .client
            .post(&self.url)
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .json(msg);
        for (k, v) in &self.headers {
            rb = rb.header(k.as_str(), v.as_str());
        }
        if let Some(sid) = self.session_id.lock().unwrap().clone() {
            rb = rb.header("Mcp-Session-Id", sid);
        }
        rb
    }

    fn capture_session(&self, resp: &reqwest::Response) {
        if let Some(sid) = resp.headers().get("Mcp-Session-Id") {
            if let Ok(s) = sid.to_str() {
                *self.session_id.lock().unwrap() = Some(s.to_string());
            }
        }
    }

    pub(crate) async fn request(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let msg = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let resp = self.build(&msg).send().await?;
        self.capture_session(&resp);
        let status = resp.status();
        if !status.is_success() {
            return Err(anyhow::anyhow!("MCP HTTP {} for '{}'", status, method));
        }
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = resp.text().await?;

        let val = if content_type.contains("text/event-stream") || body.trim_start().starts_with("data:") {
            parse_sse_for_id(&body, id)
                .ok_or_else(|| anyhow::anyhow!("MCP SSE response carried no payload for id {}", id))?
        } else {
            serde_json::from_str::<Value>(&body)
                .map_err(|e| anyhow::anyhow!("invalid MCP JSON response: {}", e))?
        };
        if let Some(err) = val.get("error") {
            return Err(anyhow::anyhow!("MCP error: {}", err));
        }
        Ok(val.get("result").cloned().unwrap_or(Value::Null))
    }

    pub(crate) async fn notify(&self, method: &str, params: Value) -> anyhow::Result<()> {
        let msg = json!({"jsonrpc":"2.0","method":method,"params":params});
        let resp = self.build(&msg).send().await?;
        self.capture_session(&resp);
        let _ = self.timeout_ms;
        Ok(())
    }
}

/// Find the JSON-RPC object with the matching id inside an SSE body.
fn parse_sse_for_id(body: &str, id: u64) -> Option<Value> {
    let mut last: Option<Value> = None;
    for line in body.lines() {
        let line = line.trim_start();
        let data = match line.strip_prefix("data:") {
            Some(d) => d.trim(),
            None => continue,
        };
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        if let Ok(val) = serde_json::from_str::<Value>(data) {
            let matches = val.get("id").and_then(id_to_u64) == Some(id);
            if matches || val.get("id").is_none() {
                last = Some(val);
            }
        }
    }
    last
}

fn id_to_u64(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// True for localhost / 127.0.0.0/8 / [::1] URLs, which should bypass any environment proxy.
fn url_is_loopback(url: &str) -> bool {
    let rest = match url.split_once("://") {
        Some((_, r)) => r,
        None => url,
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = match authority.rsplit_once('@') {
        Some((_, h)) => h,
        None => authority,
    };
    let host = if let Some(stripped) = host.strip_prefix('[') {
        stripped.split(']').next().unwrap_or(stripped)
    } else {
        host.split(':').next().unwrap_or(host)
    };
    host.eq_ignore_ascii_case("localhost")
        || host == "::1"
        || host.starts_with("127.")
        || host == "0.0.0.0"
}

/// Shared JSON-RPC client over either transport.
pub(crate) enum Transport {
    Stdio(Arc<StdioTransport>),
    Http(Arc<HttpTransport>),
}

impl Transport {
    pub(crate) fn from_config(cfg: &McpServerConfig, timeout_ms: u64) -> anyhow::Result<Self> {
        if cfg.is_http() {
            Ok(Transport::Http(HttpTransport::new(cfg, timeout_ms)?))
        } else {
            Ok(Transport::Stdio(StdioTransport::spawn(cfg, timeout_ms)?))
        }
    }

    pub(crate) async fn request(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        match self {
            Transport::Stdio(t) => t.request(method, params).await,
            Transport::Http(t) => t.request(method, params).await,
        }
    }

    pub(crate) async fn notify(&self, method: &str, params: Value) -> anyhow::Result<()> {
        match self {
            Transport::Stdio(t) => t.notify(method, params).await,
            Transport::Http(t) => t.notify(method, params).await,
        }
    }

    pub(crate) fn kill(&self) {
        if let Transport::Stdio(t) = self {
            t.kill();
        }
    }
}

/// Perform the MCP handshake and return the server's tool list.
pub(crate) async fn handshake(transport: &Transport) -> anyhow::Result<Vec<RemoteTool>> {
    let init_params = json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": {"name": "zen-code", "version": env!("CARGO_PKG_VERSION")}
    });
    let _ = transport.request("initialize", init_params).await.map_err(|e| {
        anyhow::anyhow!("initialize failed: {}", e)
    })?;
    transport
        .notify("notifications/initialized", json!({}))
        .await
        .map_err(|e| anyhow::anyhow!("initialized notification failed: {}", e))?;
    list_tools(transport).await
}

pub(crate) async fn list_tools(transport: &Transport) -> anyhow::Result<Vec<RemoteTool>> {
    let mut tools = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let params = match &cursor {
            Some(c) => json!({"cursor": c}),
            None => json!({}),
        };
        let result = transport
            .request("tools/list", params)
            .await
            .map_err(|e| anyhow::anyhow!("tools/list failed: {}", e))?;
        if let Some(arr) = result.get("tools").and_then(|v| v.as_array()) {
            for t in arr {
                let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if name.is_empty() {
                    continue;
                }
                let description = t
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let schema = t
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object"}));
                tools.push(RemoteTool { name, description, schema });
            }
        }
        cursor = result
            .get("nextCursor")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if cursor.is_none() {
            break;
        }
    }
    Ok(tools)
}

#[derive(Debug, Clone)]
pub(crate) struct RemoteTool {
    pub name: String,
    pub description: String,
    pub schema: Value,
}
