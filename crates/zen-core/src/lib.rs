//! zen-core: shared types and traits for every zen-code crate.
//! OWNER: integrator only. Workers must NOT edit this crate; if you need a change, write it in
//! your crate's NOTES.md ("core change request") and work around it locally.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

// ───────────────────────────── chat wire types (OpenAI-compatible) ─────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub tool_type: String, // always "function"
    pub function: FunctionDefinition,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionCall {
    pub name: String,
    /// JSON-encoded arguments string, as the API sends it.
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "default_function")]
    pub call_type: String,
    pub function: FunctionCall,
}
fn default_function() -> String {
    "function".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ChatMessage {
    pub role: String, // system | user | assistant | tool
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ChatMessage {
    pub fn system(c: impl Into<String>) -> Self {
        Self { role: "system".into(), content: Some(c.into()), ..Default::default() }
    }
    pub fn user(c: impl Into<String>) -> Self {
        Self { role: "user".into(), content: Some(c.into()), ..Default::default() }
    }
    pub fn assistant(c: Option<String>, calls: Option<Vec<ToolCall>>) -> Self {
        Self { role: "assistant".into(), content: c, tool_calls: calls, ..Default::default() }
    }
    pub fn tool(id: impl Into<String>, c: impl Into<String>) -> Self {
        Self { role: "tool".into(), content: Some(c.into()), tool_call_id: Some(id.into()), ..Default::default() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDefinition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub message: ChatMessage,
    pub usage: Usage,
    pub finish_reason: Option<String>,
}

/// Anything that can answer a chat request (real HTTP client in zen-agent, mocks in tests).
#[async_trait]
pub trait LlmBackend: Send + Sync {
    async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse>;
}

// ───────────────────────────── modes & formats ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    #[default]
    Default,
    AcceptEdits,
    Plan,
    BypassPermissions,
    DontAsk,
}

impl PermissionMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::AcceptEdits => "acceptEdits",
            Self::Plan => "plan",
            Self::BypassPermissions => "bypassPermissions",
            Self::DontAsk => "dontAsk",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            // `manual` is an alias; `auto` (classifier mode) is P3 and falls back to default.
            "default" | "manual" | "auto" => Self::Default,
            "acceptEdits" => Self::AcceptEdits,
            "plan" => Self::Plan,
            "bypassPermissions" => Self::BypassPermissions,
            "dontAsk" => Self::DontAsk,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Text,
    Json,
    StreamJson,
}

// ───────────────────────────── tools ─────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}
impl ToolOutput {
    pub fn ok(c: impl Into<String>) -> Self {
        Self { content: c.into(), is_error: false }
    }
    pub fn err(c: impl Into<String>) -> Self {
        Self { content: c.into(), is_error: true }
    }
}

/// Shared per-session state handed to every tool call.
#[derive(Clone)]
pub struct ToolContext {
    /// Persistent working directory (Bash `cd` updates it).
    pub cwd: Arc<Mutex<PathBuf>>,
    /// Extra directories file tools may touch (--add-dir, permissions.additionalDirectories).
    pub extra_dirs: Vec<PathBuf>,
    /// Env vars exported to child processes (settings `env`).
    pub env: HashMap<String, String>,
    /// Absolute paths Read (or written) in this session, mapped to a stamp of the content seen
    /// then. Write/Edit require a prior Read of existing files and refuse a file whose content
    /// changed since (r7).
    pub read_files: Arc<Mutex<HashMap<PathBuf, u64>>>,
    /// Session todo list (TodoWrite replaces it).
    pub todos: Arc<Mutex<Vec<TodoItem>>>,
    pub session_id: String,
    /// Id of the tool call being executed. The agent sets it on a per-call clone (Task tool uses it
    /// as parent_tool_use_id for sub-agent events).
    pub current_tool_use_id: Option<String>,
    /// Per-session scratch directory for spilled tool output and background-job logs (p0b).
    /// The CLI sets it to the transcript path without `.jsonl`; `None` uses a temp dir.
    pub session_dir: Option<PathBuf>,
}

impl ToolContext {
    pub fn new(cwd: PathBuf, session_id: impl Into<String>) -> Self {
        Self {
            cwd: Arc::new(Mutex::new(cwd)),
            extra_dirs: vec![],
            env: HashMap::new(),
            read_files: Default::default(),
            todos: Default::default(),
            session_id: session_id.into(),
            current_tool_use_id: None,
            session_dir: None,
        }
    }
    pub fn cwd(&self) -> PathBuf {
        self.cwd.lock().unwrap().clone()
    }
    /// The session scratch directory: `session_dir`, or `<tmp>/zen-<session_id>` when unset.
    pub fn scratch_dir(&self) -> PathBuf {
        match &self.session_dir {
            Some(d) => d.clone(),
            None => {
                let id: String = self
                    .session_id
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
                    .collect();
                std::env::temp_dir().join(format!("zen-{}", if id.is_empty() { "session".into() } else { id }))
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TodoItem {
    pub content: String,
    pub status: String, // pending | in_progress | completed
    #[serde(default, rename = "activeForm", skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    /// Our own description text (clean-room; write it yourself).
    fn description(&self) -> &str;
    /// JSON Schema object for the arguments.
    fn parameters_schema(&self) -> Value;
    /// Read-only tools never need permission in `default` mode.
    fn is_read_only(&self) -> bool;
    async fn call(&self, args: Value, ctx: &ToolContext) -> ToolOutput;

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionDefinition {
                name: self.name().to_string(),
                description: self.description().to_string(),
                parameters: self.parameters_schema(),
            },
        }
    }
}

/// Ordered tool set. Duplicate names: last registration wins.
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}
impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn register(&mut self, t: Arc<dyn Tool>) {
        self.tools.retain(|x| x.name() != t.name());
        self.tools.push(t);
    }
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.name() == name).cloned()
    }
    pub fn names(&self) -> Vec<String> {
        self.tools.iter().map(|t| t.name().to_string()).collect()
    }
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.iter().map(|t| t.definition()).collect()
    }
    /// Keep only tools whose name is in `allowed` (used for sub-agents / --tools).
    pub fn filtered(&self, allowed: &[String]) -> Self {
        Self { tools: self.tools.iter().filter(|t| allowed.iter().any(|a| a == t.name())).cloned().collect() }
    }
}

