



























#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;


pub const TEST_TIMEOUT: Duration = Duration::from_secs(60);







pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "zen_e2e_{}_{}_{}",
            tag,
            std::process::id(),
            n
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }


    pub fn subdir(&self, rel: &str) -> PathBuf {
        let p = self.path.join(rel);
        std::fs::create_dir_all(&p).expect("create subdir");
        p
    }


    pub fn write(&self, rel: &str, contents: &str) -> PathBuf {
        let p = self.path.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(&p, contents).expect("write file");
        p
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}







pub struct Reply {
    pub status: u16,
    pub body: String,

    pub abrupt: bool,
}

impl Reply {

    pub fn text(content: &str) -> Self {
        Self::with_tool_calls(content, None)
    }


    pub fn with_tool_calls(content: &str, tool_calls: Option<serde_json::Value>) -> Self {
        let mut message = serde_json::json!({"role": "assistant", "content": content});
        let finish_reason = if tool_calls.is_some() { "tool_calls" } else { "stop" };
        if let Some(tc) = tool_calls {
            message["tool_calls"] = tc;


            if content.is_empty() {
                message["content"] = serde_json::Value::Null;
            }
        }
        let body = serde_json::json!({
            "id": "chatcmpl-mock",
            "object": "chat.completion",
            "created": 0,
            "model": "mock-model",
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": finish_reason
            }],
            "usage": {"prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10}
        })
        .to_string();
        Self { status: 200, body, abrupt: false }
    }


    pub fn status(status: u16, message: &str) -> Self {
        Self {
            status,
            body: serde_json::json!({"error": {"message": message}}).to_string(),
            abrupt: false,
        }
    }


    pub fn malformed() -> Self {
        Self { status: 200, body: "<html>not json</html>".to_string(), abrupt: false }
    }


    pub fn abrupt() -> Self {
        Self { status: 200, body: String::new(), abrupt: true }
    }
}


struct MockState {
    replies: Vec<Reply>,
    requests: Vec<String>,

    overflow: usize,
}






pub struct MockServer {
    addr: SocketAddr,
    state: Arc<Mutex<MockState>>,
    handle: tokio::task::JoinHandle<()>,
}

impl MockServer {

    pub async fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(MockState {
            replies,
            requests: Vec::new(),
            overflow: 0,
        }));
        let st = state.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                let req = read_request(&mut sock).await;
                let selected = {
                    let mut g = st.lock().await;
                    g.requests.push(req);
                    let idx = g.requests.len() - 1;
                    if idx >= g.replies.len() {


                        g.overflow += 1;
                    }
                    let i = idx.min(g.replies.len().saturating_sub(1));
                    ReplySnapshot::from(&g.replies[i])
                };
                if selected.abrupt {
                    let _ = sock.shutdown().await;
                    continue;
                }
                let resp = format!(
                    "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    selected.status,
                    status_text(selected.status),
                    selected.body.len(),
                    selected.body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        Self { addr, state, handle }
    }


    pub async fn single_text(text: &str) -> Self {
        Self::start(vec![Reply::text(text)]).await
    }


    pub fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }


    pub async fn requests(&self) -> Vec<String> {
        self.state.lock().await.requests.clone()
    }


    pub async fn overflow(&self) -> usize {
        self.state.lock().await.overflow
    }


    pub async fn finish(self) -> Vec<String> {
        self.handle.abort();
        let g = self.state.lock().await;
        g.requests.clone()
    }
}


struct ReplySnapshot {
    status: u16,
    body: String,
    abrupt: bool,
}

impl From<&Reply> for ReplySnapshot {
    fn from(r: &Reply) -> Self {
        Self { status: r.status, body: r.body.clone(), abrupt: r.abrupt }
    }
}

fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}





pub async fn read_request(sock: &mut tokio::net::TcpStream) -> String {
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
    String::from_utf8_lossy(&buf).to_string()
}


pub fn request_json(raw: &str) -> serde_json::Value {
    let body = raw.split("\r\n\r\n").nth(1).unwrap_or("");
    serde_json::from_str(body).unwrap_or_else(|e| {
        panic!("request body is not JSON ({}):\n{}", e, raw)
    })
}


