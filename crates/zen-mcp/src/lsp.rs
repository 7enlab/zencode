







use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};


const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(30);

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);


type Pending = Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Value>>>>;




#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LspLanguage {
    Rust,
    Python,
    TypeScript,
}


pub struct LspServerSpec {
    pub command: String,
    pub args: Vec<String>,
    pub install_hint: &'static str,

    pub markers: &'static [&'static str],
}


pub fn detect_language(path: &Path) -> Option<LspLanguage> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())?;
    match ext.as_str() {
        "rs" => Some(LspLanguage::Rust),
        "py" => Some(LspLanguage::Python),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => Some(LspLanguage::TypeScript),
        _ => None,
    }
}


pub fn server_spec(lang: LspLanguage) -> LspServerSpec {
    match lang {
        LspLanguage::Rust => LspServerSpec {
            command: "rust-analyzer".to_string(),
            args: Vec::new(),
            install_hint: "rustup component add rust-analyzer",
            markers: &["Cargo.toml"],
        },
        LspLanguage::Python => LspServerSpec {
            command: "pyright-langserver".to_string(),
            args: vec!["--stdio".to_string()],
            install_hint: "npm install -g pyright",
            markers: &["pyproject.toml", "setup.py", "setup.cfg"],
        },
        LspLanguage::TypeScript => LspServerSpec {
            command: "typescript-language-server".to_string(),
            args: vec!["--stdio".to_string()],
            install_hint: "npm install -g typescript-language-server",
            markers: &["tsconfig.json", "package.json"],
        },
    }
}


fn language_id_for(path: &Path, lang: LspLanguage) -> &'static str {
    match lang {
        LspLanguage::Rust => "rust",
        LspLanguage::Python => "python",
        LspLanguage::TypeScript => {
            match path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
                .as_deref()
            {
                Some("ts") | Some("tsx") => "typescript",
                _ => "javascript",
            }
        }
    }
}



pub fn find_project_root(file: &Path, lang: LspLanguage) -> PathBuf {
    let spec = server_spec(lang);
    let start = file.parent().map(Path::to_path_buf);
    let mut dir = start.clone();
    while let Some(d) = dir {
        if spec.markers.iter().any(|m| d.join(m).is_file()) {
            return d;
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    start.unwrap_or_else(|| PathBuf::from("."))
}




pub fn find_binary(name: &str) -> Option<PathBuf> {
    let direct = PathBuf::from(name);
    if direct.is_absolute() {
        return direct.is_file().then_some(direct);
    }
    #[cfg(windows)]
    let candidates: Vec<String> = {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".exe") || lower.ends_with(".cmd") || lower.ends_with(".bat") {
            vec![name.to_string()]
        } else {
            vec![
                format!("{name}.exe"),
                format!("{name}.cmd"),
                format!("{name}.bat"),
                name.to_string(),
            ]
        }
    };
    #[cfg(not(windows))]
    let candidates = [name.to_string()];
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for cand in &candidates {
            let full = dir.join(cand);
            if full.is_file() {
                return Some(full);
            }
        }
    }
    None
}




fn encode_uri_path(s: &str, out: &mut String) {
    for &b in s.as_bytes() {
        let c = b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
}


pub fn path_to_uri(path: &Path) -> String {
    #[cfg(windows)]
    {
        let s = path.to_string_lossy().replace('\\', "/");
        let mut out = String::from("file://");
        if !s.starts_with('/') {
            out.push('/');
        }
        encode_uri_path(&s, &mut out);
        out
    }
    #[cfg(not(windows))]
    {
        let mut out = String::from("file://");
        encode_uri_path(&path.to_string_lossy(), &mut out);
        out
    }
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}


pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let mut bytes = Vec::with_capacity(rest.len());
    let mut it = rest.as_bytes().iter();
    while let Some(&b) = it.next() {
        if b == b'%' {
            let h = hex_val(*it.next()?)?;
            let l = hex_val(*it.next()?)?;
            bytes.push((h << 4) | l);
        } else {
            bytes.push(b);
        }
    }
    let s = String::from_utf8(bytes).ok()?;
    #[cfg(windows)]
    {

        let s = s.strip_prefix('/').unwrap_or(&s).replace('/', "\\");
        Some(PathBuf::from(s))
    }
    #[cfg(not(windows))]
    {
        Some(PathBuf::from(s))
    }
}