// ───────────────────────────── permissions ─────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum PermissionDecision {
    Allow,
    Deny(String),
    /// Needs a human; the agent asks via `Prompter` or denies in headless mode.
    Ask,
}

pub trait PermissionChecker: Send + Sync {
    fn check(&self, tool_name: &str, input: &Value, read_only: bool, ctx: &ToolContext) -> PermissionDecision;
    /// Persist an "always allow" rule (REPL answer "a").
    fn add_allow_rule(&self, _rule: &str) {}
    /// True when an explicit `ask` rule matches this call. A PreToolUse hook `allow` may skip
    /// a mode-default prompt but never an ask rule (SPEC C2.13).
    fn ask_rule_matches(&self, _tool_name: &str, _input: &Value, _ctx: &ToolContext) -> bool {
        false
    }
}

/// Interactive yes/no/always. Headless runs pass None.
pub trait Prompter: Send + Sync {
    fn ask_permission(&self, tool_name: &str, input: &Value) -> PromptAnswer;
}
#[derive(Debug, Clone, PartialEq)]
pub enum PromptAnswer {
    Yes,
    Always(String),
    No,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PermissionDenial {
    pub tool_name: String,
    pub tool_use_id: String,
    pub tool_input: Value,
}

// ───────────────────────────── hooks ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    UserPromptSubmit,
    Stop,
    SubagentStop,
    SessionStart,
    SessionEnd,
    PreCompact,
    Notification,
    /// A tool call returned an error result (parity C3.2).
    PostToolUseFailure,
    /// A sub-agent is about to start (matcher target = agent type).
    SubagentStart,
    /// Compaction finished (matcher target = "manual" | "auto").
    PostCompact,
    /// A permission prompt is about to be shown / decided (P2).
    PermissionRequest,
    /// A tool call was denied by permissions (P2).
    PermissionDenied,
}
impl HookEvent {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::Stop => "Stop",
            Self::SubagentStop => "SubagentStop",
            Self::SessionStart => "SessionStart",
            Self::SessionEnd => "SessionEnd",
            Self::PreCompact => "PreCompact",
            Self::Notification => "Notification",
            Self::PostToolUseFailure => "PostToolUseFailure",
            Self::SubagentStart => "SubagentStart",
            Self::PostCompact => "PostCompact",
            Self::PermissionRequest => "PermissionRequest",
            Self::PermissionDenied => "PermissionDenied",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct HookOutcome {
    /// Exit 2 or decision=block / permissionDecision=deny.
    pub blocked: bool,
    /// Text to feed back to the model (stderr on block, or additional context on success).
    pub feedback: Option<String>,
    /// PreToolUse JSON may force allow/deny/ask.
    pub permission_override: Option<PermissionDecision>,
    /// `{"continue": false}` → stop the whole run.
    pub stop_run: bool,
    /// `stopReason` accompanying `continue: false` (shown to the user).
    pub stop_reason: Option<String>,
    /// PreToolUse `hookSpecificOutput.updatedInput`: replaces the tool input before execution.
    pub updated_input: Option<Value>,
    /// PostToolUse `hookSpecificOutput.updatedToolOutput`: replaces the tool result text.
    pub updated_output: Option<String>,
    /// `systemMessage`: warning for the user (stderr in REPL, system line in stream-json).
    pub system_message: Option<String>,
}

#[async_trait]
pub trait HookRunner: Send + Sync {
    /// `matcher_target` = tool name for tool events, trigger for SessionStart/PreCompact, else "".
    /// `payload` = event-specific JSON fields; the runner adds session_id, cwd, hook_event_name, etc.
    async fn run(&self, event: HookEvent, matcher_target: &str, payload: Value) -> HookOutcome;
}

/// No-op hooks for tests / --bare.
pub struct NoHooks;
#[async_trait]
impl HookRunner for NoHooks {
    async fn run(&self, _: HookEvent, _: &str, _: Value) -> HookOutcome {
        HookOutcome::default()
    }
}

// ───────────────────────────── events (stream-json / REPL rendering) ─────────────────────────────

/// Everything the agent loop reports. zen-cli turns these into stream-json lines or REPL output.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    /// Assistant message (text and/or tool calls). parent = Task tool_call id for sub-agents.
    Assistant { message: ChatMessage, parent_tool_use_id: Option<String> },
    /// One tool result.
    ToolResult { tool_use_id: String, content: String, is_error: bool, parent_tool_use_id: Option<String> },
    /// Partial assistant text delta (when streaming is on).
    TextDelta { text: String },
    ApiRetry { attempt: u32, max_retries: u32, retry_delay_ms: u64, error_status: Option<u16>, error: String },
    CompactBoundary { trigger: String, pre_tokens: u64 },
    PermissionDenied(PermissionDenial),
    /// A hook's `systemMessage` (or a non-fatal hook warning) meant for the user, not the model.
    /// `event` is the hook event name that produced it.
    SystemMessage { event: String, text: String },
}