pub fn request_messages(raw: &str) -> Vec<serde_json::Value> {
    request_json(raw)["messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}


pub fn request_messages_text(raw: &str) -> String {
    serde_json::to_string(&request_messages(raw)).unwrap_or_default()
}


pub fn system_message(raw: &str) -> String {
    request_messages(raw)
        .into_iter()
        .filter(|m| m["role"] == "system")
        .map(|m| {
            m["content"]
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| m["content"].to_string())
        })
        .collect::<Vec<_>>()
        .join("\n")
}


pub fn request_tool_names(raw: &str) -> Vec<String> {
    request_json(raw)["tools"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| {
                    t["function"]["name"]
                        .as_str()
                        .map(|s| s.to_string())
                        .or_else(|| t["name"].as_str().map(|s| s.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}







pub struct ZenRun {
    args: Vec<String>,
    cwd: PathBuf,
    home: PathBuf,
    stdin: Option<String>,
    env_extra: Vec<(String, Option<String>)>,
    timeout: Duration,
}

impl ZenRun {
    pub fn new(_tag: &str, home: &TempDir, cwd: &TempDir) -> Self {
        Self {
            args: Vec::new(),
            cwd: cwd.path().to_path_buf(),
            home: home.path().to_path_buf(),
            stdin: None,
            env_extra: Vec::new(),
            timeout: TEST_TIMEOUT,
        }
    }

    pub fn arg(mut self, a: impl Into<String>) -> Self {
        self.args.push(a.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for a in args {
            self.args.push(a.into());
        }
        self
    }


    pub fn stdin(mut self, text: impl Into<String>) -> Self {
        self.stdin = Some(text.into());
        self
    }


    pub fn env(mut self, key: &str, value: Option<&str>) -> Self {
        self.env_extra.push((key.to_string(), value.map(|v| v.to_string())));
        self
    }

    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = d;
        self
    }


    pub fn mock(self, model: &str, server: &MockServer) -> Self {
        self.arg("--model").arg(model).arg("--base-url").arg(server.base_url())
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(zen_bin());
        cmd.current_dir(&self.cwd)
            .env("HOME", &self.home)

            .env("USERPROFILE", &self.home)
            .env("ZEN_API_KEY", "dummy")
            .env_remove("ZEN_MODEL")
            .env_remove("ZEN_BASE_URL")
            .env_remove("OPENAI_API_KEY")
            .env_remove("ZEN_SETTINGS")


            .env("ZEN_TRUST_FOLDER", "1")
            .stdin(if self.stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in &self.env_extra {
            match v {
                Some(v) => cmd.env(k, v),
                None => cmd.env_remove(k),
            };
        }
        cmd.args(&self.args);
        cmd
    }


    pub fn spawn(self) -> std::process::Child {
        let mut cmd = self.command();
        cmd.stdin(Stdio::null());
        cmd.spawn().expect("failed to spawn zen binary")
    }


    pub fn spawn_piped(self) -> std::process::Child {
        let mut cmd = self.command();
        cmd.stdin(Stdio::piped());
        cmd.spawn().expect("failed to spawn zen binary")
    }


    pub fn run(self) -> Output {
        let mut cmd = self.command();
        let mut child = cmd.spawn().expect("failed to spawn zen binary");

        if let Some(text) = &self.stdin {
            use std::io::Write;
            if let Some(mut si) = child.stdin.take() {
                let _ = si.write_all(text.as_bytes());
                drop(si);
            }
        }

        let deadline = std::time::Instant::now() + self.timeout;
        loop {
            match child.try_wait().expect("try_wait") {
                Some(_) => break,
                None => {
                    if std::time::Instant::now() > deadline {
                        let _ = child.kill();
                        let out = child.wait_with_output().expect("wait after kill");
                        panic!(
                            "zen subprocess exceeded {:?}\nargs: {:?}\nstdout: {}\nstderr: {}",
                            self.timeout,
                            self.args,
                            String::from_utf8_lossy(&out.stdout),
                            String::from_utf8_lossy(&out.stderr)
                        );
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
        child.wait_with_output().expect("wait_with_output")
    }


    pub fn run_output(self) -> (String, String, i32) {
        let out = self.run();
        (
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
            out.status.code().unwrap_or(-1),
        )
    }
}


pub fn zen_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_zen"))
}




pub fn ndjson(stdout: &str) -> Vec<serde_json::Value> {
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str::<serde_json::Value>(l)
                .unwrap_or_else(|e| panic!("stdout line is not JSON ({}):\n{}", e, stdout))
        })
        .collect()
}


pub fn last_result(stdout: &str) -> serde_json::Value {
    let lines = ndjson(stdout);
    let last = lines.last().unwrap_or_else(|| panic!("no output lines:\n{}", stdout));
    assert_eq!(last["type"], "result", "last line is not a result:\n{}", stdout);
    last.clone()
}





pub fn assert_fields(v: &serde_json::Value, expected: &[&str], what: &str) {
    let obj = v.as_object().unwrap_or_else(|| panic!("{} is not an object: {}", what, v));
    let mut got: Vec<&str> = obj.keys().map(|s| s.as_str()).collect();
    got.sort_unstable();
    let mut want: Vec<&str> = expected.to_vec();
    want.sort_unstable();
    assert_eq!(got, want, "{} has unexpected field set:\n{}", what, v);
}


pub fn find_line<'a>(
    lines: &'a [serde_json::Value],
    ty: &str,
    subtype: Option<&str>,
) -> Option<&'a serde_json::Value> {
    lines.iter().find(|l| {
        l["type"] == ty && subtype.map(|s| l["subtype"] == s).unwrap_or(true)
    })
}


pub fn write_settings(root: &Path, name: &str, json: &str) -> PathBuf {
    let p = root.join(".zen").join(name);
    std::fs::create_dir_all(p.parent().unwrap()).expect("settings dir");
    std::fs::write(&p, json).expect("write settings");
    p
}
