//! Command-line surface for the `zen` binary (SPEC §1, Appendix B1).
//!
//! Every user-facing flag and subcommand is declared here as a clap derive struct so that
//! `zen --help` and argument validation come for free. This module only *describes* the CLI;
//! turning the parsed `Cli` into a resolved runtime configuration happens in [`crate::settings`],
//! and dispatch to headless/REPL/subcommand behaviour happens in [`crate::run_with_home`].
//!
//! Key design decision: the positional `prompt` is a plain `Vec<String>` (words joined with
//! spaces) rather than `trailing_var_arg`. That lets flags such as `--output-format json` appear
//! *after* the prompt, which the integrator explicitly requires.

use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
    #[value(name = "stream-json")]
    StreamJson,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum InputFormat {
    Text,
    #[value(name = "stream-json")]
    StreamJson,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    Default,
    #[value(name = "acceptEdits")]
    AcceptEdits,
    Plan,
    #[value(name = "bypassPermissions")]
    BypassPermissions,
    #[value(name = "dontAsk")]
    DontAsk,
}

impl PermissionMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            PermissionMode::Default => "default",
            PermissionMode::AcceptEdits => "acceptEdits",
            PermissionMode::Plan => "plan",
            PermissionMode::BypassPermissions => "bypassPermissions",
            PermissionMode::DontAsk => "dontAsk",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            // `manual` is an alias of default; `auto` (classifier) is P3 → default (C2.11).
            "default" | "manual" | "auto" => PermissionMode::Default,
            "acceptEdits" => PermissionMode::AcceptEdits,
            "plan" => PermissionMode::Plan,
            "bypassPermissions" => PermissionMode::BypassPermissions,
            "dontAsk" => PermissionMode::DontAsk,
            _ => return None,
        })
    }
}

/// `--permission-mode` values: the five modes plus `manual` (alias of default) and `auto`, which
/// zen does not implement and runs as default with a warning (SPEC C2.11).
fn parse_tool_bundle(s: &str) -> Result<zen_agent::ToolBundle, String> {
    s.parse::<zen_agent::ToolBundle>()
}

fn parse_permission_mode_flag(s: &str) -> Result<PermissionMode, String> {
    if s == "auto" {
        eprintln!("warning: permission mode 'auto' is not supported; using 'default'");
    }
    PermissionMode::parse(s).ok_or_else(|| {
        format!(
            "invalid permission mode '{}' (expected one of: default, manual, acceptEdits, plan, \
             dontAsk, bypassPermissions)",
            s
        )
    })
}

impl std::fmt::Display for PermissionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            PermissionMode::Default => "default",
            PermissionMode::AcceptEdits => "acceptEdits",
            PermissionMode::Plan => "plan",
            PermissionMode::BypassPermissions => "bypassPermissions",
            PermissionMode::DontAsk => "dontAsk",
        };
        write!(f, "{}", s)
    }
}

/// `--autocompact` value parser: `auto`, `off` or a positive token count.
fn parse_autocompact(s: &str) -> Result<String, String> {
    match s {
        "auto" | "off" => Ok(s.to_string()),
        n if n.parse::<u64>().map(|v| v > 0).unwrap_or(false) => Ok(n.to_string()),
        _ => Err(format!("invalid --autocompact '{}': use auto, off or a token count", s)),
    }
}

/// `--session-id` value parser: a valid UUID, normalised to lowercase hyphenated form (C9.2).
fn parse_session_uuid(s: &str) -> Result<String, String> {
    uuid::Uuid::parse_str(s.trim())
        .map(|u| u.hyphenated().to_string())
        .map_err(|_| format!("invalid session id '{}': must be a valid UUID", s))
}

#[derive(Parser, Debug, Clone)]
#[command(name = "zen", version, about = "zen-code CLI coding agent")]
pub struct Cli {
    /// Headless: run one prompt and exit
    #[arg(short = 'p', long = "print")]
    pub print: bool,

    /// Output format (with -p): text, json, stream-json
    #[arg(long = "output-format", value_enum, default_value_t = OutputFormat::Text)]
    pub output_format: OutputFormat,

    /// Input format (with -p): text, stream-json
    #[arg(long = "input-format", value_enum, default_value_t = InputFormat::Text)]
    pub input_format: InputFormat,

