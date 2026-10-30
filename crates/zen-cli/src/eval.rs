use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use zen_agent::{Agent, AgentConfig, OpenAiClient};
use zen_core::{
    AgentEvent, EventSink, LlmBackend, NoHooks, PermissionChecker, PermissionDecision,
    PermissionMode, RunResult, ToolContext, ToolRegistry,
};
use zen_tools::{BashTool, EditTool, MultiEditTool, ReadTool, WriteTool};

#[derive(Debug, Clone)]
pub enum Check {
    FileContains {
        path: &'static str,
        needle: &'static str,
    },

    FileEquals {
        path: &'static str,
        content: &'static str,
    },

    FileNotExists {
        path: &'static str,
    },

    CommandOutputContains {
        cmd: &'static str,
        args: &'static [&'static str],
        needle: &'static str,
    },

    All(Vec<Check>),

    Not(Box<Check>),
}

impl Check {
    pub fn run(&self, workdir: &Path) -> bool {
        match self {
            Check::FileContains { path, needle } => std::fs::read_to_string(workdir.join(path))
                .map(|c| c.contains(needle))
                .unwrap_or(false),
            Check::FileEquals { path, content } => std::fs::read_to_string(workdir.join(path))
                .map(|c| c == *content)
                .unwrap_or(false),
            Check::FileNotExists { path } => !workdir.join(path).exists(),
            Check::CommandOutputContains { cmd, args, needle } => std::process::Command::new(cmd)
                .args(*args)
                .current_dir(workdir)
                .output()
                .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains(needle))
                .unwrap_or(false),
            Check::All(checks) => checks.iter().all(|c| c.run(workdir)),
            Check::Not(inner) => !inner.run(workdir),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Check::FileContains { path, needle } => format!("{path} contains {needle:?}"),
            Check::FileEquals { path, .. } => format!("{path} equals expected content"),
            Check::FileNotExists { path } => format!("{path} does not exist"),
            Check::CommandOutputContains { cmd, args, needle } => {
                format!("`{} {}` output contains {needle:?}", cmd, args.join(" "))
            }
            Check::All(cs) => cs
                .iter()
                .map(|c| c.describe())
                .collect::<Vec<_>>()
                .join(" + "),
            Check::Not(c) => format!("NOT ({})", c.describe()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EvalTask {
    pub id: &'static str,

    pub description: &'static str,

    pub prompt: &'static str,

    pub setup: &'static [(&'static str, &'static str)],

    pub check: Check,
}

pub static EVAL_TASKS: std::sync::LazyLock<Vec<EvalTask>> = std::sync::LazyLock::new(|| {
    vec![
    EvalTask {
        id: "create-file",
        description: "Create hello.txt with exact content",
        prompt: "Create a file named hello.txt containing exactly the text: Hello, eval!",
        setup: &[],
        check: Check::FileEquals {
            path: "hello.txt",
            content: "Hello, eval!",
        },
    },
    EvalTask {
        id: "create-nested",
        description: "Create nested markdown checklist",
        prompt: "Create the file docs/notes/todo.md containing a markdown checklist with exactly these 3 items: Buy milk, Write code, Ship it",
        setup: &[],
        check: Check::All(vec![
            Check::FileContains { path: "docs/notes/todo.md", needle: "Buy milk" },
            Check::FileContains { path: "docs/notes/todo.md", needle: "Write code" },
            Check::FileContains { path: "docs/notes/todo.md", needle: "Ship it" },
        ]),
    },
    EvalTask {
        id: "fix-typo",
        description: "Fix a typo in an existing file",
        prompt: "Fix the typo in message.txt: change 'quik' to 'quick'. Keep everything else identical.",
        setup: &[("message.txt", "The quik brown fox jumps over the lazy dog.\n")],
        check: Check::All(vec![
            Check::FileContains { path: "message.txt", needle: "quick" },
            Check::Not(Box::new(Check::FileContains { path: "message.txt", needle: "quik" })),
        ]),
    },
    EvalTask {
        id: "fix-bug",
        description: "Fix a shell-script bug so it prints result=42",
        prompt: "Fix the bug in calc.sh so that running `sh calc.sh` prints exactly: result=42",
        setup: &[("calc.sh", "#!/bin/sh\nresult=$((6 * 6))\necho \"result=$result\"\n")],
        check: Check::CommandOutputContains {
            cmd: "sh",
            args: &["calc.sh"],
            needle: "result=42",
        },
    },
    EvalTask {
        id: "append-log",
        description: "Append a line to a log file, keeping existing content",
        prompt: "Append a new line containing exactly 'eval-ok' to log.txt. Do not remove or change the existing content.",
        setup: &[("log.txt", "line1\nline2\n")],
        check: Check::All(vec![
            Check::FileContains { path: "log.txt", needle: "line1" },
            Check::FileContains { path: "log.txt", needle: "eval-ok" },
        ]),
    },
    EvalTask {
        id: "rename-key",
        description: "Rename a JSON key, keeping its value",
        prompt: "In data.json, rename the key \"name\" to \"title\". Keep the value \"zen\" and keep the \"version\" key unchanged.",
        setup: &[("data.json", "{\n  \"name\": \"zen\",\n  \"version\": 1\n}\n")],
        check: Check::All(vec![
            Check::FileContains { path: "data.json", needle: "\"title\"" },
            Check::FileContains { path: "data.json", needle: "\"zen\"" },
            Check::Not(Box::new(Check::FileContains { path: "data.json", needle: "\"name\"" })),
        ]),
    },
    EvalTask {
        id: "sum-script",
        description: "Write a script that prints the sum of 19 and 23",
        prompt: "Write a shell script named add.sh that prints the sum of 19 and 23 (i.e. running `sh add.sh` must print 42).",
        setup: &[],
        check: Check::CommandOutputContains {
            cmd: "sh",
            args: &["add.sh"],
            needle: "42",
        },
    },
    EvalTask {
        id: "delete-file",
        description: "Delete a scratch file",
        prompt: "Delete the file scratch.tmp.",
        setup: &[("scratch.tmp", "temporary\n")],
        check: Check::FileNotExists { path: "scratch.tmp" },
    },
    EvalTask {
        id: "multi-edit",
        description: "Fix two typos in one file",
        prompt: "Fix both typos in notes.txt: change 'teh' to 'the' and 'recieve' to 'receive'.",
        setup: &[("notes.txt", "teh package will recieve an update soon.\n")],
        check: Check::All(vec![
            Check::FileContains { path: "notes.txt", needle: "the package" },
            Check::FileContains { path: "notes.txt", needle: "receive" },
            Check::Not(Box::new(Check::FileContains { path: "notes.txt", needle: "teh" })),
            Check::Not(Box::new(Check::FileContains { path: "notes.txt", needle: "recieve" })),
        ]),
    },
    EvalTask {
        id: "readme-section",
        description: "Append an Eval section to README.md",
        prompt: "Add a new section titled '## Eval' at the end of README.md, with one line describing this benchmark.",
        setup: &[("README.md", "# Demo project\n\nSome text here.\n")],
        check: Check::All(vec![
            Check::FileContains { path: "README.md", needle: "## Eval" },
            Check::FileContains { path: "README.md", needle: "Some text here." },
        ]),
    },
]
});

const EDIT_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit"];

#[derive(Debug, Default)]
pub struct MetricSink {
    pub requests: AtomicUsize,

    pub rate_limited_429: AtomicUsize,

    pub edit_failures: AtomicUsize,

    tool_names: Mutex<HashMap<String, String>>,
}

impl MetricSink {
    pub fn snapshot(&self) -> (usize, usize, usize) {
        (
            self.requests.load(Ordering::Relaxed),
            self.rate_limited_429.load(Ordering::Relaxed),
            self.edit_failures.load(Ordering::Relaxed),
        )
    }
}

impl EventSink for MetricSink {
    fn emit(&self, ev: AgentEvent) {
        match ev {
            AgentEvent::Assistant { message, .. } => {
                self.requests.fetch_add(1, Ordering::Relaxed);
                if let Some(calls) = message.tool_calls {
                    let mut map = self.tool_names.lock().unwrap();
                    for tc in calls {
                        map.insert(tc.id, tc.function.name);
                    }
                }
            }
            AgentEvent::ApiRetry {
                error_status: Some(429),
                ..
            } => {
                self.rate_limited_429.fetch_add(1, Ordering::Relaxed);
            }
            AgentEvent::ToolResult {
                tool_use_id,
                is_error: true,
                ..
            } => {
                let map = self.tool_names.lock().unwrap();
                if let Some(name) = map.get(&tool_use_id) {
                    if EDIT_TOOLS.contains(&name.as_str()) {
                        self.edit_failures.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct AllowAll;

impl PermissionChecker for AllowAll {
    fn check(
        &self,
        _name: &str,
        _input: &Value,
        _read_only: bool,
        _ctx: &ToolContext,
    ) -> PermissionDecision {
        PermissionDecision::Allow
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskMetrics {
    pub task_id: String,
    pub passed: bool,
    pub requests: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub rate_limited_429: usize,
    pub edit_failures: usize,
    pub duration_ms: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalReport {
    pub model: String,
    pub task_count: usize,
    pub passed_count: usize,
    pub tasks: Vec<TaskMetrics>,
}

impl EvalReport {
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "# zencode eval - {}\n\n{}/{} tasks passed\n\n",
            self.model, self.passed_count, self.task_count
        ));
        out.push_str(
            "| Task | Pass | Requests | Tokens in | Tokens out | 429s | Edit fails | ms |\n",
        );
        out.push_str(
            "|------|------|----------|-----------|------------|------|------------|----|\n",
        );
        for t in &self.tasks {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
                t.task_id,
                if t.passed { "✅" } else { "❌" },
                t.requests,
                t.input_tokens,
                t.output_tokens,
                t.rate_limited_429,
                t.edit_failures,
                t.duration_ms,
            ));
        }
        for t in self.tasks.iter().filter(|t| t.error.is_some()) {
            out.push_str(&format!(
                "\n- {} error: {}",
                t.task_id,
                t.error.as_deref().unwrap_or("")
            ));
        }
        out
    }

    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).context("serialize eval report")
    }
}

#[derive(Debug, Clone)]
pub struct EvalOptions {
    pub model: String,

    pub task_filter: Option<String>,

    pub max_turns: usize,

    pub base_url: String,
    pub api_key: Option<String>,
}

const EVAL_SYSTEM_PROMPT: &str = "You are a coding agent running inside an eval harness. \
Complete the user's task using the available tools (Read, Write, Edit, MultiEdit, Bash). \
Work only inside the current directory. When the task is done, reply with a brief summary. \
Be concise; do not ask questions.";

async fn run_one_task(
    task: &EvalTask,
    backend: Arc<dyn LlmBackend>,
    sink: Arc<MetricSink>,
    max_turns: usize,
) -> TaskMetrics {
    let started = Instant::now();

    let workdir: PathBuf =
        std::env::temp_dir().join(format!("zencode-eval-{}-{}", task.id, uuid::Uuid::new_v4()));
    let setup_result: Result<()> = (|| {
        std::fs::create_dir_all(&workdir)?;
        for (rel, content) in task.setup {
            let p = workdir.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&p, content)?;
        }
        Ok(())
    })();
    if let Err(e) = setup_result {
        return TaskMetrics {
            task_id: task.id.into(),
            passed: false,
            requests: 0,
            input_tokens: 0,
            output_tokens: 0,
            rate_limited_429: 0,
            edit_failures: 0,
            duration_ms: started.elapsed().as_millis() as u64,
            error: Some(format!("setup failed: {e:#}")),
        };
    }

    let session_id = format!("eval-{}-{}", task.id, uuid::Uuid::new_v4());
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ReadTool));
    registry.register(Arc::new(WriteTool));
    registry.register(Arc::new(EditTool));
    registry.register(Arc::new(MultiEditTool));
    registry.register(Arc::new(BashTool));

    let cfg = AgentConfig {
        model: String::new(),
        system_prompt: EVAL_SYSTEM_PROMPT.into(),
        max_turns,
        permission_mode: PermissionMode::BypassPermissions,
        auto_compact_threshold: None,
        ..Default::default()
    };
    let agent_sink: Arc<dyn EventSink> = sink.clone();
    let mut agent = Agent::new(
        backend,
        registry,
        Arc::new(AllowAll),
        None,
        Arc::new(NoHooks),
        agent_sink,
        ToolContext::new(workdir.clone(), session_id),
        cfg,
    );

    let turn: Result<RunResult> = async { Ok(agent.run_turn(task.prompt).await) }.await;
    let (requests, rate_limited_429, edit_failures) = sink.snapshot();
    let duration_ms = started.elapsed().as_millis() as u64;

    let (passed, error, input_tokens, output_tokens) = match turn {
        Ok(r) => {
            let ok = !r.is_error && task.check.run(&workdir);
            (ok, None, r.usage.input_tokens, r.usage.output_tokens)
        }
        Err(e) => (false, Some(format!("{e:#}")), 0, 0),
    };

    let _ = std::fs::remove_dir_all(&workdir);

    TaskMetrics {
        task_id: task.id.into(),
        passed,
        requests,
        input_tokens,
        output_tokens,
        rate_limited_429,
        edit_failures,
        duration_ms,
        error,
    }
}

pub async fn run_eval(opts: &EvalOptions) -> Result<EvalReport> {
    if !opts.model.contains(":free") {
        bail!(
            "refusing to eval non-free model '{}': only `:free` models are allowed",
            opts.model
        );
    }
    let tasks: Vec<&EvalTask> = EVAL_TASKS
        .iter()
        .filter(|t| {
            opts.task_filter
                .as_deref()
                .map(|f| t.id.contains(f))
                .unwrap_or(true)
        })
        .collect();
    if tasks.is_empty() {
        bail!("no eval tasks match the filter");
    }

    let mut results = Vec::with_capacity(tasks.len());
    for task in tasks {
        let sink = Arc::new(MetricSink::default());
        let client_sink: Arc<dyn EventSink> = sink.clone();
        let backend: Arc<dyn LlmBackend> = Arc::new(OpenAiClient::new(
            opts.base_url.clone(),
            opts.api_key.clone(),
            client_sink,
        ));
        results.push(run_one_task(task, backend, sink, opts.max_turns).await);
    }

    let passed_count = results.iter().filter(|m| m.passed).count();
    Ok(EvalReport {
        model: opts.model.clone(),
        task_count: results.len(),
        passed_count,
        tasks: results,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use zen_core::{ChatMessage, ChatRequest, ChatResponse, FunctionCall, ToolCall, Usage};

    struct ScriptedBackend {
        queue: StdMutex<std::collections::VecDeque<ChatResponse>>,
    }

    impl ScriptedBackend {
        fn new(responses: Vec<ChatResponse>) -> Arc<Self> {
            Arc::new(Self {
                queue: StdMutex::new(responses.into_iter().collect()),
            })
        }
    }

    #[async_trait::async_trait]
    impl LlmBackend for ScriptedBackend {
        async fn chat(&self, _req: ChatRequest) -> anyhow::Result<ChatResponse> {
            self.queue
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("scripted backend exhausted"))
        }
    }

    fn text_resp(text: &str) -> ChatResponse {
        ChatResponse {
            message: ChatMessage::assistant(Some(text.into()), None),
            usage: Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                ..Default::default()
            },
            finish_reason: Some("stop".into()),
        }
    }

    fn tool_resp(name: &str, args: &str, id: &str) -> ChatResponse {
        ChatResponse {
            message: ChatMessage::assistant(
                None,
                Some(vec![ToolCall {
                    id: id.into(),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: name.into(),
                        arguments: args.into(),
                    },
                }]),
            ),
            usage: Usage {
                prompt_tokens: 20,
                completion_tokens: 8,
                total_tokens: 28,
                ..Default::default()
            },
            finish_reason: Some("tool_calls".into()),
        }
    }

    fn assistant_event(id: &str, tool: &str) -> AgentEvent {
        AgentEvent::Assistant {
            message: ChatMessage::assistant(
                None,
                Some(vec![ToolCall {
                    id: id.into(),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: tool.into(),
                        arguments: "{}".into(),
                    },
                }]),
            ),
            parent_tool_use_id: None,
        }
    }