pub(crate) fn col_to_utf16(line: &str, col1: usize) -> u32 {
    let mut units: u32 = 0;
    for (i, c) in line.chars().enumerate() {
        if i + 1 >= col1 {
            break;
        }
        units += c.len_utf16() as u32;
    }
    units
}


pub(crate) fn utf16_to_col(line: &str, utf16: u32) -> usize {
    let mut units: u32 = 0;
    let mut col: usize = 1;
    for c in line.chars() {
        if units >= utf16 {
            break;
        }
        units += c.len_utf16() as u32;
        col += 1;
    }
    col
}




pub(crate) async fn write_frame<W>(w: &mut W, msg: &Value) -> anyhow::Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    let body = serde_json::to_vec(msg)?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    w.write_all(header.as_bytes()).await?;
    w.write_all(&body).await?;
    w.flush().await?;
    Ok(())
}


pub(crate) async fn read_frame<R>(r: &mut R) -> anyhow::Result<Value>
where
    R: AsyncBufReadExt + Unpin,
{
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = r.read_line(&mut line).await?;
        if n == 0 {
            anyhow::bail!("LSP server closed the connection");
        }
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().ok();
            }
        }
    }
    let len =
        content_length.ok_or_else(|| anyhow::anyhow!("LSP frame without Content-Length"))?;
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    Ok(serde_json::from_slice(&body)?)
}

fn id_to_u64(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}




pub(crate) struct LspClient {
    writer: tokio::sync::Mutex<tokio::process::ChildStdin>,
    pending: Pending,
    next_id: AtomicU64,
    child: Mutex<Option<tokio::process::Child>>,


    child_pid: Mutex<Option<u32>>,
}

impl LspClient {
    pub(crate) fn spawn(command: &str, args: &[String]) -> anyhow::Result<Arc<Self>> {
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args);



