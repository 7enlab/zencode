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

            "default" | "manual" | "auto" => PermissionMode::Default,
            "acceptEdits" => PermissionMode::AcceptEdits,
            "plan" => PermissionMode::Plan,
            "bypassPermissions" => PermissionMode::BypassPermissions,
            "dontAsk" => PermissionMode::DontAsk,
            _ => return None,
        })
    }
}

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

fn parse_autocompact(s: &str) -> Result<String, String> {
    match s {
        "auto" | "off" => Ok(s.to_string()),
        n if n.parse::<u64>().map(|v| v > 0).unwrap_or(false) => Ok(n.to_string()),
        _ => Err(format!("invalid --autocompact '{}': use auto, off or a token count", s)),
    }
}

fn parse_session_uuid(s: &str) -> Result<String, String> {
    uuid::Uuid::parse_str(s.trim())
        .map(|u| u.hyphenated().to_string())
        .map_err(|_| format!("invalid session id '{}': must be a valid UUID", s))
}

#[derive(Parser, Debug, Clone)]
#[command(name = "zen", version, about = "ZenCode CLI coding agent")]
pub struct Cli {

    #[arg(short = 'p', long = "print")]
    pub print: bool,

    #[arg(long = "acp")]
    pub acp: bool,

    #[arg(long = "output-format", value_enum, default_value_t = OutputFormat::Text)]
    pub output_format: OutputFormat,

    #[arg(long = "input-format", value_enum, default_value_t = InputFormat::Text)]
    pub input_format: InputFormat,

    #[arg(long = "include-partial-messages")]
    pub include_partial_messages: bool,

    #[arg(long = "verbose")]
    pub verbose: bool,

    #[arg(short = 'c', long = "continue")]
    pub continue_session: bool,

    #[arg(short = 'r', long = "resume", num_args = 0..=1, default_missing_value = "")]
    pub resume: Option<String>,

    #[arg(long = "autocompact", value_parser = parse_autocompact)]
    pub autocompact: Option<String>,

    #[arg(long = "prune", overrides_with = "no_prune")]
    pub prune: bool,

    #[arg(long = "no-prune", overrides_with = "prune")]
    pub no_prune: bool,

    #[arg(long = "prune-after")]
    pub prune_after: Option<usize>,

    #[arg(long = "max-result-tokens")]
    pub max_result_tokens: Option<u64>,

    #[arg(long = "session-id", value_parser = parse_session_uuid)]
    pub session_id: Option<String>,

    #[arg(long = "fork-session")]
    pub fork_session: bool,

    #[arg(long = "no-session-persistence")]
    pub no_session_persistence: bool,

    #[arg(long)]
    pub ephemeral: bool,

    #[arg(long = "model")]
    pub model: Option<String>,

    #[arg(long = "btw-model")]
    pub btw_model: Option<String>,

    #[arg(long = "base-url")]
    pub base_url: Option<String>,

    #[arg(long = "provider")]
    pub provider: Option<String>,

    #[arg(long = "permission-mode", value_parser = parse_permission_mode_flag)]
    pub permission_mode: Option<PermissionMode>,

    #[arg(long = "allowedTools", visible_alias = "allowed-tools", num_args = 1..)]
    pub allowed_tools: Vec<String>,

    #[arg(long = "disallowedTools", visible_alias = "disallowed-tools", num_args = 1..)]
    pub disallowed_tools: Vec<String>,

    #[arg(long = "tools", allow_hyphen_values = true)]
    pub tools: Option<String>,

    #[arg(long = "system-prompt")]
    pub system_prompt: Option<String>,

    #[arg(long = "append-system-prompt")]
    pub append_system_prompt: Option<String>,

    #[arg(long = "add-dir", num_args = 1..)]
    pub add_dir: Vec<PathBuf>,

    #[arg(long = "mcp-config", num_args = 1..)]
    pub mcp_config: Vec<String>,

    #[arg(long = "strict-mcp-config")]
    pub strict_mcp_config: bool,

    #[arg(long = "settings")]
    pub settings: Option<String>,

    #[arg(long = "setting-sources")]
    pub setting_sources: Option<String>,

    #[arg(long = "agents")]
    pub agents: Option<String>,

    #[arg(long = "agent")]
    pub agent: Option<String>,