    /// Emit partial stream events in stream-json mode.
    /// Required for streaming UIs that render token-by-token output; without it,
    /// only complete assistant messages appear in the stream. Deltas are
    /// provisional: the following full `assistant` record is authoritative —
    /// replace (not append) accumulated deltas when it arrives.
    #[arg(long = "include-partial-messages")]
    pub include_partial_messages: bool,

    /// Extra diagnostics on stderr; in stream-json, emit every message
    #[arg(long = "verbose")]
    pub verbose: bool,

    /// Continue most recent session for current directory
    #[arg(short = 'c', long = "continue")]
    pub continue_session: bool,

    /// Resume a session by id or path
    #[arg(short = 'r', long = "resume", num_args = 0..=1, default_missing_value = "")]
    pub resume: Option<String>,

    /// Auto-compaction: `auto` (80% of the context window), `off`, or a token threshold
    /// (overrides the `autoCompact` setting, SPEC C10.1)
    #[arg(long = "autocompact", value_parser = parse_autocompact)]
    pub autocompact: Option<String>,

    /// Prune old tool results (kills the N× re-billing multiplier); on by default.
    /// `--no-prune` disables.
    #[arg(long = "prune", overrides_with = "no_prune")]
    pub prune: bool,

    /// Disable tool-result pruning.
    #[arg(long = "no-prune", overrides_with = "prune")]
    pub no_prune: bool,

    /// Prune tool results older than N turns (default 5; never prunes the last 2 turns)
    #[arg(long = "prune-after")]
    pub prune_after: Option<usize>,

    /// Truncate tool results larger than N tokens with a re-read hint (default 8000)
    #[arg(long = "max-result-tokens")]
    pub max_result_tokens: Option<u64>,

    /// Use a fixed session id (must be a valid UUID, SPEC C9.2)
    #[arg(long = "session-id", value_parser = parse_session_uuid)]
    pub session_id: Option<String>,

    /// With resume/continue: copy history into a new session id
    #[arg(long = "fork-session")]
    pub fork_session: bool,

    /// Do not write transcript (only with -p)
    #[arg(long = "no-session-persistence")]
    pub no_session_persistence: bool,

    /// Override model
    #[arg(long = "model")]
    pub model: Option<String>,

    /// Model for `/btw` side questions (a cheap model is recommended);
    /// defaults to the session model
    #[arg(long = "btw-model")]
    pub btw_model: Option<String>,

    /// OpenAI-compatible endpoint base URL
    #[arg(long = "base-url")]
    pub base_url: Option<String>,

    /// Provider shortcut: "ollama" routes to the local Ollama instance
    /// (same as --model ollama/<name>)
    #[arg(long = "provider")]
    pub provider: Option<String>,

    /// Permission mode
    #[arg(long = "permission-mode", value_parser = parse_permission_mode_flag)]
    pub permission_mode: Option<PermissionMode>,

    /// Allowed tool permission rules (comma/space separated)
    #[arg(long = "allowedTools", visible_alias = "allowed-tools", num_args = 1..)]
    pub allowed_tools: Vec<String>,

    /// Disallowed tool permission rules (comma/space separated)
    #[arg(long = "disallowedTools", visible_alias = "disallowed-tools", num_args = 1..)]
    pub disallowed_tools: Vec<String>,

    /// Built-in tool set: "" = none, "default" = all, "Read,Bash" = only those,
    /// "-Bash,-Write" / "+Name" = default set minus/plus (no mixing with plain names)
    #[arg(long = "tools", allow_hyphen_values = true)]
    pub tools: Option<String>,

    /// Replace built-in system prompt
    #[arg(long = "system-prompt")]
    pub system_prompt: Option<String>,

    /// Append to built-in system prompt
    #[arg(long = "append-system-prompt")]
    pub append_system_prompt: Option<String>,

    /// Extra directories file tools may access
    #[arg(long = "add-dir", num_args = 1..)]
    pub add_dir: Vec<PathBuf>,

    /// Load MCP servers from file or JSON string
    #[arg(long = "mcp-config", num_args = 1..)]
    pub mcp_config: Vec<String>,

    /// Only use MCP servers from --mcp-config
    #[arg(long = "strict-mcp-config")]
    pub strict_mcp_config: bool,

    /// Session-only settings layer (file path or JSON string)
    #[arg(long = "settings")]
    pub settings: Option<String>,

