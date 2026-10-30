use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::{AsyncWriteExt, BufReader};
use zen_core::{Tool, ToolContext};

use crate::lsp::LspClient;
use crate::lsp::{
    col_to_utf16, detect_language, find_binary, find_project_root, parse_locations, path_to_uri,
    render_hover, server_spec, uri_to_path, utf16_to_col, LspLanguage, LspManager,
};
use crate::lsp::{read_frame, write_frame};
use crate::lsp_tools::{FindReferencesTool, GotoDefinitionTool, HoverTool};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

static PATH_LOCK: Mutex<()> = Mutex::new(());

fn unique_tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zen-lsp-test-{}-{}-{}",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::SeqCst),
        tag
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

const MOCK_SERVER: &str = r##"#!/usr/bin/env python3
import sys, json

log_path, target_uri = sys.argv[1], sys.argv[2]

def log(method):
    with open(log_path, "a") as f:
        f.write(method + "\n")

def read_msg():
    headers = {}
    while True:
        raw = sys.stdin.buffer.readline()
        if not raw:
            return None
        line = raw.decode("utf-8", "replace")
        if line in ("\r\n", "\n", ""):
            break
        if ":" in line:
            k, v = line.split(":", 1)
            headers[k.strip().lower()] = v.strip()
    n = int(headers.get("content-length", "0"))
    if n <= 0:
        return None
    return json.loads(sys.stdin.buffer.read(n).decode("utf-8"))

def send(obj):
    data = json.dumps(obj).encode("utf-8")
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(data) + data)
    sys.stdout.buffer.flush()

while True:
    msg = read_msg()
    if msg is None:
        break
    method, mid = msg.get("method", ""), msg.get("id")
    log(method)
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": mid, "result": {
            "capabilities": {"textDocumentSync": 2, "definitionProvider": True,
                             "hoverProvider": True, "referencesProvider": True}}})
    elif method == "textDocument/definition":
        send({"jsonrpc": "2.0", "id": mid, "result": {
            "uri": target_uri,
            "range": {"start": {"line": 9, "character": 4},
                      "end": {"line": 9, "character": 12}}}})
    elif method == "textDocument/hover":
        send({"jsonrpc": "2.0", "id": mid, "result": {
            "contents": {"kind": "markdown", "value": "**mock** hover"}}})
    elif method == "textDocument/references":
        send({"jsonrpc": "2.0", "id": mid, "result": []})
    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": mid, "result": None})
    elif method == "exit":
        break
    # notifications (initialized / didOpen / didChange) and unknown methods: no reply
"##;

struct MockSetup {
    manager: LspManager,
    _tmp: PathBuf,
    main_rs: PathBuf,
    target_rs: PathBuf,
    log: PathBuf,
}

fn mock_setup() -> Option<MockSetup> {
    let py = find_binary("python3")?;
    let tmp = unique_tmpdir("mock");
    let main_rs = tmp.join("main.rs");
    write_file(
        &main_rs,
        "fn main() {\n    let x = 1;\n    println!(\"{x}\");\n}\n",
    );
    let target_rs = tmp.join("target.rs");
    let mut target_src = String::new();
    for i in 1..=9 {
        target_src.push_str(&format!("// line {i}\n"));
    }
    target_src.push_str("    pub fn target_fn() {}\n");
    write_file(&target_rs, &target_src);
    let script = tmp.join("mock_lsp.py");
    write_file(&script, MOCK_SERVER);
    let log = tmp.join("methods.log");
    write_file(&log, "");

    let manager = LspManager::new();
    manager.set_server_command(
        LspLanguage::Rust,
        py.to_string_lossy().into_owned(),
        vec![
            script.to_string_lossy().into_owned(),
            log.to_string_lossy().into_owned(),
            path_to_uri(&target_rs),
        ],
    );
    Some(MockSetup {
        manager,
        _tmp: tmp,
        main_rs,
        target_rs,
        log,
    })
}