    #[arg(long = "max-turns")]
    pub max_turns: Option<usize>,

    #[arg(long = "max-budget-usd")]
    pub max_budget_usd: Option<f64>,

    #[arg(long = "budget-warn-at")]
    pub budget_warn_at: Option<f64>,

    #[arg(long = "until")]
    pub until: Option<String>,

    #[arg(long = "max-verify-retries", default_value_t = 3)]
    pub max_verify_retries: usize,

    #[arg(long = "plan")]
    pub plan: bool,

    #[arg(short = 'y', long = "yes")]
    pub yes: bool,

    #[arg(long = "bare")]
    pub bare: bool,

    #[arg(long = "terse")]
    pub terse: bool,

    #[arg(long = "max-output-tokens")]
    pub max_output_tokens: Option<u32>,

    #[arg(long = "repomap-budget", default_value_t = crate::repomap::DEFAULT_MAP_TOKENS)]
    pub map_tokens: usize,

    #[arg(long = "trust-folder")]
    pub trust_folder: bool,

    #[arg(long = "i-understand-the-risks")]
    pub i_understand_the_risks: bool,

    #[arg(long = "tool-bundle", value_parser = parse_tool_bundle)]
    pub tool_bundle: Option<zen_agent::ToolBundle>,

    #[arg(long = "style", value_name = "STYLE")]
    pub style: Option<String>,

    #[arg(long = "json-schema", value_name = "FILE")]
    pub json_schema: Option<std::path::PathBuf>,

    #[command(subcommand)]
    pub subcommand: Option<Commands>,

    pub prompt: Vec<String>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Commands {

    Mcp(McpArgs),

    Skill(SkillArgs),

    Doctor,

    Models(ModelsArgs),

    Cost(CostArgs),

    Context(ContextArgs),

    #[command(name = "tool-help")]
    ToolHelp {

        name: String,
    },

    Export(ExportArgs),

    Import(ImportArgs),

    Sessions(SessionsArgs),

    Eval(EvalArgs),

    Review(ReviewArgs),

    #[command(name = "pr-comments")]
    PrComments {

        pr: Option<String>,

        #[arg(long)]
        json: bool,
    },

    #[command(name = "ci-fix")]
    CiFix {

        #[arg(long)]
        json: bool,
    },

    Update(UpdateArgs),
}

#[derive(Args, Debug, Clone)]
pub struct UpdateArgs {

    #[arg(long)]
    pub check: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ModelsArgs {

    #[arg(long = "refresh")]
    pub refresh: bool,

    #[arg(long = "best")]
    pub best: bool,
}

#[derive(Args, Debug, Clone)]
pub struct CostArgs {

    #[arg(long = "session")]
    pub session: Option<String>,

    #[arg(long = "output-format", value_enum, default_value_t = OutputFormat::Text)]
    pub output_format: OutputFormat,
}

#[derive(Args, Debug, Clone)]
pub struct ContextArgs {