    #[test]
    fn eval_task_list_has_ten_tasks_with_unique_ids() {
        assert_eq!(EVAL_TASKS.len(), 10);
        let mut ids: Vec<&str> = EVAL_TASKS.iter().map(|t| t.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 10);
        for t in EVAL_TASKS.iter() {
            assert!(!t.prompt.is_empty(), "task {} has empty prompt", t.id);
            assert!(
                !t.description.is_empty(),
                "task {} has empty description",
                t.id
            );
        }
    }

    #[test]
    fn metric_sink_counts_requests_and_429s() {
        let sink = MetricSink::default();
        sink.emit(assistant_event("a1", "Read"));
        sink.emit(assistant_event("a2", "Write"));
        sink.emit(AgentEvent::ApiRetry {
            attempt: 1,
            max_retries: 5,
            retry_delay_ms: 1000,
            error_status: Some(429),
            error: "rate limited".into(),
        });
        sink.emit(AgentEvent::ApiRetry {
            attempt: 1,
            max_retries: 5,
            retry_delay_ms: 1000,
            error_status: Some(500),
            error: "server error".into(),
        });
        let (requests, r429, edit_failures) = sink.snapshot();
        assert_eq!(requests, 2);
        assert_eq!(r429, 1, "only 429 (not 500) counts as rate_limited_429");
        assert_eq!(edit_failures, 0);
    }

