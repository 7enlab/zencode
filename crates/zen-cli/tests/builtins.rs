//! End-to-end tests for built-in slash commands (SPEC C5.1), the `-p` built-in subset (C5.4),
//! custom-command `model` / `allowed-tools` frontmatter (C5.2), sub-agent lines in stream-json
//! (C6.4) and Ctrl-C in the REPL (C8.9).

mod common;

use common::*;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

fn run_p(tag: &str, home: &TempDir, cwd: &TempDir, server: &MockServer, prompt: &str) -> (serde_json::Value, i32) {
    let (stdout, stderr, code) = ZenRun::new(tag, home, cwd)
        .args(["-p", "--output-format", "json", "--setting-sources", "", "--no-session-persistence"])
        .mock("mock-model", server)
        .arg(prompt)
        .run_output();
    assert!(!stdout.trim().is_empty(), "no stdout; stderr: {}", stderr);
    (last_result(&stdout), code)
}

/// C5.4: `/model x`, `/cost`, `/mcp`, `/context` answer locally in -p (no model request);
/// terminal-only built-ins are an error result with a non-zero exit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c5_4_headless_builtins_answer_locally() {
    let home = TempDir::new("pbuiltin_home");
    let cwd = TempDir::new("pbuiltin_cwd");
    let server = MockServer::single_text("should not be reached").await;

    let (v, code) = run_p("pb1", &home, &cwd, &server, "/model other-model");
    assert_eq!(code, 0);
    assert_eq!(v["result"], "Set model to other-model");
    assert_eq!(v["model"], "other-model");

    let (v, code) = run_p("pb2", &home, &cwd, &server, "/model");
    assert_eq!(code, 0);
    assert_eq!(v["result"], "Current model: mock-model");

    let (v, _) = run_p("pb3", &home, &cwd, &server, "/cost");
    assert!(v["result"].as_str().unwrap().contains("Usage: 0 input, 0 output tokens"), "{}", v);

    let (v, _) = run_p("pb4", &home, &cwd, &server, "/mcp");
    assert_eq!(v["result"], "No MCP servers configured.");

    let (v, _) = run_p("pb5", &home, &cwd, &server, "/context");
    let text = v["result"].as_str().unwrap();
    assert!(text.starts_with("Context usage (estimated)"), "{}", text);
    assert!(text.contains("- tools ("), "{}", text);

    let (v, code) = run_p("pb6", &home, &cwd, &server, "/help");
    assert_eq!(code, 1);
    assert_eq!(v["is_error"], true);
    assert!(v["result"].as_str().unwrap().contains("/help isn't available in -p mode"), "{}", v);

    assert!(server.requests().await.is_empty(), "built-ins must not call the model");
}

/// C5.4: `/mcp` reports each configured server's status.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c5_4_headless_mcp_lists_server_status() {
    let home = TempDir::new("pmcp_home");
    let cwd = TempDir::new("pmcp_cwd");
    cwd.write(".mcp.json", r#"{"mcpServers":{"broken":{"command":"/nonexistent/zen-mcp-server"}}}"#);
    let server = MockServer::single_text("unused").await;
    let (stdout, _, code) = ZenRun::new("pmcp", &home, &cwd)
        .args(["-p", "--output-format", "json", "--setting-sources", "", "--no-session-persistence"])
        .env("MCP_TIMEOUT", Some("3000"))
        .mock("mock-model", &server)
        .arg("/mcp")
        .run_output();
    assert_eq!(code, 0, "{}", stdout);
    assert_eq!(last_result(&stdout)["result"], "broken: failed");
}

/// C5.1/C5.4: `/review [focus]` sends zen's review prompt to the model.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c5_1_review_sends_review_prompt() {
    let home = TempDir::new("review_home");
    let cwd = TempDir::new("review_cwd");
    let server = MockServer::single_text("looks fine").await;
    let (v, code) = run_p("review", &home, &cwd, &server, "/review error handling");
    assert_eq!(code, 0);
    assert_eq!(v["result"], "looks fine");
    let reqs = server.requests().await;
    let text = request_messages_text(&reqs[0]);
    assert!(text.contains("git diff") && text.contains("Focus: error handling"), "{}", text);
}