pub trait EventSink: Send + Sync {
    fn emit(&self, ev: AgentEvent);
}
pub struct NullSink;
impl EventSink for NullSink {
    fn emit(&self, _: AgentEvent) {}
}

// ───────────────────────────── results ─────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct UsageStats {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// The final `{"type":"result",...}` object (SPEC §14).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunResult {
    #[serde(rename = "type")]
    pub record_type: String, // "result"
    pub subtype: String,     // success | error_max_turns | error_during_execution
    pub is_error: bool,
    pub duration_ms: u64,
    pub duration_api_ms: u64,
    pub num_turns: usize,
    pub result: String,
    pub session_id: String,
    pub total_cost_usd: f64,
    pub usage: UsageStats,
    pub model: String,
    pub permission_denials: Vec<PermissionDenial>,
}

// ───────────────────────────── settings, memory, skills, agents ─────────────────────────────

/// Fully merged settings (zen-ext does the layering). Unknown keys kept in `extra`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub fallback_models: Vec<String>,
    #[serde(default)]
    pub permissions: PermissionSettings,
    /// Raw hooks object: { "PreToolUse": [ { "matcher": "...", "hooks": [ {type, command, timeout} ] } ] }
    #[serde(default)]
    pub hooks: Value,
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// false = off, true/None = auto (80% of context), number = token threshold.
    #[serde(default)]
    pub auto_compact: Option<Value>,
    #[serde(default)]
    pub max_turns: Option<usize>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PermissionSettings {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub ask: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub default_mode: Option<String>,
    #[serde(default)]
    pub additional_directories: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub path: PathBuf, // the SKILL.md
    pub body: String,  // markdown after frontmatter
    pub allowed_tools: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AgentDefinition {
    pub name: String,
    pub description: String,
    pub tools: Option<Vec<String>>, // None = inherit all
    pub model: Option<String>,      // None or "inherit" = parent model
    pub system_prompt: String,
    /// Tools removed from the (inherited or listed) set (SPEC C6.1 `disallowedTools`).
    pub disallowed_tools: Vec<String>,
    /// Permission mode override for the sub-agent (`permissionMode`), e.g. "plan".
    pub permission_mode: Option<String>,
    /// Turn cap for the sub-agent (`maxTurns`); None = parent's cap.
    pub max_turns: Option<usize>,
    /// Skill names preloaded into the sub-agent's system prompt (`skills`).
    pub skills: Vec<String>,
}

/// Runs a sub-agent to completion and returns its final text. Implemented in zen-agent,
/// consumed by the Task tool in zen-mcp (avoids a crate cycle).
#[async_trait]
pub trait SubAgentRunner: Send + Sync {
    async fn run_subagent(
        &self,
        def: &AgentDefinition,
        prompt: &str,
        parent_tool_use_id: &str,
    ) -> anyhow::Result<String>;
}

/// Rough token estimate when the API gives no usage (chars / 4).
pub fn estimate_tokens(messages: &[ChatMessage]) -> u64 {
    messages
        .iter()
        .map(|m| {
            m.content.as_deref().map(|c| c.len()).unwrap_or(0)
                + m.tool_calls.as_ref().map(|v| v.iter().map(|c| c.function.arguments.len() + c.function.name.len()).sum()).unwrap_or(0)
        })
        .sum::<usize>() as u64
        / 4
}

/// PEM bundle named by `SSL_CERT_FILE` (if set and readable). HTTP clients add these as extra
/// trusted roots so zen works behind TLS-inspecting proxies, like curl/openssl do.
pub fn extra_root_pem() -> Option<Vec<u8>> {
    let p = std::env::var_os("SSL_CERT_FILE")?;
    std::fs::read(p).ok()
}

// ───────────────────────────── shell resolver ─────────────────────────────

/// Which syntax a resolved shell speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    /// bash / sh: `-c <script>`.
    Posix,
    /// pwsh / powershell: `-NoProfile -NonInteractive -Command <script>`.
    PowerShell,
    /// cmd.exe: `/C <script>`.
    Cmd,
}