    /// Comma-separated list of setting layers to load: user,project,local
    #[arg(long = "setting-sources")]
    pub setting_sources: Option<String>,

    /// Define sub-agents inline (JSON or file path)
    #[arg(long = "agents")]
    pub agents: Option<String>,

    /// Run a named sub-agent definition as the main agent (B1)
    #[arg(long = "agent")]
    pub agent: Option<String>,

    /// Cap agent loop iterations
    #[arg(long = "max-turns")]
    pub max_turns: Option<usize>,

    /// Hard spend cap in USD for this session: model calls that would breach it
    /// are refused (`error_budget_exceeded`). Also `ZEN_MAX_BUDGET_USD` / config
    /// `maxBudgetUsd`. A no-op for free models.
    #[arg(long = "max-budget-usd")]
    pub max_budget_usd: Option<f64>,

    /// Warn once when spend crosses this fraction of the budget (default 0.8).
    /// Also `ZEN_BUDGET_WARN_AT` / config `budgetWarnAt`.
    #[arg(long = "budget-warn-at")]
    pub budget_warn_at: Option<f64>,

    /// After each successful turn, run this shell command; on non-zero exit, feed its output
    /// back to the agent and retry (up to --max-verify-retries times)
    #[arg(long = "until")]
    pub until: Option<String>,

    /// How many times to retry after a failing --until command (default 3)
    #[arg(long = "max-verify-retries", default_value_t = 3)]
    pub max_verify_retries: usize,

    /// Plan first: run in plan mode, show the plan, ask for approval, then execute
    #[arg(long = "plan")]
    pub plan: bool,

    /// Auto-approve the --plan plan without asking
    #[arg(short = 'y', long = "yes")]
    pub yes: bool,

    /// Skip ZEN.md, hooks, skills, custom commands, agents, MCP auto-discovery
    #[arg(long = "bare")]
    pub bare: bool,

    /// Terse mode: append a maximal-concision instruction to the system prompt
    /// (saves output tokens; `/terse` toggles it in the REPL)
    #[arg(long = "terse")]
    pub terse: bool,

    /// Hard cap on model output tokens per request (provider-level max_tokens)
    #[arg(long = "max-output-tokens")]
    pub max_output_tokens: Option<u32>,

    /// Token budget for the repository map block in the system prompt
    /// (0 disables it; estimated as chars/4)
    #[arg(long = "repomap-budget", default_value_t = crate::repomap::DEFAULT_MAP_TOKENS)]
    pub map_tokens: usize,

    /// Trust this folder for this run: load project hooks, MCP servers, allow rules, env and
    /// verify from `.zen/` and `.mcp.json` (same as `ZEN_TRUST_FOLDER=1`; never saved)
    #[arg(long = "trust-folder")]
    pub trust_folder: bool,

    /// Acknowledge account-safety warnings and proceed anyway (concurrency /
    /// parallel-session volume overrides only; never disables backoff or quota handling).
    /// The acknowledgement is logged with a timestamp.
    #[arg(long = "i-understand-the-risks")]
    pub i_understand_the_risks: bool,

    /// Tool bundle: which tools are sent with full schemas (`all`, `core`,
    /// `read`, `write`, `exec`). Tools outside the bundle are name-only stubs.
    /// Fixed for the session so prompt caches stay valid. Also `ZEN_TOOL_BUNDLE`
    /// or settings `toolBundle`.
    #[arg(long = "tool-bundle", value_parser = parse_tool_bundle)]
    pub tool_bundle: Option<zen_agent::ToolBundle>,

    #[command(subcommand)]
    pub subcommand: Option<Commands>,