fn bash_call(id: &str, cmd: &str) -> Reply {
    Reply::with_tool_calls(
        "",
        Some(serde_json::json!([{
            "id": id, "type": "function",
            "function": {"name": "Bash", "arguments": serde_json::json!({"command": cmd}).to_string()}
        }])),
    )
}

/// C5.2: a custom command's `model` picks the request model and its `allowed-tools` lets the
/// listed tool run without asking (in -p it would otherwise be denied).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c5_2_command_model_and_allowed_tools_apply() {
    let home = TempDir::new("cmdfm_home");
    let cwd = TempDir::new("cmdfm_cwd");
    cwd.write(
        ".zen/commands/hello.md",
        "---\ndescription: say hello\nmodel: special-model\nallowed-tools: Bash(echo zen-hello)\n---\nRun echo zen-hello\n",
    );
    cwd.write(".zen/commands/plain.md", "---\ndescription: no tools\n---\nRun echo zen-hello\n");

    let server = MockServer::start(vec![bash_call("call_1", "echo zen-hello"), Reply::text("done")]).await;
    let (v, code) = run_p("cmdfm", &home, &cwd, &server, "/hello");
    assert_eq!(code, 0, "{}", v);
    assert_eq!(v["permission_denials"], serde_json::json!([]), "{}", v);
    let reqs = server.requests().await;
    assert_eq!(request_json(&reqs[0])["model"], "special-model");
    assert!(request_messages_text(&reqs[1]).contains("zen-hello"), "tool output reaches the model");

    // Control: the same call from a command without allowed-tools is denied in -p.
    let server2 = MockServer::start(vec![bash_call("call_2", "echo zen-hello"), Reply::text("done")]).await;
    let (v2, _) = run_p("cmdfm2", &home, &cwd, &server2, "/plain");
    assert_eq!(v2["permission_denials"][0]["tool_name"], "Bash", "{}", v2);
    assert_eq!(request_json(&server2.requests().await[0])["model"], "mock-model");
}

/// C6.4: in stream-json, the sub-agent's own messages carry parent_tool_use_id = Task call id.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c6_4_stream_json_subagent_lines_carry_parent_tool_use_id() {
    let home = TempDir::new("c64_home");
    let cwd = TempDir::new("c64_cwd");
    let agents = r#"{"helper":{"description":"helps","prompt":"You help.","tools":["Bash"]}}"#;
    let server = MockServer::start(vec![
        Reply::with_tool_calls("", Some(serde_json::json!([{
            "id": "call_task_9", "type": "function",
            "function": {"name": "Task", "arguments": "{\"description\":\"d\",\"prompt\":\"run it\",\"subagent_type\":\"helper\"}"}
        }]))),
        bash_call("call_sub_bash", "echo from-sub"),
        Reply::text("sub done"),
        Reply::text("parent done"),
    ])
    .await;
    let (stdout, stderr, code) = ZenRun::new("c64", &home, &cwd)
        .args(["-p", "--output-format", "stream-json", "--bare", "--setting-sources", "",
            "--permission-mode", "bypassPermissions", "--no-session-persistence", "--agents", agents])
        .mock("mock-model", &server)
        .arg("delegate")
        .run_output();
    assert_eq!(code, 0, "stderr: {}", stderr);
    let lines = ndjson(&stdout);
    let child: Vec<&serde_json::Value> =
        lines.iter().filter(|l| l["parent_tool_use_id"] == "call_task_9").collect();
    let kinds: Vec<&str> = child.iter().map(|l| l["type"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"assistant") && kinds.contains(&"user"), "{}", stdout);
    assert!(child.iter().any(|l| l.to_string().contains("call_sub_bash")), "sub-agent tool_use");
    assert!(child.iter().any(|l| l.to_string().contains("sub done")), "sub-agent final text");
    // The parent's own Task call and final answer stay top-level.
    let parent_final = lines.iter().find(|l| l["type"] == "assistant" && l.to_string().contains("parent done")).unwrap();
    assert!(parent_final["parent_tool_use_id"].is_null());
    assert!(lines.iter().all(|l| l["session_id"].is_string()));
}