/// A shell program plus its syntax (perf p0, item 7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellSpec {
    pub program: String,
    pub kind: ShellKind,
}

impl ShellSpec {
    /// Arguments that make this shell run `script`.
    pub fn args(&self, script: &str) -> Vec<String> {
        match self.kind {
            ShellKind::Posix => vec!["-c".into(), script.into()],
            ShellKind::PowerShell => {
                vec!["-NoProfile".into(), "-NonInteractive".into(), "-Command".into(), script.into()]
            }
            ShellKind::Cmd => vec!["/C".into(), script.into()],
        }
    }
}

/// Pick the shell for running `preferred` (`"bash"` or `"sh"`) scripts. On Unix this is always
/// `preferred` itself (behavior unchanged). On Windows: `ZEN_SHELL` if set, else `preferred`
/// when it is on PATH (Git Bash, MSYS), else pwsh, powershell, and finally `cmd`.
/// `on_path` answers whether a program name resolves on PATH (injected for tests).
pub fn pick_shell(
    preferred: &str,
    windows: bool,
    zen_shell: Option<&str>,
    on_path: &dyn Fn(&str) -> bool,
) -> ShellSpec {
    let posix = |p: &str| ShellSpec { program: p.to_string(), kind: ShellKind::Posix };
    if !windows {
        return posix(preferred);
    }
    let kind_of = |p: &str| {
        // Split on both separators so a Windows path classifies the same on any host.
        let file = p.rsplit(['/', '\\']).next().unwrap_or(p).to_ascii_lowercase();
        let base = file.strip_suffix(".exe").unwrap_or(&file).to_string();
        match base.as_str() {
            "pwsh" | "powershell" => ShellKind::PowerShell,
            "cmd" => ShellKind::Cmd,
            _ => ShellKind::Posix,
        }
    };
    if let Some(z) = zen_shell.map(str::trim).filter(|z| !z.is_empty()) {
        return ShellSpec { program: z.to_string(), kind: kind_of(z) };
    }
    for cand in [preferred, "bash", "pwsh", "powershell"] {
        if on_path(cand) {
            return ShellSpec { program: cand.to_string(), kind: kind_of(cand) };
        }
    }
    ShellSpec { program: "cmd".into(), kind: ShellKind::Cmd }
}