fn logged_methods(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn count(methods: &[String], name: &str) -> usize {
    methods.iter().filter(|m| m.as_str() == name).count()
}

#[tokio::test]
async fn frame_roundtrip_over_duplex() {
    let (mut a, mut b) = tokio::io::duplex(65536);
    let first = json!({"jsonrpc": "2.0", "id": 7, "method": "ping"});
    let second = json!({"jsonrpc": "2.0", "id": 8, "method": "pong", "params": {"é": "😀"}});
    write_frame(&mut a, &first).await.unwrap();
    write_frame(&mut a, &second).await.unwrap();

    let mut reader = BufReader::new(&mut b);
    let got1 = read_frame(&mut reader).await.unwrap();
    let got2 = read_frame(&mut reader).await.unwrap();
    assert_eq!(got1, first);
    assert_eq!(got2, second);
}

#[tokio::test]
async fn read_frame_ignores_extra_headers_and_case() {
    let (mut a, mut b) = tokio::io::duplex(65536);
    let body = br#"{"ok":true}"#;
    let raw = format!(
        "content-length: {}\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n",
        body.len()
    );
    a.write_all(raw.as_bytes()).await.unwrap();
    a.write_all(body).await.unwrap();
    let got = read_frame(&mut BufReader::new(&mut b)).await.unwrap();
    assert_eq!(got, json!({"ok": true}));
}

#[tokio::test]
async fn read_frame_errors_without_content_length() {
    let (mut a, mut b) = tokio::io::duplex(65536);
    a.write_all(b"Content-Type: application/json\r\n\r\n{}")
        .await
        .unwrap();
    let err = read_frame(&mut BufReader::new(&mut b)).await.unwrap_err();
    assert!(err.to_string().contains("Content-Length"), "got: {err}");
}

#[tokio::test]
async fn mock_server_full_roundtrip() {
    let Some(setup) = mock_setup() else {
        eprintln!("SKIP: python3 not found");
        return;
    };
    let locs = setup
        .manager
        .goto_definition(&setup.main_rs, 1, 5)
        .await
        .unwrap();
    assert_eq!(locs.len(), 1, "expected one definition, got {locs:?}");
    let loc = &locs[0];
    assert_eq!(loc.path, setup.target_rs.to_string_lossy());
    assert_eq!(loc.line, 10);
    assert_eq!(loc.col, 5);
    assert_eq!(loc.end_col, 13);

    let methods = logged_methods(&setup.log);
    assert_eq!(
        count(&methods, "initialize"),
        1,
        "handshake must run once: {methods:?}"
    );
    assert!(
        methods.contains(&"initialized".to_string()),
        "initialized notification missing: {methods:?}"
    );
    assert_eq!(count(&methods, "textDocument/didOpen"), 1);
    assert_eq!(count(&methods, "textDocument/definition"), 1);

    let _ = setup
        .manager
        .goto_definition(&setup.main_rs, 2, 9)
        .await
        .unwrap();
    let methods = logged_methods(&setup.log);
    assert_eq!(
        count(&methods, "initialize"),
        1,
        "server must stay alive: {methods:?}"
    );
    assert_eq!(count(&methods, "textDocument/didOpen"), 1);
    assert_eq!(count(&methods, "textDocument/definition"), 2);
}

#[tokio::test]
async fn mock_server_sends_did_change_when_mtime_moves() {
    let Some(setup) = mock_setup() else {
        eprintln!("SKIP: python3 not found");
        return;
    };
    let _ = setup
        .manager
        .goto_definition(&setup.main_rs, 1, 5)
        .await
        .unwrap();
    let old_mtime = std::fs::metadata(&setup.main_rs)
        .unwrap()
        .modified()
        .unwrap();

    for _ in 0..50 {
        write_file(&setup.main_rs, "fn main() {\n    let y = 2;\n}\n");
        let new_mtime = std::fs::metadata(&setup.main_rs)
            .unwrap()
            .modified()
            .unwrap();
        if new_mtime != old_mtime {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = setup
        .manager
        .goto_definition(&setup.main_rs, 1, 5)
        .await
        .unwrap();
    let methods = logged_methods(&setup.log);
    assert_eq!(
        count(&methods, "textDocument/didChange"),
        1,
        "expected one didChange after mtime moved: {methods:?}"
    );
    assert_eq!(count(&methods, "textDocument/didOpen"), 1);
}

#[tokio::test]
async fn mock_server_hover_and_references() {
    let Some(setup) = mock_setup() else {
        eprintln!("SKIP: python3 not found");
        return;
    };
    let hover = setup.manager.hover(&setup.main_rs, 1, 5).await.unwrap();
    assert!(hover.contains("**mock** hover"), "got: {hover}");
    let refs = setup
        .manager
        .find_references(&setup.main_rs, 1, 5, true)
        .await
        .unwrap();
    assert!(refs.is_empty(), "mock returns no references, got {refs:?}");
}

#[tokio::test]
async fn request_timeout_is_surfaced_not_hung() {
    let py = match find_binary("python3") {
        Some(p) => p,
        None => {
            eprintln!("SKIP: python3 not found");
            return;
        }
    };
    let tmp = unique_tmpdir("timeout");
    let script = tmp.join("mock_lsp.py");
    write_file(&script, MOCK_SERVER);
    let log = tmp.join("methods.log");
    write_file(&log, "");
    let client = LspClient::spawn(
        py.to_str().unwrap(),
        &[
            script.to_string_lossy().into_owned(),
            log.to_string_lossy().into_owned(),
            "file:///none".to_string(),
        ],
    )
    .unwrap();

    let err = client
        .request("test/neverResponds", json!({}), Duration::from_millis(300))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("timed out"), "got: {err}");
}

fn loc_result(uri: &str, sl: u64, sc: u64, el: u64, ec: u64) -> Value {
    json!({"uri": uri,
           "range": {"start": {"line": sl, "character": sc},
                     "end": {"line": el, "character": ec}}})
}

#[test]
fn parse_definition_single_location() {
    let locs = parse_locations(&loc_result("file:///nonexistent/a.rs", 4, 2, 4, 8));
    assert_eq!(locs.len(), 1);
    assert_eq!(locs[0].path, "/nonexistent/a.rs");
    assert_eq!((locs[0].line, locs[0].col), (5, 3));
    assert_eq!((locs[0].end_line, locs[0].end_col), (5, 9));
}

#[test]
fn parse_definition_location_array() {
    let result = json!([
        loc_result("file:///nonexistent/a.rs", 0, 0, 0, 5),
        loc_result("file:///nonexistent/b.py", 10, 1, 10, 6),
    ]);
    let locs = parse_locations(&result);
    assert_eq!(locs.len(), 2);
    assert_eq!(locs[1].path, "/nonexistent/b.py");
    assert_eq!((locs[1].line, locs[1].col), (11, 2));
}

#[test]
fn parse_definition_location_links() {
    let result = json!([{
        "originSelectionRange": {"start": {"line": 1, "character": 1},
                                "end": {"line": 1, "character": 4}},
        "targetUri": "file:///nonexistent/lib.rs",
        "targetRange": {"start": {"line": 41, "character": 4},
                       "end": {"line": 41, "character": 10}},
        "targetSelectionRange": {"start": {"line": 41, "character": 4},
                                "end": {"line": 41, "character": 10}},
    }]);
    let locs = parse_locations(&result);
    assert_eq!(locs.len(), 1);
    assert_eq!(locs[0].uri, "file:///nonexistent/lib.rs");
    assert_eq!(locs[0].path, "/nonexistent/lib.rs");
    assert_eq!((locs[0].line, locs[0].col), (42, 5));
}

#[test]
fn parse_definition_null_and_empty() {
    assert!(parse_locations(&Value::Null).is_empty());
    assert!(parse_locations(&json!([])).is_empty());

    assert!(parse_locations(&json!([{"nope": 1}])).is_empty());
}

#[test]
fn parse_definition_converts_utf16_columns_via_target_file() {
    let tmp = unique_tmpdir("utf16-target");
    let target = tmp.join("u.rs");
    write_file(
        &target,
        "fn x() {}\nlet caf\u{e9}_\u{2615}_\u{1f600} = 1;\n",
    );
    let uri = path_to_uri(&target);
    let locs = parse_locations(&loc_result(&uri, 1, 11, 1, 13));
    assert_eq!(locs.len(), 1);
    assert_eq!(
        locs[0].col, 12,
        "emoji counts as one character, got {:?}",
        locs[0]
    );

    assert_eq!(locs[0].end_col, 13);
}

#[test]
fn utf16_conversions() {
    assert_eq!(col_to_utf16("abc", 1), 0);
    assert_eq!(col_to_utf16("abc", 2), 1);
    assert_eq!(col_to_utf16("abc", 4), 3);
    assert_eq!(col_to_utf16("abc", 99), 3);
    assert_eq!(col_to_utf16("", 1), 0);

    assert_eq!(col_to_utf16("a\u{e9}\u{1f600}b", 3), 2);
    assert_eq!(col_to_utf16("a\u{e9}\u{1f600}b", 4), 4);

    assert_eq!(utf16_to_col("abc", 0), 1);
    assert_eq!(utf16_to_col("abc", 3), 4);
    assert_eq!(utf16_to_col("", 0), 1);
    assert_eq!(utf16_to_col("a\u{e9}\u{1f600}b", 2), 3);
    assert_eq!(utf16_to_col("a\u{e9}\u{1f600}b", 4), 4);

    assert_eq!(utf16_to_col("a\u{e9}\u{1f600}b", 3), 4);
}

#[test]
fn hover_renders_all_content_shapes() {
    let md = |v: &str| format!("{{\"contents\":{v}}}");

    assert_eq!(
        render_hover(&serde_json::from_str(&md("\"hello\"")).unwrap()),
        "hello"
    );

    let r = render_hover(
        &serde_json::from_str(&md("{\"kind\":\"markdown\",\"value\":\"**sig**\"}")).unwrap(),
    );
    assert_eq!(r, "**sig**");

    let r = render_hover(
        &serde_json::from_str(&md("{\"language\":\"rust\",\"value\":\"fn f()\"}")).unwrap(),
    );
    assert_eq!(r, "```rust\nfn f()\n```");

    let r = render_hover(
        &serde_json::from_str(&md(
            "[\"intro\",{\"language\":\"python\",\"value\":\"def f():\"},{\"kind\":\"plaintext\",\"value\":\"docs\"}]",
        ))
        .unwrap(),
    );
    assert_eq!(r, "intro\n\n```python\ndef f():\n```\n\ndocs");

    assert_eq!(render_hover(&json!(null)), "");
    assert_eq!(render_hover(&json!({})), "");
}

#[test]
fn uri_roundtrip_with_special_chars() {
    let p = PathBuf::from("/tmp/some dir/caf\u{e9}/x.rs");
    let uri = path_to_uri(&p);
    assert!(uri.starts_with("file:///tmp/some%20dir/"), "got: {uri}");
    assert_eq!(uri_to_path(&uri).unwrap(), p);
    assert_eq!(uri_to_path("untitled:Untitled-1"), None);
}

#[test]
fn language_detection_matrix() {
    let cases: &[(&str, Option<LspLanguage>)] = &[
        ("main.rs", Some(LspLanguage::Rust)),
        ("MAIN.RS", Some(LspLanguage::Rust)),
        ("mod.py", Some(LspLanguage::Python)),
        ("index.ts", Some(LspLanguage::TypeScript)),
        ("app.tsx", Some(LspLanguage::TypeScript)),
        ("bundle.js", Some(LspLanguage::TypeScript)),
        ("comp.jsx", Some(LspLanguage::TypeScript)),
        ("esm.mjs", Some(LspLanguage::TypeScript)),
        ("cjs.cjs", Some(LspLanguage::TypeScript)),
        ("notes.txt", None),
        ("Makefile", None),
        ("noext", None),
    ];
    for (name, expected) in cases {
        assert_eq!(
            detect_language(Path::new(name)),
            *expected,
            "extension of {name}"
        );
    }
}

#[test]
fn project_root_uses_markers() {
    let tmp = unique_tmpdir("markers");

    let rs = tmp.join("proj-rs").join("src").join("deep").join("main.rs");
    write_file(&tmp.join("proj-rs").join("Cargo.toml"), "[package]\n");
    write_file(&rs, "fn main() {}\n");
    assert_eq!(
        find_project_root(&rs, LspLanguage::Rust),
        tmp.join("proj-rs")
    );

    let py = tmp.join("proj-py").join("pkg").join("mod.py");
    write_file(&tmp.join("proj-py").join("pyproject.toml"), "[project]\n");
    write_file(&py, "x = 1\n");
    assert_eq!(
        find_project_root(&py, LspLanguage::Python),
        tmp.join("proj-py")
    );

    let ts = tmp.join("proj-ts").join("src").join("index.ts");
    write_file(&tmp.join("proj-ts").join("package.json"), "{}\n");
    write_file(&ts, "export {};\n");
    assert_eq!(
        find_project_root(&ts, LspLanguage::TypeScript),
        tmp.join("proj-ts")
    );

    let mixed = tmp.join("mixed").join("x.rs");
    write_file(&tmp.join("mixed").join("setup.py"), "");
    write_file(&mixed, "fn f() {}\n");
    assert_eq!(
        find_project_root(&mixed, LspLanguage::Rust),
        tmp.join("mixed")
    );

    let plain = tmp.join("plain").join("a").join("b").join("c.rs");
    write_file(&plain, "fn f() {}\n");
    assert_eq!(
        find_project_root(&plain, LspLanguage::Rust),
        tmp.join("plain").join("a").join("b")
    );
}

#[test]
fn server_specs_match_requirements() {
    let rust = server_spec(LspLanguage::Rust);
    assert_eq!(rust.command, "rust-analyzer");
    assert!(rust.args.is_empty());
    assert!(rust
        .install_hint
        .contains("rustup component add rust-analyzer"));

    let py = server_spec(LspLanguage::Python);
    assert_eq!(py.command, "pyright-langserver");
    assert_eq!(py.args, vec!["--stdio".to_string()]);
    assert!(py.install_hint.contains("npm install -g pyright"));

    let ts = server_spec(LspLanguage::TypeScript);
    assert_eq!(ts.command, "typescript-language-server");
    assert_eq!(ts.args, vec!["--stdio".to_string()]);
    assert!(ts
        .install_hint
        .contains("npm install -g typescript-language-server"));
}

#[tokio::test]
async fn unknown_extension_is_a_clear_error() {
    let tmp = unique_tmpdir("unknown-ext");
    let f = tmp.join("data.xyz");
    write_file(&f, "hello\n");
    let mgr = LspManager::new();
    let err = mgr.goto_definition(&f, 1, 1).await.unwrap_err().to_string();
    assert!(err.contains("unsupported file type"), "got: {err}");
    assert!(err.contains(".rs"), "got: {err}");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]

async fn missing_binary_names_the_install_command() {
    let _guard = PATH_LOCK.lock().unwrap();
    let tmp = unique_tmpdir("empty-path");
    let old_path = std::env::var_os("PATH");
    std::env::set_var("PATH", &tmp);

    let mgr = LspManager::new();
    let cases = [
        ("a.rs", "rustup component add rust-analyzer"),
        ("b.py", "npm install -g pyright"),
        ("c.ts", "npm install -g typescript-language-server"),
    ];
    let mut failures = Vec::new();
    for (name, hint) in cases {
        let f = tmp.join(name);
        write_file(&f, "x\n");
        let err = mgr.goto_definition(&f, 1, 1).await.unwrap_err().to_string();
        if !err.contains(hint) {
            failures.push(format!("{name}: expected hint '{hint}', got '{err}'"));
        }
        if !err.contains("not found on PATH") {
            failures.push(format!("{name}: expected 'not found on PATH', got '{err}'"));
        }
    }

    match old_path {
        Some(v) => std::env::set_var("PATH", v),
        None => std::env::remove_var("PATH"),
    }
    drop(_guard);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn test_ctx(dir: &Path) -> ToolContext {
    ToolContext::new(dir.to_path_buf(), "lsp-test-session".to_string())
}

#[test]
fn tool_metadata() {
    let mgr = std::sync::Arc::new(LspManager::new());
    let def = GotoDefinitionTool::new(mgr.clone());
    let hov = HoverTool::new(mgr.clone());
    let refs = FindReferencesTool::new(mgr);
    assert_eq!(def.name(), "goto_definition");
    assert_eq!(hov.name(), "hover");
    assert_eq!(refs.name(), "find_references");
    assert!(def.is_read_only() && hov.is_read_only() && refs.is_read_only());
    for t in [&def as &dyn Tool, &hov as &dyn Tool, &refs as &dyn Tool] {
        let schema = t.parameters_schema();
        let required = schema
            .get("required")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>())
            .unwrap_or_default();
        assert!(required.contains(&"file"), "{}", t.name());
        assert!(required.contains(&"line"), "{}", t.name());
        assert!(required.contains(&"col"), "{}", t.name());
    }
    let refs_schema = refs.parameters_schema();
    assert!(
        refs_schema
            .get("properties")
            .and_then(|p| p.get("include_declaration"))
            .is_some(),
        "find_references must document include_declaration"
    );
}

#[tokio::test]
async fn tool_rejects_bad_arguments() {
    let mgr = std::sync::Arc::new(LspManager::new());
    let ctx = test_ctx(&unique_tmpdir("bad-args"));
    let def = GotoDefinitionTool::new(mgr);

    let out = def.call(json!({}), &ctx).await;
    assert!(
        out.is_error && out.content.contains("'file'"),
        "got: {}",
        out.content
    );

    let out = def
        .call(json!({"file": "x.rs", "line": 0, "col": 1}), &ctx)
        .await;
    assert!(
        out.is_error && out.content.contains(">= 1"),
        "got: {}",
        out.content
    );
}

#[tokio::test]
async fn tool_reports_unsupported_extension() {
    let tmp = unique_tmpdir("tool-unsupported");
    write_file(&tmp.join("data.xyz"), "hi\n");
    let mgr = std::sync::Arc::new(LspManager::new());
    let ctx = test_ctx(&tmp);
    let out = GotoDefinitionTool::new(mgr)
        .call(json!({"file": "data.xyz", "line": 1, "col": 1}), &ctx)
        .await;
    assert!(out.is_error, "expected error, got: {}", out.content);
    assert!(
        out.content.contains("unsupported file type"),
        "got: {}",
        out.content
    );
}

#[tokio::test]
async fn tool_output_has_human_lines_and_json_block() {
    let Some(setup) = mock_setup() else {
        eprintln!("SKIP: python3 not found");
        return;
    };

    let mgr = std::sync::Arc::new(setup.manager);
    let ctx = test_ctx(&setup._tmp);
    let out = GotoDefinitionTool::new(mgr.clone())
        .call(json!({"file": "main.rs", "line": 1, "col": 5}), &ctx)
        .await;
    assert!(!out.is_error, "got error: {}", out.content);
    assert!(
        out.content.contains("goto_definition"),
        "got: {}",
        out.content
    );
    assert!(
        out.content.contains("target.rs:10:5"),
        "got: {}",
        out.content
    );
    assert!(out.content.contains("```json"), "got: {}", out.content);
    assert!(out.content.contains("\"line\": 10"), "got: {}", out.content);

    let hover_out = HoverTool::new(mgr)
        .call(json!({"file": "main.rs", "line": 1, "col": 5}), &ctx)
        .await;
    assert!(!hover_out.is_error, "got error: {}", hover_out.content);
    assert!(
        hover_out.content.contains("**mock** hover"),
        "got: {}",
        hover_out.content
    );
    assert!(
        hover_out.content.contains("```json"),
        "got: {}",
        hover_out.content
    );
}