/// C5.1: REPL /rename, /export, /add-dir, /context, /mcp, then /resume by the new name.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c5_1_repl_builtins_rename_export_resume() {
    let home = TempDir::new("replb_home");
    let cwd = TempDir::new("replb_cwd");
    cwd.subdir("extra");
    let server = MockServer::start(vec![Reply::text("hi there"), Reply::text("second answer")]).await;
    let (stdout, stderr, code) = ZenRun::new("replb", &home, &cwd)
        .args(["--setting-sources", ""])
        .mock("mock-model", &server)
        .stdin("hello zen\n/rename my-feature\n/export out.md\n/add-dir extra\n/add-dir nope\n/context\n/mcp\n/exit\n")
        .run_output();
    assert_eq!(code, 0, "stderr: {}", stderr);
    assert!(stdout.contains("hi there"), "{}", stdout);
    assert!(stdout.contains("session renamed to my-feature"), "{}", stdout);
    assert!(stdout.contains("added working directory"), "{}", stdout);
    assert!(stderr.contains("not a directory"), "{}", stderr);
    assert!(stdout.contains("Context usage (estimated)") && stdout.contains("- messages (2)"), "{}", stdout);
    assert!(stdout.contains("No MCP servers configured."), "{}", stdout);
    let md = std::fs::read_to_string(cwd.path().join("out.md")).expect("export file");
    assert!(md.contains("## User\n\nhello zen") && md.contains("## Assistant\n\nhi there"), "{}", md);

    // A new REPL resumes the renamed session and sends its history with the next prompt.
    let (stdout2, stderr2, code2) = ZenRun::new("replb2", &home, &cwd)
        .args(["--setting-sources", ""])
        .mock("mock-model", &server)
        .stdin("/resume\n/resume my-feature\nand again\n/exit\n")
        .run_output();
    assert_eq!(code2, 0, "stderr: {}", stderr2);
    assert!(stdout2.contains("my-feature"), "listing shows the title: {}", stdout2);
    assert!(stdout2.contains("(2 messages)"), "{}", stdout2);
    assert!(stdout2.contains("second answer"), "{}", stdout2);
    let reqs = server.requests().await;
    let last = request_messages_text(reqs.last().unwrap());
    assert!(last.contains("hello zen") && last.contains("hi there") && last.contains("and again"), "{}", last);
}

/// `ps` state of a pid (Linux and macOS; macOS has no /proc). None = no such process.
#[cfg(unix)]
fn ps_stat(pid: &str) -> Option<String> {
    let o = std::process::Command::new("ps").args(["-o", "stat=", "-p", pid]).output().ok()?;
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (o.status.success() && !s.is_empty()).then_some(s)
}

/// Alive = the process exists and is not a zombie (a reparented child may linger as Z).
#[cfg(unix)]
fn alive(pid: &str) -> bool {
    ps_stat(pid).is_some_and(|s| !s.starts_with('Z'))
}

/// C8.9 (REPL): Ctrl-C ends the running turn and stops its Bash child, the REPL keeps going,
/// and the interrupted turn leaves no orphaned tool call in the history.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c8_9_repl_sigint_cancels_turn_and_continues() {
    let home = TempDir::new("replint_home");
    let cwd = TempDir::new("replint_cwd");
    let pid_file = cwd.path().join("sleep.pid");
    let cmd = format!("sleep 30 & echo $! > '{}'; wait", pid_file.display());
    let server = MockServer::start(vec![bash_call("call_sleep", &cmd), Reply::text("after ok")]).await;
    let mut child = ZenRun::new("replint", &home, &cwd)
        .args(["--setting-sources", "", "--permission-mode", "bypassPermissions"])
        .mock("mock-model", &server)
        .spawn_piped();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "sleep please").unwrap();

    let start = Instant::now();
    let sleep_pid = loop {
        if let Ok(s) = std::fs::read_to_string(&pid_file) {
            if !s.trim().is_empty() {
                break s.trim().to_string();
            }
        }
        assert!(start.elapsed() < Duration::from_secs(20), "sleep never started");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(alive(&sleep_pid));
    assert!(std::process::Command::new("kill").args(["-INT", &child.id().to_string()]).status().unwrap().success());
    let start = Instant::now();
    while alive(&sleep_pid) && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive(&sleep_pid), "sleep survived Ctrl-C");
    assert!(child.try_wait().unwrap().is_none(), "REPL must keep running after Ctrl-C");

    writeln!(stdin, "next prompt").unwrap();
    writeln!(stdin, "/exit").unwrap();
    drop(stdin);
    let start = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait().unwrap() {
            break st;
        }
        if start.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            panic!("REPL did not exit");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let (mut out, mut err) = (String::new(), String::new());
    child.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    child.stderr.take().unwrap().read_to_string(&mut err).unwrap();
    assert_eq!(status.code(), Some(0), "stdout {} stderr {}", out, err);
    assert!(err.contains("Interrupted."), "{}", err);
    assert!(out.contains("after ok") && out.contains("bye"), "{}", out);
    let reqs = server.requests().await;
    let msgs = request_messages(reqs.last().unwrap());
    assert!(msgs.iter().all(|m| m["role"] != "tool" && m.get("tool_calls").is_none()), "orphan left: {:?}", msgs);
    assert!(request_messages_text(reqs.last().unwrap()).contains("next prompt"));
}

