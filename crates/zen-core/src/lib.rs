use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: FunctionDefinition,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionCall {
    pub name: String,

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
    pub role: String,
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

    #[serde(default)]
    pub cache_read_input_tokens: u64,

    #[serde(default)]
    pub cache_creation_input_tokens: u64,
}

pub fn cache_hit_rate(cache_read_input_tokens: u64, prompt_tokens: u64) -> f64 {
    if prompt_tokens == 0 {
        return 0.0;
    }
    (cache_read_input_tokens as f64 / prompt_tokens as f64).min(1.0)
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

#[async_trait]
pub trait LlmBackend: Send + Sync {
    async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse>;
}

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

#[derive(Clone)]
pub struct ToolContext {

    pub cwd: Arc<Mutex<PathBuf>>,

    pub extra_dirs: Vec<PathBuf>,

    pub env: HashMap<String, String>,

    pub api_key_env: Option<String>,

    pub allow_private_network: bool,

    pub read_files: Arc<Mutex<HashMap<PathBuf, u64>>>,

    pub todos: Arc<Mutex<Vec<TodoItem>>>,
    pub session_id: String,

    pub current_tool_use_id: Option<String>,

    pub session_dir: Option<PathBuf>,

    pub tool_timeout_ms: Option<u64>,

    pub checkpoints: Option<Arc<dyn FileCheckpoints>>,
}

pub trait FileCheckpoints: Send + Sync {

    fn begin_turn(&self, prompt: &str);

    fn before_write(&self, path: &Path) -> Option<String>;

    fn after_write(&self, path: &Path, content: &[u8]);
}

impl ToolContext {
    pub fn new(cwd: PathBuf, session_id: impl Into<String>) -> Self {
        Self {
            cwd: Arc::new(Mutex::new(cwd)),
            extra_dirs: vec![],
            env: HashMap::new(),
            api_key_env: None,
            allow_private_network: false,
            read_files: Default::default(),
            todos: Default::default(),
            session_id: session_id.into(),
            current_tool_use_id: None,
            session_dir: None,
            tool_timeout_ms: None,
            checkpoints: None,
        }
    }
    pub fn cwd(&self) -> PathBuf {
        self.cwd.lock().unwrap().clone()
    }

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
    pub status: String,
    #[serde(default, rename = "activeForm", skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;

    fn description(&self) -> &str;

    fn parameters_schema(&self) -> Value;

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

    pub fn filtered(&self, allowed: &[String]) -> Self {
        Self { tools: self.tools.iter().filter(|t| allowed.iter().any(|a| a == t.name())).cloned().collect() }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PermissionDecision {
    Allow,
    Deny(String),

    Ask,
}

pub trait PermissionChecker: Send + Sync {
    fn check(&self, tool_name: &str, input: &Value, read_only: bool, ctx: &ToolContext) -> PermissionDecision;

    fn add_allow_rule(&self, _rule: &str) {}

    fn ask_rule_matches(&self, _tool_name: &str, _input: &Value, _ctx: &ToolContext) -> bool {
        false
    }
}

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

    PostToolUseFailure,

    SubagentStart,

    PostCompact,

    PermissionRequest,

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

    pub blocked: bool,

    pub feedback: Option<String>,

    pub permission_override: Option<PermissionDecision>,

    pub stop_run: bool,

    pub stop_reason: Option<String>,

    pub updated_input: Option<Value>,

    pub updated_output: Option<String>,

    pub system_message: Option<String>,
}

#[async_trait]
pub trait HookRunner: Send + Sync {

    async fn run(&self, event: HookEvent, matcher_target: &str, payload: Value) -> HookOutcome;
}

pub struct NoHooks;
#[async_trait]
impl HookRunner for NoHooks {
    async fn run(&self, _: HookEvent, _: &str, _: Value) -> HookOutcome {
        HookOutcome::default()
    }
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum AgentEvent {

    Assistant { message: ChatMessage, parent_tool_use_id: Option<String> },

    ToolResult { tool_use_id: String, content: String, is_error: bool, parent_tool_use_id: Option<String>, parallel: bool },

    TextDelta { text: String },

    ToolProgress { tool_use_id: String, tool_name: String, elapsed_ms: u64 },
    ApiRetry { attempt: u32, max_retries: u32, retry_delay_ms: u64, error_status: Option<u16>, error: String },
    CompactBoundary { trigger: String, pre_tokens: u64 },

    Pruned { pruned: usize, tokens_saved: u64 },
    PermissionDenied(PermissionDenial),

    SystemMessage { event: String, text: String },
}

pub trait EventSink: Send + Sync {
    fn emit(&self, ev: AgentEvent);
}
pub struct NullSink;
impl EventSink for NullSink {
    fn emit(&self, _: AgentEvent) {}
}

pub trait ToolJournal: Send + Sync {

    fn assistant(&self, message: &ChatMessage);

    fn tool_result(&self, tool_use_id: &str, content: &str, is_error: bool, final_result: bool);
}

pub const CACHE_VOLATILE_MARKER: &str = "\n\n<!-- zen-cache: volatile context below -->\n\n";

pub fn split_stable_volatile(prompt: &str) -> (&str, &str) {
    match prompt.split_once(CACHE_VOLATILE_MARKER) {
        Some((stable, volatile)) => (stable, volatile),
        None => (prompt, ""),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct UsageStats {
    pub input_tokens: u64,
    pub output_tokens: u64,

    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

impl UsageStats {

    pub fn cache_hit_rate(&self) -> f64 {
        cache_hit_rate(self.cache_read_input_tokens, self.input_tokens)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunResult {
    #[serde(rename = "type")]
    pub record_type: String,
    pub subtype: String,
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

    #[serde(default)]
    pub hooks: Value,
    #[serde(default)]
    pub env: HashMap<String, String>,

    #[serde(default)]
    pub auto_compact: Option<Value>,

    #[serde(default)]
    pub auto_commit: Option<bool>,
    #[serde(default)]
    pub max_turns: Option<usize>,
    #[serde(default)]
    pub context_window: Option<u64>,

    #[serde(default)]
    pub allow_private_network: bool,

    #[serde(default)]
    pub rate_gate: RateGateFileConfig,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RateGateFileConfig {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub daily_limit: Option<u64>,
    #[serde(default)]
    pub limits: HashMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PermissionSettings {    #[serde(default)]
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
    pub path: PathBuf,
    pub body: String,
    pub allowed_tools: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AgentDefinition {
    pub name: String,
    pub description: String,
    pub tools: Option<Vec<String>>,
    pub model: Option<String>,
    pub system_prompt: String,

    pub disallowed_tools: Vec<String>,

    pub permission_mode: Option<String>,

    pub max_turns: Option<usize>,

    pub skills: Vec<String>,
}

#[async_trait]
pub trait SubAgentRunner: Send + Sync {
    async fn run_subagent(
        &self,
        def: &AgentDefinition,
        prompt: &str,
        parent_tool_use_id: &str,
    ) -> anyhow::Result<String>;
}

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

pub fn extra_root_pem() -> Option<Vec<u8>> {
    let p = std::env::var_os("SSL_CERT_FILE")?;
    std::fs::read(p).ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {

    Posix,

    PowerShell,

    Cmd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellSpec {
    pub program: String,
    pub kind: ShellKind,
}

impl ShellSpec {

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

pub fn program_on_path(name: &str) -> bool {
    find_program(name).is_some()
}

pub fn find_program(name: &str) -> Option<PathBuf> {
    let system_root = std::env::var("SystemRoot").or_else(|_| std::env::var("WINDIR")).ok();
    let found = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).find_map(|dir| {
            let mut cands = vec![dir.join(name)];
            if cfg!(windows) {
                cands.insert(0, dir.join(format!("{}.exe", name)));
            }
            cands.into_iter().find(|p| {
                p.is_file() && !(cfg!(windows) && is_wsl_launcher(p, system_root.as_deref()))
            })
        })
    });
    if found.is_some() || !cfg!(windows) || !matches!(name, "bash" | "sh") {
        return found;
    }
    ["ProgramFiles", "ProgramW6432"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .map(|pf| PathBuf::from(pf).join("Git").join("bin").join(format!("{}.exe", name)))
        .find(|p| p.is_file())
}

pub fn is_wsl_launcher(path: &Path, system_root: Option<&str>) -> bool {
    let Some(root) = system_root.map(str::trim).filter(|r| !r.is_empty()) else { return false };
    let norm = |s: &str| s.replace('/', "\\").trim_end_matches('\\').to_ascii_lowercase();
    let p = norm(&path.to_string_lossy());
    let file = p.rsplit('\\').next().unwrap_or(&p);
    let base = file.strip_suffix(".exe").unwrap_or(file);
    matches!(base, "bash" | "sh") && p.starts_with(&format!("{}\\", norm(root)))
}

pub fn api_key_env_names(api_key_env: Option<&str>) -> Vec<String> {
    let mut names = vec!["ZEN_API_KEY".to_string(), "OPENAI_API_KEY".to_string()];
    if let Some(n) = api_key_env {
        let n = n.trim();
        if !n.is_empty() && !names.iter().any(|x| x == n) {
            names.push(n.to_string());
        }
    }
    names
}

pub fn resolve_shell(preferred: &str) -> ShellSpec {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, ShellSpec>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(s) = cache.lock().unwrap().get(preferred) {
        return s.clone();
    }
    let zen_shell = std::env::var("ZEN_SHELL").ok();
    let mut spec = pick_shell(preferred, cfg!(windows), zen_shell.as_deref(), &program_on_path);

    if cfg!(windows) && zen_shell.as_deref().map(str::trim).is_none_or(str::is_empty) {
        if let Some(p) = find_program(&spec.program) {
            spec.program = p.to_string_lossy().into_owned();
        }
    }
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
    #[test]
    fn windows_bash_skips_wsl_launcher() {
        let root = Some(r"C:\Windows");
        assert!(is_wsl_launcher(Path::new(r"C:\Windows\System32\bash.exe"), root));
        assert!(is_wsl_launcher(Path::new(r"c:/windows/system32/BASH.EXE"), root));
        assert!(is_wsl_launcher(Path::new(r"C:\WINDOWS\system32\sh.exe"), Some(r"C:\Windows\")));
        assert!(!is_wsl_launcher(Path::new(r"C:\Program Files\Git\bin\bash.exe"), root));
        assert!(!is_wsl_launcher(Path::new(r"C:\Windows\System32\cmd.exe"), root));
        assert!(!is_wsl_launcher(Path::new(r"C:\WindowsApps\bash.exe"), root));
        assert!(!is_wsl_launcher(Path::new(r"C:\Windows\System32\bash.exe"), None));
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    #[test]
    fn split_stable_volatile_with_marker() {
        let p = format!("stable{}volatile", CACHE_VOLATILE_MARKER);
        let (s, v) = split_stable_volatile(&p);
        assert_eq!(s, "stable");
        assert_eq!(v, "volatile");
    }

    #[test]
    fn split_stable_volatile_without_marker_is_all_stable() {
        let (s, v) = split_stable_volatile("just a prompt");
        assert_eq!(s, "just a prompt");
        assert_eq!(v, "");
    }

    #[test]
    fn cache_hit_rate_math() {
        assert_eq!(cache_hit_rate(0, 0), 0.0);
        assert_eq!(cache_hit_rate(0, 100), 0.0);
        assert!((cache_hit_rate(75, 100) - 0.75).abs() < 1e-9);

        assert_eq!(cache_hit_rate(150, 100), 1.0);
    }

    #[test]
    fn usage_stats_cache_hit_rate() {
        let u = UsageStats { input_tokens: 1000, output_tokens: 10, cache_read_input_tokens: 900 };
        assert!((u.cache_hit_rate() - 0.9).abs() < 1e-9);
        assert_eq!(UsageStats::default().cache_hit_rate(), 0.0);
    }

    #[test]
    fn usage_deserializes_old_json_without_cache_fields() {

        let u: Usage = serde_json::from_str(r#"{"prompt_tokens":5,"completion_tokens":2,"total_tokens":7}"#).unwrap();
        assert_eq!(u.cache_read_input_tokens, 0);
        assert_eq!(u.cache_creation_input_tokens, 0);
        let s: UsageStats = serde_json::from_str(r#"{"input_tokens":5,"output_tokens":2}"#).unwrap();
        assert_eq!(s.cache_read_input_tokens, 0);
    }
}

pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pat: Vec<&str> = pattern.split('/').collect();
    let segs: Vec<&str> = path.split('/').collect();
    match_segments(&pat, &segs)
}

fn match_segments(pat: &[&str], path: &[&str]) -> bool {
    if pat.is_empty() {
        return path.is_empty();
    }
    if pat[0] == "**" {

        return (0..=path.len()).any(|i| match_segments(&pat[1..], &path[i..]));
    }
    if path.is_empty() || !match_segment(pat[0], path[0]) {
        return false;
    }
    match_segments(&pat[1..], &path[1..])
}

fn match_segment(pat: &str, s: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let c: Vec<char> = s.chars().collect();
    let (mut pi, mut si) = (0usize, 0usize);
    let (mut star, mut ss) = (None, 0usize);
    while si < c.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == c[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            pi += 1;
            ss = si;
        } else if let Some(sp) = star {
            pi = sp + 1;
            ss += 1;
            si = ss;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod glob_tests {
    use super::glob_match;

    #[test]
    fn glob_basics() {
        assert!(glob_match("src/**", "src/a/b.rs"));
        assert!(glob_match("src/**", "src/x.rs"));
        assert!(!glob_match("src/**", "other/x.rs"));
        assert!(glob_match("exact/path.rs", "exact/path.rs"));
        assert!(!glob_match("exact/path.rs", "exact/other.rs"));
    }

    #[test]
    fn glob_star_stays_in_segment() {
        assert!(glob_match("*.md", "README.md"));
        assert!(!glob_match("*.md", "docs/a.md"));
        assert!(glob_match("docs/*.md", "docs/a.md"));
        assert!(!glob_match("docs/*.md", "docs/sub/a.md"));
        assert!(glob_match("src/*.rs", "src/main.rs"));
    }

    #[test]
    fn glob_double_star_spans_segments() {
        assert!(glob_match("**/*.rs", "a/b/c.rs"));
        assert!(glob_match("**/*.rs", "c.rs"));
        assert!(glob_match("a/**/z.rs", "a/z.rs"));
        assert!(glob_match("a/**/z.rs", "a/b/c/z.rs"));
        assert!(!glob_match("a/**/z.rs", "a/b/c/y.rs"));
    }

    #[test]
    fn glob_question_mark() {
        assert!(glob_match("src/?ain.rs", "src/main.rs"));
        assert!(!glob_match("src/?ain.rs", "src/xxain.rs"));
        assert!(!glob_match("src/?ain.rs", "src/main.tsx"));
    }
}