    /// Prompt text. Words are joined with spaces. Flags may appear before or after it
    /// (no trailing_var_arg: that would swallow `--output-format json` placed after the prompt).
    pub prompt: Vec<String>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Commands {
    /// Manage MCP servers
    Mcp(McpArgs),
    /// Check loaded settings sources and model reachability
    Doctor,
    /// List and probe free models across configured providers
    Models(ModelsArgs),
    /// Show token usage and cost for a session
    Cost(CostArgs),
    /// Show the context debugger: token attribution for a session
    Context(ContextArgs),
    /// Show full documentation for a tool (parameters, behavior, examples)
    #[command(name = "tool-help")]
    ToolHelp {
        /// Tool name, e.g. Read
        name: String,
    },
    /// Export a session transcript to a portable format (JSON/Markdown/HTML)
    Export(ExportArgs),
    /// Import a session from a JSON export file
    Import(ImportArgs),
}

#[derive(Args, Debug, Clone)]
pub struct ModelsArgs {
    /// Re-probe all candidate free models now (sequential, respectful of rate limits)
    #[arg(long = "refresh")]
    pub refresh: bool,
    /// Print only the recommended model id (for scripting: zen --model $(zen models --best))
    #[arg(long = "best")]
    pub best: bool,
}

#[derive(Args, Debug, Clone)]
pub struct CostArgs {
    /// Session id (default: most recent session)
    #[arg(long = "session")]
    pub session: Option<String>,
    /// Output format: text, json, stream-json
    #[arg(long = "output-format", value_enum, default_value_t = OutputFormat::Text)]
    pub output_format: OutputFormat,
}

/// Arguments for `zen context`.
#[derive(Args, Debug, Clone)]
pub struct ContextArgs {
    /// Session id or transcript path (default: latest session for cwd)
    #[arg(long = "session")]
    pub session: Option<String>,
}


/// Arguments for `zen export`.
#[derive(Args, Debug, Clone)]
pub struct ExportArgs {
    /// Output format: json, markdown, or html
    #[arg(long, value_enum, default_value_t = crate::export::ExportFormat::Markdown)]
    pub format: crate::export::ExportFormat,
    /// Session id (default: most recent session for the current directory)
    #[arg(long)]
    pub session: Option<String>,
    /// Write to this file instead of stdout
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Redact potential secrets (API keys, tokens, private keys) from the export
    #[arg(long)]
    pub redact: bool,
}

/// Arguments for `zen import`.
#[derive(Args, Debug, Clone)]
pub struct ImportArgs {
    /// JSON export file produced by `zen export --format json`
    pub file: PathBuf,
}

#[derive(Args, Debug, Clone)]
pub struct McpArgs {
    #[command(subcommand)]
    pub command: McpSubcommand,
}

#[derive(Subcommand, Debug, Clone)]
pub enum McpSubcommand {
    /// List configured MCP servers
    List,
    /// Add an MCP server
    Add {
        /// Transport type (stdio or http)
        #[arg(long = "transport", default_value = "stdio")]
        transport: String,
        /// Configuration scope: local, project, user
        #[arg(short = 's', long = "scope", default_value = "local")]
        scope: String,
        /// Environment variable for a stdio server (repeatable), KEY=VALUE
        #[arg(short = 'e', long = "env", value_name = "KEY=VALUE")]
        env: Vec<String>,
        /// HTTP header for an http server (repeatable), KEY=VALUE
        #[arg(long = "header", value_name = "KEY=VALUE")]
        header: Vec<String>,
        /// Server name
        name: String,
        /// Command or URL and arguments
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Remove an MCP server
    Remove {
        /// Configuration scope: local, project, user
        #[arg(short = 's', long = "scope", default_value = "local")]
        scope: String,
        /// Server name
        name: String,
    },
    /// Print one server's configuration as JSON
    Get {
        /// Configuration scope: local, project, user (empty = search all)
        #[arg(short = 's', long = "scope", default_value = "")]
        scope: String,
        /// Server name
        name: String,
    },
    /// Add a server from a JSON object (same shape as the config file entry)
    AddJson {
        /// Configuration scope: local, project, user
        #[arg(short = 's', long = "scope", default_value = "local")]
        scope: String,
        /// Server name
        name: String,
        /// JSON object describing the server
        json: String,
    },
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn parse(args: &[&str]) -> Cli {
        let mut base = vec!["zen"];
        base.extend_from_slice(args);
        Cli::parse_from(base)
    }

    #[test]
    fn parse_export_subcommand() {
        let cli = parse(&["export", "--format", "json", "--session", "abc", "--redact"]);
        match cli.subcommand {
            Some(Commands::Export(e)) => {
                assert_eq!(e.format, crate::export::ExportFormat::Json);
                assert_eq!(e.session.as_deref(), Some("abc"));
                assert!(e.redact);
                assert!(e.output.is_none());
            }
            other => panic!("expected Export, got {:?}", other.map(|_| ())),
        }
        let cli = parse(&["export", "-o", "s.md"]);
        match cli.subcommand {
            Some(Commands::Export(e)) => {
                assert_eq!(e.format, crate::export::ExportFormat::Markdown);
                assert_eq!(e.output, Some(PathBuf::from("s.md")));
            }
            _ => panic!("expected Export"),
        }
        let cli = parse(&["import", "sess.json"]);
        match cli.subcommand {
            Some(Commands::Import(i)) => assert_eq!(i.file, PathBuf::from("sess.json")),
            _ => panic!("expected Import"),
        }
    }

    #[test]
    fn parse_print_and_prompt() {
        let cli = parse(&["-p", "hello", "world"]);
        assert!(cli.print);
        assert_eq!(cli.prompt, vec!["hello", "world"]);
    }

    #[test]
    fn parse_output_format() {
        let cli = parse(&["-p", "--output-format", "json", "hi"]);
        assert_eq!(cli.output_format, OutputFormat::Json);
    }

    #[test]
    fn parse_output_format_stream_json() {
        let cli = parse(&["-p", "--output-format", "stream-json", "hi"]);
        assert_eq!(cli.output_format, OutputFormat::StreamJson);
    }

    #[test]
    fn parse_permission_mode() {
        let cli = parse(&["--permission-mode", "plan"]);
        assert_eq!(cli.permission_mode, Some(PermissionMode::Plan));
    }

    #[test]
    fn c2_11_permission_mode_aliases() {
        assert_eq!(parse(&["--permission-mode", "manual"]).permission_mode, Some(PermissionMode::Default));
        assert_eq!(parse(&["--permission-mode", "auto"]).permission_mode, Some(PermissionMode::Default));
        assert_eq!(
            parse(&["--permission-mode", "dontAsk"]).permission_mode,
            Some(PermissionMode::DontAsk)
        );
        let err = Cli::try_parse_from(["zen", "--permission-mode", "yolo"]).unwrap_err();
        assert!(err.to_string().contains("invalid permission mode 'yolo'"), "{}", err);
    }

    #[test]
    fn parse_tools_empty() {
        let cli = parse(&["--tools", ""]);
        assert_eq!(cli.tools.as_deref(), Some(""));
    }

    #[test]
    fn parse_tools_default() {
        let cli = parse(&["--tools", "default"]);
        assert_eq!(cli.tools.as_deref(), Some("default"));
    }

    #[test]
    fn parse_tools_list() {
        let cli = parse(&["--tools", "Read,Bash"]);
        assert_eq!(cli.tools.as_deref(), Some("Read,Bash"));
        let cli = parse(&["--tools", "-Bash,-Write", "-p", "hi"]);
        assert_eq!(cli.tools.as_deref(), Some("-Bash,-Write"));
        assert!(cli.print);
        assert_eq!(cli.prompt, vec!["hi".to_string()]);
    }

    #[test]
    fn parse_mcp_subcommand_list() {
        let cli = parse(&["mcp", "list"]);
        assert!(matches!(cli.subcommand, Some(Commands::Mcp(_))));
    }

    #[test]
    fn parse_mcp_subcommand_remove() {
        let cli = parse(&["mcp", "remove", "-s", "user", "foo"]);
        match &cli.subcommand {
            Some(Commands::Mcp(m)) => {
                assert!(matches!(m.command, McpSubcommand::Remove{..}));
            }
            _ => panic!("expected Mcp Remove"),
        }
    }

    #[test]
    fn parse_mcp_subcommand_add_http() {
        let cli = parse(&["mcp", "add", "--transport", "http", "-s", "project", "myserver", "http://localhost:3000"]);
        match &cli.subcommand {
            Some(Commands::Mcp(m)) => {
                assert!(matches!(&m.command, McpSubcommand::Add{transport,..} if transport == "http"));
            }
            _ => panic!("expected Mcp Add"),
        }
    }

    #[test]
    fn parse_doctor_subcommand() {
        let cli = parse(&["doctor"]);
        assert!(matches!(cli.subcommand, Some(Commands::Doctor)));
    }

    #[test]
    fn parse_models_subcommand() {
        let cli = parse(&["models"]);
        match &cli.subcommand {
            Some(Commands::Models(m)) => {
                assert!(!m.refresh);
                assert!(!m.best);
            }
            _ => panic!("expected Models"),
        }
        let cli = parse(&["models", "--refresh", "--best"]);
        match &cli.subcommand {
            Some(Commands::Models(m)) => {
                assert!(m.refresh);
                assert!(m.best);
            }
            _ => panic!("expected Models with flags"),
        }
    }

    #[test]
    fn parse_flag_bare() {
        let cli = parse(&["--bare"]);
        assert!(cli.bare);
    }

    #[test]
    fn parse_flag_terse() {
        let cli = parse(&["--terse"]);
        assert!(cli.terse);
        let cli = parse(&[]);
        assert!(!cli.terse);
    }

    #[test]
    fn parse_max_output_tokens() {
        let cli = parse(&["--max-output-tokens", "512"]);
        assert_eq!(cli.max_output_tokens, Some(512));
        let cli = parse(&[]);
        assert_eq!(cli.max_output_tokens, None);
    }

    #[test]
    fn parse_no_args() {
        let cli = parse(&[]);
        assert!(!cli.print);
        assert!(cli.prompt.is_empty());
        assert!(cli.subcommand.is_none());
    }

    #[test]
    fn parse_setting_sources() {
        let cli = parse(&["--setting-sources", "user,project"]);
        assert_eq!(cli.setting_sources.as_deref(), Some("user,project"));
    }

    #[test]
    fn parse_resume_with_value() {
        let cli = parse(&["--resume", "abc"]);
        assert_eq!(cli.resume.as_deref(), Some("abc"));
    }

    #[test]
    fn parse_resume_no_value() {
        let cli = parse(&["--resume"]);
        assert_eq!(cli.resume.as_deref(), Some(""));
    }

    #[test]
    fn parse_fork_session() {
        let cli = parse(&["--fork-session"]);
        assert!(cli.fork_session);
    }

    #[test]
    fn parse_prune_flags() {
        // Default: pruning on (prune=false, no_prune=false → enabled).
        let cli = parse(&[]);
        assert!(!cli.prune);
        assert!(!cli.no_prune);
        assert_eq!(cli.prune_after, None);
        assert_eq!(cli.max_result_tokens, None);

        let cli = parse(&["--prune-after", "3", "--max-result-tokens", "4000"]);
        assert_eq!(cli.prune_after, Some(3));
        assert_eq!(cli.max_result_tokens, Some(4000));

        // --no-prune wins over --prune when both are given (last wins).
        let cli = parse(&["--prune", "--no-prune"]);
        assert!(cli.no_prune);
        assert!(!cli.prune);
        let cli = parse(&["--no-prune", "--prune"]);
        assert!(cli.prune);
        assert!(!cli.no_prune);
    }

    #[test]
    fn parse_session_id() {
        let id = "123e4567-e89b-12d3-a456-426614174000";
        let cli = parse(&["--session-id", id]);
        assert_eq!(cli.session_id.as_deref(), Some(id));
    }

    #[test]
    fn c9_2_session_id_must_be_uuid() {
        for bad in ["sess-1", "", "123e4567-e89b-12d3-a456", "not a uuid"] {
            let r = Cli::try_parse_from(["zen", "--session-id", bad]);
            assert!(r.is_err(), "{:?} should be rejected", bad);
        }
        // Normalised to the canonical lowercase hyphenated form.
        let cli = parse(&["--session-id", "123E4567E89B12D3A456426614174000"]);
        assert_eq!(cli.session_id.as_deref(), Some("123e4567-e89b-12d3-a456-426614174000"));
    }

    #[test]
    fn parse_max_turns() {
        let cli = parse(&["--max-turns", "10"]);
        assert_eq!(cli.max_turns, Some(10));
    }

    #[test]
    fn parse_max_budget_usd() {
        let cli = parse(&["--max-budget-usd", "2.50"]);
        assert_eq!(cli.max_budget_usd, Some(2.50));
        assert_eq!(cli.budget_warn_at, None);
        let cli = parse(&["--max-budget-usd", "2.50", "--budget-warn-at", "0.5"]);
        assert_eq!(cli.budget_warn_at, Some(0.5));
        // Defaults: no cap unless configured.
        let cli = parse(&[] as &[&str]);
        assert_eq!(cli.max_budget_usd, None);
    }

    #[test]
    fn parse_until_and_max_verify_retries() {
        let cli = parse(&["-p", "--until", "cargo test -q", "go"]);
        assert_eq!(cli.until.as_deref(), Some("cargo test -q"));
        // Default is 3 retries.
        assert_eq!(cli.max_verify_retries, 3);
        let cli = parse(&["-p", "--until", "make check", "--max-verify-retries", "0", "go"]);
        assert_eq!(cli.max_verify_retries, 0);
    }

    #[test]
    fn parse_plan_and_yes() {
        let cli = parse(&["-p", "--plan", "go"]);
        assert!(cli.plan);
        assert!(!cli.yes);
        let cli = parse(&["-p", "--plan", "--yes", "go"]);
        assert!(cli.plan && cli.yes);
        let cli = parse(&["-p", "--plan", "-y", "go"]);
        assert!(cli.yes);
    }

    #[test]
    fn flags_after_prompt_are_parsed() {
        let cli = parse(&["-p", "list files", "--output-format", "json", "--model", "m:free"]);
        assert!(cli.print);
        assert_eq!(cli.prompt, vec!["list files".to_string()]);
        assert!(matches!(cli.output_format, OutputFormat::Json));
        assert_eq!(cli.model.as_deref(), Some("m:free"));
    }

    #[test]
    fn parse_model_flag() {
        let cli = parse(&["--model", "gpt-4"]);
        assert_eq!(cli.model.as_deref(), Some("gpt-4"));
    }

    #[test]
    fn parse_base_url_flag() {
        let cli = parse(&["--base-url", "https://x.com/v1"]);
        assert_eq!(cli.base_url.as_deref(), Some("https://x.com/v1"));
    }

    #[test]
    fn parse_add_dir() {
        let cli = parse(&["--add-dir", "/a", "/b"]);
        assert_eq!(cli.add_dir.len(), 2);
    }

    #[test]
    fn parse_allowed_disallowed() {
        let cli = parse(&["--allowedTools", "Read", "Bash(git status)", "--disallowedTools", "Write"]);
        assert_eq!(cli.allowed_tools, vec!["Read", "Bash(git status)"]);
        assert_eq!(cli.disallowed_tools, vec!["Write"]);
    }

    #[test]
    fn parse_bare_skips_settings_flag() {
        let cli = parse(&["--bare", "-p", "hi"]);
        assert!(cli.bare);
        assert!(cli.print);
    }

    #[test]
    fn parse_agent_flag() {
        let cli = parse(&["-p", "--agent", "researcher", "go"]);
        assert_eq!(cli.agent.as_deref(), Some("researcher"));
    }

    #[test]
    fn parse_mcp_get_with_scope() {
        let cli = parse(&["mcp", "get", "-s", "user", "srv"]);
        match &cli.subcommand {
            Some(Commands::Mcp(m)) => match &m.command {
                McpSubcommand::Get { scope, name } => {
                    assert_eq!(scope, "user");
                    assert_eq!(name, "srv");
                }
                other => panic!("expected Get, got {:?}", other),
            },
            other => panic!("expected Mcp, got {:?}", other),
        }
    }

    #[test]
    fn parse_mcp_add_json() {
        let cli = parse(&["mcp", "add-json", "srv", r#"{"command":"x"}"#]);
        match &cli.subcommand {
            Some(Commands::Mcp(m)) => match &m.command {
                McpSubcommand::AddJson { name, json, .. } => {
                    assert_eq!(name, "srv");
                    assert!(json.contains("command"));
                }
                other => panic!("expected AddJson, got {:?}", other),
            },
            other => panic!("expected Mcp, got {:?}", other),
        }
    }

    #[test]
    fn parse_mcp_add_env_and_header() {
        let cli = parse(&[
            "mcp",
            "add",
            "--transport",
            "http",
            "-e",
            "K=V",
            "--header",
            "Authorization=Bearer x",
            "srv",
            "http://localhost:1",
        ]);
        match &cli.subcommand {
            Some(Commands::Mcp(m)) => match &m.command {
                McpSubcommand::Add {
                    env, header, name, ..
                } => {
                    assert_eq!(env, &vec!["K=V".to_string()]);
                    assert_eq!(header, &vec!["Authorization=Bearer x".to_string()]);
                    assert_eq!(name, "srv");
                }
                other => panic!("expected Add, got {:?}", other),
            },
            other => panic!("expected Mcp, got {:?}", other),
        }
    }

    #[test]
    fn parse_provider_flag() {
        let cli = parse(&["--provider", "ollama"]);
        assert_eq!(cli.provider.as_deref(), Some("ollama"));
        let cli = parse(&[]);
        assert_eq!(cli.provider, None);
    }
}