/// C5.1: `/config key=value` saves to .zen/settings.local.json; `/clear` starts a new session
/// with its own transcript.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c5_1_repl_config_saves_and_clear_starts_new_session() {
    let home = TempDir::new("replc_home");
    let cwd = TempDir::new("replc_cwd");
    cwd.write(".zen/settings.local.json", r#"{"model":"keep-me"}"#);
    let server = MockServer::start(vec![Reply::text("one"), Reply::text("two")]).await;
    let (stdout, stderr, code) = ZenRun::new("replc", &home, &cwd)
        .args(["--setting-sources", ""])
        .mock("mock-model", &server)
        .stdin("/config autoCompact=5000\n/config theme=dark\nfirst\n/clear\nsecond\n/exit\n")
        .run_output();
    assert_eq!(code, 0, "stderr: {}", stderr);
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(cwd.path().join(".zen/settings.local.json")).unwrap()).unwrap();
    assert_eq!(saved, serde_json::json!({"model": "keep-me", "autoCompact": 5000, "theme": "dark"}));
    assert!(stdout.contains("context cleared, new session:"), "{}", stdout);
    // The turn after /clear does not see the first exchange.
    let reqs = server.requests().await;
    let last = request_messages_text(reqs.last().unwrap());
    assert!(last.contains("second") && !last.contains("first"), "{}", last);
    // Two transcripts: one per session.
    let proj = home.path().join(".zen").join("projects");
    let files: Vec<_> = walkdir_jsonl(&proj);
    assert_eq!(files.len(), 2, "{:?}", files);
}

fn walkdir_jsonl(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![];
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walkdir_jsonl(&p));
        } else if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
            out.push(p);
        }
    }
    out
}

/// C10.1: `--autocompact <tokens>` sets the threshold without any setting; bad values are a
/// flag error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c10_1_autocompact_flag_sets_threshold() {
    let home = TempDir::new("acflag_home");
    let cwd = TempDir::new("acflag_cwd");
    let server = MockServer::start(vec![
        Reply::text("first reply"),
        Reply::text("summary of conversation so far"),
        Reply::text("second reply"),
    ])
    .await;
    let run = |tag: &str, cont: bool, prompt: String| {
        let mut r = ZenRun::new(tag, &home, &cwd).args(["-p", "--output-format", "json", "--bare",
            "--setting-sources", "", "--autocompact", "10"]);
        if cont {
            r = r.arg("--continue");
        }
        r.mock("mock-model", &server).arg(prompt).run_output()
    };
    let (_, e1, c1) = run("ac1", false, "turn 1".into());
    assert_eq!(c1, 0, "{}", e1);
    let (o2, e2, c2) = run("ac2", true, format!("turn 2 {}", "x".repeat(80)));
    assert_eq!(c2, 0, "{}", e2);
    assert_eq!(last_result(&o2)["result"], "second reply");
    let reqs = server.requests().await;
    assert_eq!(reqs.len(), 3, "turn 1, compaction, turn 2");
    assert!(request_messages_text(&reqs[1]).contains("Summarize this coding session"));

    let (_, err, code) = ZenRun::new("ac3", &home, &cwd)
        .args(["-p", "--autocompact", "sometimes", "hi"])
        .run_output();
    assert_ne!(code, 0);
    assert!(err.contains("--autocompact"), "{}", err);
}