    #[arg(long = "session")]
    pub session: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct ExportArgs {

    #[arg(long, value_enum, default_value_t = crate::export::ExportFormat::Markdown)]
    pub format: crate::export::ExportFormat,

    #[arg(long)]
    pub session: Option<String>,

    #[arg(short, long)]
    pub output: Option<PathBuf>,

    #[arg(long)]
    pub redact: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ImportArgs {

    pub file: PathBuf,
}

#[derive(Args, Debug, Clone)]
pub struct SessionsArgs {
    #[command(subcommand)]
    pub command: SessionSubcommand,
}

#[derive(Subcommand, Debug, Clone)]
pub enum SessionSubcommand {

    List {

        #[arg(long)]
        all: bool,
    },

    Archive {

        id: String,
    },

    Delete {

        id: String,

        #[arg(long)]
        yes: bool,
    },

    Restore {

        id: String,
    },
}

#[derive(Args, Debug, Clone)]
pub struct EvalArgs {

    #[arg(long)]
    pub model: Option<String>,

    #[arg(long)]
    pub task: Option<String>,

    #[arg(long)]
    pub json: bool,

    #[arg(long, default_value_t = 25)]
    pub max_turns: usize,
}

#[derive(Args, Debug, Clone)]
pub struct SkillArgs {
    #[command(subcommand)]
    pub command: SkillSubcommand,
}

#[derive(Subcommand, Debug, Clone)]
pub enum SkillSubcommand {

    List,

    Install {

        source: String,

        #[arg(long)]
        yes: bool,

        #[arg(long)]
        registry: Option<String>,
    },

    Search {

        query: String,

        #[arg(long)]
        registry: Option<String>,

        #[arg(long)]
        curated: bool,
    },

    Info {

        name: String,

        #[arg(long)]
        registry: Option<String>,
    },

    Remove {

        name: String,
    },

    Test {

        name: String,
    },

    Create {

        name: String,
    },
}

#[derive(Args, Debug, Clone)]
pub struct McpArgs {
    #[command(subcommand)]
    pub command: McpSubcommand,
}

#[derive(Subcommand, Debug, Clone)]
pub enum McpSubcommand {

    List,

    Add {

        #[arg(long = "transport", default_value = "stdio")]
        transport: String,

        #[arg(short = 's', long = "scope", default_value = "local")]
        scope: String,

        #[arg(short = 'e', long = "env", value_name = "KEY=VALUE")]
        env: Vec<String>,

        #[arg(long = "header", value_name = "KEY=VALUE")]
        header: Vec<String>,

        name: String,

        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },

    Remove {

        #[arg(short = 's', long = "scope", default_value = "local")]
        scope: String,

        name: String,
    },

    Get {

        #[arg(short = 's', long = "scope", default_value = "")]
        scope: String,

        name: String,
    },

    AddJson {

        #[arg(short = 's', long = "scope", default_value = "local")]
        scope: String,

        name: String,

        json: String,
    },
}

#[derive(Args, Debug, Clone)]
pub struct ReviewArgs {

    #[arg(long)]
    pub uncommitted: bool,

    #[arg(long, value_name = "REF")]
    pub base: Option<String>,

    #[arg(long, value_name = "SHA")]
    pub commit: Option<String>,

    #[arg(long, default_value = "high")]
    pub fail_on: String,

    #[arg(long = "output-format", value_enum, default_value_t = OutputFormat::Text)]
    pub output_format: OutputFormat,

    #[arg(long, default_value_t = 100_000)]
    pub max_diff_bytes: usize,
}

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
    fn parse_update_subcommand() {
        let cli = parse(&["update"]);
        match &cli.subcommand {
            Some(Commands::Update(args)) => {
                assert!(!args.check);
            }
            _ => panic!("expected Update"),
        }
        let cli = parse(&["update", "--check"]);
        match &cli.subcommand {
            Some(Commands::Update(args)) => {
                assert!(args.check);
            }
            _ => panic!("expected Update with check"),
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
    fn parse_sessions_subcommands() {
        let cli = parse(&["sessions", "list"]);
        assert!(matches!(
            cli.subcommand,
            Some(Commands::Sessions(s)) if matches!(s.command, SessionSubcommand::List { all: false })
        ));
        let cli = parse(&["sessions", "list", "--all"]);
        assert!(matches!(
            cli.subcommand,
            Some(Commands::Sessions(s)) if matches!(s.command, SessionSubcommand::List { all: true })
        ));
        let cli = parse(&["sessions", "archive", "abc123"]);
        match cli.subcommand {
            Some(Commands::Sessions(s)) => match s.command {
                SessionSubcommand::Archive { id } => assert_eq!(id, "abc123"),
                other => panic!("expected Archive, got {:?}", other),
            },
            _ => panic!("expected Sessions"),
        }
        let cli = parse(&["sessions", "delete", "abc123", "--yes"]);
        match cli.subcommand {
            Some(Commands::Sessions(s)) => match s.command {
                SessionSubcommand::Delete { id, yes } => {
                    assert_eq!(id, "abc123");
                    assert!(yes);
                }
                other => panic!("expected Delete, got {:?}", other),
            },
            _ => panic!("expected Sessions"),
        }
        let cli = parse(&["sessions", "restore", "abc123"]);
        assert!(matches!(
            cli.subcommand,
            Some(Commands::Sessions(s)) if matches!(s.command, SessionSubcommand::Restore { .. })
        ));
    }

    #[test]
    fn parse_skill_subcommands() {
        let cli = parse(&["skill", "list"]);
        assert!(matches!(
            cli.subcommand,
            Some(Commands::Skill(s)) if matches!(s.command, SkillSubcommand::List)
        ));
        let cli = parse(&["skill", "install", "builtin:tdd", "--yes"]);
        match cli.subcommand {
            Some(Commands::Skill(s)) => match s.command {
                SkillSubcommand::Install { source, yes, registry } => {
                    assert_eq!(source, "builtin:tdd");
                    assert!(yes);
                    assert!(registry.is_none());
                }
                other => panic!("expected Install, got {:?}", other),
            },
            _ => panic!("expected Skill"),
        }
        let cli = parse(&["skill", "test", "tdd"]);
        assert!(matches!(
            cli.subcommand,
            Some(Commands::Skill(s)) if matches!(s.command, SkillSubcommand::Test{..})
        ));
        let cli = parse(&["skill", "remove", "tdd"]);
        assert!(matches!(
            cli.subcommand,
            Some(Commands::Skill(s)) if matches!(s.command, SkillSubcommand::Remove{..})
        ));
        let cli = parse(&["skill", "create", "my-skill"]);
        assert!(matches!(
            cli.subcommand,
            Some(Commands::Skill(s)) if matches!(s.command, SkillSubcommand::Create{..})
        ));
        let cli = parse(&["skill", "search", "tdd"]);
        assert!(matches!(
            cli.subcommand,
            Some(Commands::Skill(s)) if matches!(s.command, SkillSubcommand::Search{..})
        ));
        let cli = parse(&["skill", "search", "tdd", "--curated"]);
        assert!(matches!(
            cli.subcommand,
            Some(Commands::Skill(s)) if matches!(s.command, SkillSubcommand::Search { curated: true, .. })
        ));
        let cli = parse(&["skill", "info", "tdd"]);
        assert!(matches!(
            cli.subcommand,
            Some(Commands::Skill(s)) if matches!(s.command, SkillSubcommand::Info{..})
        ));
        let cli = parse(&["skill", "install", "registry:tdd", "--yes"]);
        match cli.subcommand {
            Some(Commands::Skill(s)) => match s.command {
                SkillSubcommand::Install { source, yes, .. } => {
                    assert_eq!(source, "registry:tdd");
                    assert!(yes);
                }
                other => panic!("expected Install, got {other:?}"),
            },
            _ => panic!("expected Skill"),
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

        let cli = parse(&[]);
        assert!(!cli.prune);
        assert!(!cli.no_prune);
        assert_eq!(cli.prune_after, None);
        assert_eq!(cli.max_result_tokens, None);

        let cli = parse(&["--prune-after", "3", "--max-result-tokens", "4000"]);
        assert_eq!(cli.prune_after, Some(3));
        assert_eq!(cli.max_result_tokens, Some(4000));

        let cli = parse(&["--prune", "--no-prune"]);
        assert!(cli.no_prune);
        assert!(!cli.prune);
        let cli = parse(&["--no-prune", "--prune"]);
        assert!(cli.prune);
        assert!(!cli.no_prune);
    }

    #[test]
    fn parse_ephemeral() {
        let cli = parse(&["--ephemeral"]);
        assert!(cli.ephemeral);

        let cli = parse(&[]);
        assert!(!cli.ephemeral);
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

        let cli = parse(&[] as &[&str]);
        assert_eq!(cli.max_budget_usd, None);
    }

    #[test]
    fn parse_until_and_max_verify_retries() {
        let cli = parse(&["-p", "--until", "cargo test -q", "go"]);
        assert_eq!(cli.until.as_deref(), Some("cargo test -q"));

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

    #[test]
    fn parse_eval_subcommand() {
        let cli = parse(&["eval", "--model", "x:free", "--task", "create", "--json"]);
        match &cli.subcommand {
            Some(Commands::Eval(a)) => {
                assert_eq!(a.model.as_deref(), Some("x:free"));
                assert_eq!(a.task.as_deref(), Some("create"));
                assert!(a.json);
                assert_eq!(a.max_turns, 25);
            }
            other => panic!("expected Eval, got {:?}", other),
        }
    }

    #[test]
    fn parse_eval_max_turns() {
        let cli = parse(&["eval", "--max-turns", "10"]);
        match &cli.subcommand {
            Some(Commands::Eval(a)) => assert_eq!(a.max_turns, 10),
            other => panic!("expected Eval, got {:?}", other),
        }
    }
}