        for name in ["ZEN_API_KEY", "OPENAI_API_KEY"] {
            cmd.env_remove(name);
        }
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::null());
        cmd.kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow::anyhow!("language server '{command}' not found on PATH")
            } else {
                anyhow::anyhow!("failed to spawn '{command}': {e}")
            }
        })?;
        let child_pid = child.id();
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("no stdin pipe"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("no stdout pipe"))?;

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let reader_pending = pending.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            while let Ok(val) = read_frame(&mut reader).await {

                if let Some(id) = val.get("id").and_then(id_to_u64) {
                    let tx = reader_pending.lock().unwrap().remove(&id);
                    if let Some(tx) = tx {
                        let _ = tx.send(val);
                    }
                }
            }

            let mut map = reader_pending.lock().unwrap();
            for (_, tx) in map.drain() {
                let _ = tx.send(
                    json!({"jsonrpc":"2.0","error":{"code":-32000,"message":"LSP server closed"}}),
                );
            }
        });

        Ok(Arc::new(Self {
            writer: tokio::sync::Mutex::new(stdin),
            pending,
            next_id: AtomicU64::new(1),
            child: Mutex::new(Some(child)),
            child_pid: Mutex::new(child_pid),
        }))
    }



    pub(crate) async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> anyhow::Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let msg = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        {
            let mut w = self.writer.lock().await;
            if let Err(e) = write_frame(&mut *w, &msg).await {
                self.pending.lock().unwrap().remove(&id);
                return Err(anyhow::anyhow!("write to LSP server failed: {e}"));
            }
        }
        let val = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(v)) => v,
            Ok(Err(_)) => return Err(anyhow::anyhow!("LSP server dropped the response")),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                return Err(anyhow::anyhow!(
                    "LSP request '{method}' timed out after {}s",
                    timeout.as_secs()
                ));
            }
        };
        if let Some(err) = val.get("error") {
            return Err(anyhow::anyhow!("LSP error: {err}"));
        }
        Ok(val.get("result").cloned().unwrap_or(Value::Null))
    }

    pub(crate) async fn notify(&self, method: &str, params: Value) -> anyhow::Result<()> {
        let msg = json!({"jsonrpc":"2.0","method":method,"params":params});
        let mut w = self.writer.lock().await;
        write_frame(&mut *w, &msg)
            .await
            .map_err(|e| anyhow::anyhow!("LSP notify '{method}' failed: {e}"))
    }


    pub(crate) async fn initialize(&self, root: &Path) -> anyhow::Result<()> {
        let params = json!({
            "processId": std::process::id(),
            "rootUri": path_to_uri(root),
            "capabilities": {
                "textDocument": {
                    "synchronization": {"dynamicRegistration": false},
                    "definition": {"dynamicRegistration": false, "linkSupport": true},
                    "hover": {"dynamicRegistration": false, "contentFormat": ["markdown", "plaintext"]},
                    "references": {"dynamicRegistration": false},
                }
            },
        });
        self.request("initialize", params, INITIALIZE_TIMEOUT)
            .await
            .map_err(|e| anyhow::anyhow!("LSP initialize failed: {e}"))?;
        self.notify("initialized", json!({}))
            .await
            .map_err(|e| anyhow::anyhow!("LSP initialized notification failed: {e}"))?;
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn kill(&self) {
        let pid = self.child_pid.lock().ok().and_then(|mut g| g.take());
        if let Ok(mut guard) = self.child.lock() {
            if let Some(mut child) = guard.take() {
                let _ = child.start_kill();
            }
        }
        kill_process_group(pid);
    }
}