/// True when `name` (plus `.exe` on Windows) is a file in some PATH directory.
pub fn program_on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path).any(|dir| {
        dir.join(name).is_file() || (cfg!(windows) && dir.join(format!("{}.exe", name)).is_file())
    })
}

/// The shell to use for `preferred` scripts on this machine (cached per name).
pub fn resolve_shell(preferred: &str) -> ShellSpec {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, ShellSpec>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(s) = cache.lock().unwrap().get(preferred) {
        return s.clone();
    }
    let zen_shell = std::env::var("ZEN_SHELL").ok();
    let spec = pick_shell(preferred, cfg!(windows), zen_shell.as_deref(), &program_on_path);
    cache.lock().unwrap().insert(preferred.to_string(), spec.clone());
    spec
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn message_serializes_without_nulls() {
        let s = serde_json::to_string(&ChatMessage::user("hi")).unwrap();
        assert_eq!(s, r#"{"role":"user","content":"hi"}"#);
    }
    #[test]
    fn settings_parse_camel_case_and_extra() {
        let s: Settings = serde_json::from_str(r#"{"model":"m","baseUrl":"u","permissions":{"allow":["Read"],"defaultMode":"plan"},"foo":1}"#).unwrap();
        assert_eq!(s.base_url.as_deref(), Some("u"));
        assert_eq!(s.permissions.allow, vec!["Read"]);
        assert_eq!(s.extra.get("foo"), Some(&serde_json::json!(1)));
    }
    #[test]
    fn mode_roundtrip() {
        for m in ["default", "acceptEdits", "plan", "bypassPermissions", "dontAsk"] {
            assert_eq!(PermissionMode::parse(m).unwrap().as_str(), m);
        }
    }

    #[test]
    fn p0_7_shell_unix_is_unchanged() {
        let none = |_: &str| false;
        assert_eq!(pick_shell("bash", false, Some("pwsh"), &none), ShellSpec { program: "bash".into(), kind: ShellKind::Posix });
        assert_eq!(pick_shell("sh", false, None, &none).program, "sh");
    }
    #[test]
    fn p0_7_shell_windows_fallback_order() {
        let only = |names: &'static [&'static str]| move |n: &str| names.contains(&n);
        assert_eq!(pick_shell("bash", true, None, &only(&["bash", "pwsh"])).kind, ShellKind::Posix);
        let s = pick_shell("bash", true, None, &only(&["pwsh", "powershell"]));
        assert_eq!((s.program.as_str(), s.kind), ("pwsh", ShellKind::PowerShell));
        let s = pick_shell("bash", true, None, &only(&["powershell"]));
        assert_eq!((s.program.as_str(), s.kind), ("powershell", ShellKind::PowerShell));
        let s = pick_shell("sh", true, None, &only(&[]));
        assert_eq!((s.program.as_str(), s.kind), ("cmd", ShellKind::Cmd));
        let s = pick_shell("bash", true, Some(r"C:\Program Files\PowerShell\7\pwsh.exe"), &only(&["bash"]));
        assert_eq!(s.kind, ShellKind::PowerShell);
        assert_eq!(ShellSpec { program: "cmd".into(), kind: ShellKind::Cmd }.args("dir"), vec!["/C", "dir"]);
        assert_eq!(ShellSpec { program: "bash".into(), kind: ShellKind::Posix }.args("ls"), vec!["-c", "ls"]);
    }
}