    #[test]
    fn metric_sink_counts_edit_failures_only_for_edit_tools() {
        let sink = MetricSink::default();
        sink.emit(assistant_event("e1", "Edit"));
        sink.emit(assistant_event("r1", "Read"));

        sink.emit(AgentEvent::ToolResult {
            tool_use_id: "e1".into(),
            content: "Error: no match".into(),
            is_error: true,
            parent_tool_use_id: None,
            parallel: false,
        });

        sink.emit(AgentEvent::ToolResult {
            tool_use_id: "r1".into(),
            content: "Error: not found".into(),
            is_error: true,
            parent_tool_use_id: None,
            parallel: false,
        });

        sink.emit(assistant_event("w1", "Write"));
        sink.emit(AgentEvent::ToolResult {
            tool_use_id: "w1".into(),
            content: "ok".into(),
            is_error: false,
            parent_tool_use_id: None,
            parallel: false,
        });
        let (_, _, edit_failures) = sink.snapshot();
        assert_eq!(edit_failures, 1);
    }

    #[test]
    fn check_file_operators() {
        let dir = std::env::temp_dir().join(format!("zencode-eval-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "hello world").unwrap();

        assert!(Check::FileContains {
            path: "a.txt",
            needle: "world"
        }
        .run(&dir));
        assert!(!Check::FileContains {
            path: "a.txt",
            needle: "missing"
        }
        .run(&dir));
        assert!(Check::FileEquals {
            path: "a.txt",
            content: "hello world"
        }
        .run(&dir));
        assert!(!Check::FileEquals {
            path: "a.txt",
            content: "hello"
        }
        .run(&dir));
        assert!(Check::FileNotExists { path: "nope.txt" }.run(&dir));
        assert!(!Check::FileNotExists { path: "a.txt" }.run(&dir));
        assert!(Check::All(vec![
            Check::FileContains {
                path: "a.txt",
                needle: "hello"
            },
            Check::Not(Box::new(Check::FileContains {
                path: "a.txt",
                needle: "zzz"
            })),
        ])
        .run(&dir));
        assert!(Check::CommandOutputContains {
            cmd: "sh",
            args: &["-c", "echo hi"],
            needle: "hi",
        }
        .run(&dir));
        assert!(!Check::CommandOutputContains {
            cmd: "sh",
            args: &["-c", "echo hi"],
            needle: "bye",
        }
        .run(&dir));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn report_renders_markdown_table_and_json() {
        let report = EvalReport {
            model: "test-model:free".into(),
            task_count: 2,
            passed_count: 1,
            tasks: vec![
                TaskMetrics {
                    task_id: "t1".into(),
                    passed: true,
                    requests: 3,
                    input_tokens: 100,
                    output_tokens: 50,
                    rate_limited_429: 0,
                    edit_failures: 0,
                    duration_ms: 123,
                    error: None,
                },
                TaskMetrics {
                    task_id: "t2".into(),
                    passed: false,
                    requests: 1,
                    input_tokens: 10,
                    output_tokens: 5,
                    rate_limited_429: 2,
                    edit_failures: 1,
                    duration_ms: 45,
                    error: Some("boom".into()),
                },
            ],
        };
        let md = report.to_markdown();
        assert!(md.contains("| t1 | ✅ | 3 | 100 | 50 | 0 | 0 | 123 |"));
        assert!(md.contains("| t2 | ❌ | 1 | 10 | 5 | 2 | 1 | 45 |"));
        assert!(md.contains("1/2 tasks passed"));
        assert!(md.contains("t2 error: boom"));
        let json = report.to_json().unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["model"], "test-model:free");
        assert_eq!(v["passed_count"], 1);
        assert_eq!(v["tasks"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn run_one_task_passes_with_scripted_backend() {
        let backend = ScriptedBackend::new(vec![
            tool_resp(
                "Write",
                r#"{"path":"hello.txt","content":"Hello, eval!"}"#,
                "call-1",
            ),
            text_resp("Done."),
        ]);
        let task = &EVAL_TASKS[0];
        assert_eq!(task.id, "create-file");
        let sink = Arc::new(MetricSink::default());
        let m = run_one_task(task, backend, sink, 10).await;
        assert!(m.error.is_none(), "unexpected error: {:?}", m.error);
        assert!(m.passed, "create-file should pass with scripted Write");
        assert_eq!(m.requests, 2);
        assert_eq!(m.input_tokens, 30);
        assert_eq!(m.output_tokens, 13);
        assert_eq!(m.edit_failures, 0);
    }

    #[tokio::test]
    async fn run_one_task_records_edit_failure() {
        let backend = ScriptedBackend::new(vec![
            tool_resp(
                "Edit",
                r#"{"path":"nope.txt","old_string":"x","new_string":"y"}"#,
                "call-1",
            ),
            text_resp("Gave up."),
        ]);
        let task = &EvalTask {
            id: "edit-fail-probe",
            description: "probe",
            prompt: "Try to edit nope.txt.",
            setup: &[],
            check: Check::FileNotExists {
                path: "never-created.txt",
            },
        };
        let m = run_one_task(task, backend, Arc::new(MetricSink::default()), 10).await;

        assert_eq!(m.edit_failures, 1);
        assert!(m.passed, "check FileNotExists should hold");
    }
}