fn kill_process_group(pid: Option<u32>) {
    let Some(pid) = pid else { return };
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .args(["-KILL", "--", &format!("-{pid}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}




struct OpenDoc {
    version: i32,
    mtime: SystemTime,
    text: String,
}



pub(crate) struct LspConnection {
    client: Arc<LspClient>,
    language: LspLanguage,
    docs: Mutex<HashMap<PathBuf, OpenDoc>>,
}

impl LspConnection {
    pub(crate) async fn start(
        lang: LspLanguage,
        command: &str,
        args: &[String],
        root: &Path,
    ) -> anyhow::Result<Arc<Self>> {
        let client = LspClient::spawn(command, args)?;
        client.initialize(root).await?;
        Ok(Arc::new(Self {
            client,
            language: lang,
            docs: Mutex::new(HashMap::new()),
        }))
    }




    async fn ensure_open(&self, path: &Path) -> anyhow::Result<String> {
        let mtime = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .map_err(|e| anyhow::anyhow!("cannot stat '{}': {e}", path.display()))?;
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("cannot read '{}': {e}", path.display()))?;
        let uri = path_to_uri(path);
        let lang_id = language_id_for(path, self.language);

        enum Action {
            None,
            Open,
            Change(i32),
        }
        let (action, current) = {
            let mut docs = self.docs.lock().unwrap();
            match docs.get_mut(path) {
                Some(doc) if doc.mtime == mtime => (Action::None, doc.text.clone()),
                Some(doc) => {
                    doc.version += 1;
                    doc.mtime = mtime;
                    doc.text.clone_from(&text);
                    (Action::Change(doc.version), text.clone())
                }
                None => {
                    docs.insert(
                        path.to_path_buf(),
                        OpenDoc { version: 1, mtime, text: text.clone() },
                    );
                    (Action::Open, text.clone())
                }
            }
        };
        match action {
            Action::None => {}
            Action::Open => {
                self.client
                    .notify(
                        "textDocument/didOpen",
                        json!({"textDocument": {"uri": uri, "languageId": lang_id, "version": 1, "text": text}}),
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!("textDocument/didOpen failed: {e}"))?;
            }
            Action::Change(version) => {
                self.client
                    .notify(
                        "textDocument/didChange",
                        json!({"textDocument": {"uri": uri, "version": version},
                               "contentChanges": [{"text": text}]}),
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!("textDocument/didChange failed: {e}"))?;
            }
        }
        Ok(current)
    }


    fn position(&self, text: &str, line1: usize, col1: usize) -> anyhow::Result<Value> {
        let line0 = line1
            .checked_sub(1)
            .ok_or_else(|| anyhow::anyhow!("line must be >= 1, got {line1}"))?;
        let line_text = text.lines().nth(line0).ok_or_else(|| {
            anyhow::anyhow!(
                "line {line1} out of range (file has {} lines)",
                text.lines().count()
            )
        })?;
        Ok(json!({"line": line0, "character": col_to_utf16(line_text, col1)}))
    }

    async fn text_document_request(
        &self,
        method: &str,
        path: &Path,
        line1: usize,
        col1: usize,
        extra: Value,
    ) -> anyhow::Result<Value> {
        let text = self.ensure_open(path).await?;
        let position = self.position(&text, line1, col1)?;
        let mut params =
            json!({"textDocument": {"uri": path_to_uri(path)}, "position": position});
        if let Value::Object(map) = extra {
            if let Value::Object(p) = &mut params {
                p.extend(map);
            }
        }
        self.client
            .request(method, params, REQUEST_TIMEOUT)
            .await
    }

    pub(crate) async fn definition(
        &self,
        path: &Path,
        line1: usize,
        col1: usize,
    ) -> anyhow::Result<Value> {
        self.text_document_request("textDocument/definition", path, line1, col1, json!({}))
            .await
    }

    pub(crate) async fn hover(
        &self,
        path: &Path,
        line1: usize,
        col1: usize,
    ) -> anyhow::Result<Value> {
        self.text_document_request("textDocument/hover", path, line1, col1, json!({}))
            .await
    }

    pub(crate) async fn references(
        &self,
        path: &Path,
        line1: usize,
        col1: usize,
        include_declaration: bool,
    ) -> anyhow::Result<Value> {
        self.text_document_request(
            "textDocument/references",
            path,
            line1,
            col1,
            json!({"context": {"includeDeclaration": include_declaration}}),
        )
        .await
    }
}





#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LspLocation {
    pub uri: String,
    pub path: String,
    pub line: usize,
    pub col: usize,
    pub end_line: usize,
    pub end_col: usize,
}




pub fn parse_locations(result: &Value) -> Vec<LspLocation> {
    let mut out = Vec::new();
    match result {
        Value::Null => {}
        Value::Array(items) => {
            for item in items {
                if let Some(loc) = parse_location_like(item) {
                    out.push(loc);
                }
            }
        }
        obj @ Value::Object(_) => {
            if let Some(loc) = parse_location_like(obj) {
                out.push(loc);
            }
        }
        _ => {}
    }
    out
}

fn parse_location_like(v: &Value) -> Option<LspLocation> {
    let (uri, range) = if let Some(u) = v.get("targetUri").and_then(Value::as_str) {
        (u, v.get("targetRange")?)
    } else {
        (v.get("uri")?.as_str()?, v.get("range")?)
    };
    let (sl, sc) = range_pos(range, "start")?;
    let (el, ec) = range_pos(range, "end")?;
    let path = uri_to_path(uri)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| uri.to_string());
    Some(LspLocation {
        uri: uri.to_string(),
        path: path.clone(),
        line: sl + 1,
        col: display_col(&path, sl, sc),
        end_line: el + 1,
        end_col: display_col(&path, el, ec),
    })
}

fn range_pos(range: &Value, key: &str) -> Option<(usize, u32)> {
    let p = range.get(key)?;
    Some((p.get("line")?.as_u64()? as usize, p.get("character")?.as_u64()? as u32))
}




fn display_col(path: &str, line0: usize, utf16: u32) -> usize {
    if let Ok(text) = std::fs::read_to_string(path) {
        if let Some(line) = text.lines().nth(line0) {
            return utf16_to_col(line, utf16);
        }
    }
    (utf16 + 1) as usize
}




pub fn render_hover(result: &Value) -> String {
    match result.get("contents").unwrap_or(&Value::Null) {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(render_hover_part)
            .collect::<Vec<_>>()
            .join("\n\n"),
        obj @ Value::Object(_) => render_hover_part(obj).unwrap_or_default(),
        _ => String::new(),
    }
}

fn render_hover_part(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Object(map) => {
            let value = map.get("value")?.as_str()?;
            if map.get("kind").and_then(Value::as_str).is_some() {
                return Some(value.to_string());
            }
            let lang = map.get("language")?.as_str()?;
            Some(format!("```{lang}\n{value}\n```"))
        }
        _ => None,
    }
}



#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ServerKey {
    language: LspLanguage,
    root: PathBuf,
}




pub struct LspManager {
    servers: Mutex<HashMap<ServerKey, Arc<LspConnection>>>,


    overrides: Mutex<HashMap<LspLanguage, (String, Vec<String>)>>,
}

impl LspManager {
    pub fn new() -> Self {
        Self {
            servers: Mutex::new(HashMap::new()),
            overrides: Mutex::new(HashMap::new()),
        }
    }

    pub fn set_server_command(&self, lang: LspLanguage, command: String, args: Vec<String>) {
        self.overrides.lock().unwrap().insert(lang, (command, args));
    }


    async fn connection_for(&self, path: &Path) -> anyhow::Result<Arc<LspConnection>> {
        let lang = detect_language(path).ok_or_else(|| {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("(none)");
            anyhow::anyhow!(
                "unsupported file type '.{ext}' - LSP code intelligence covers \
                 Rust (.rs), Python (.py) and TypeScript/JavaScript \
                 (.ts, .tsx, .js, .jsx, .mjs, .cjs)"
            )
        })?;
        let root = find_project_root(path, lang);
        let key = ServerKey { language: lang, root: root.clone() };
        if let Some(conn) = self.servers.lock().unwrap().get(&key).cloned() {
            return Ok(conn);
        }
        let spec = server_spec(lang);
        let (command, args) = self
            .overrides
            .lock()
            .unwrap()
            .get(&lang)
            .cloned()
            .unwrap_or((spec.command.clone(), spec.args.clone()));
        if find_binary(&command).is_none() {
            anyhow::bail!(
                "language server '{command}' not found on PATH - install it with: {}",
                spec.install_hint
            );
        }
        let conn = LspConnection::start(lang, &command, &args, &root)
            .await
            .map_err(|e| anyhow::anyhow!("failed to start '{}': {e}", spec.command))?;
        self.servers.lock().unwrap().insert(key, conn.clone());
        Ok(conn)
    }



    pub async fn goto_definition(
        &self,
        path: &Path,
        line: usize,
        col: usize,
    ) -> anyhow::Result<Vec<LspLocation>> {
        let conn = self.connection_for(path).await?;
        Ok(parse_locations(&conn.definition(path, line, col).await?))
    }


    pub async fn hover(
        &self,
        path: &Path,
        line: usize,
        col: usize,
    ) -> anyhow::Result<String> {
        let conn = self.connection_for(path).await?;
        Ok(render_hover(&conn.hover(path, line, col).await?))
    }


    pub async fn find_references(
        &self,
        path: &Path,
        line: usize,
        col: usize,
        include_declaration: bool,
    ) -> anyhow::Result<Vec<LspLocation>> {
        let conn = self.connection_for(path).await?;
        Ok(parse_locations(
            &conn.references(path, line, col, include_declaration).await?,
        ))
    }
}

impl Default for LspManager {
    fn default() -> Self {
        Self::new()
    }
}
