use crate::cli::{Cli, PermissionMode};
use zen_core::{PermissionSettings, Settings};
use anyhow::Result;

pub const DEFAULT_BASE_URL: &str = "https://api.experientiallabs.ai/api/v1";
pub const DEFAULT_MODEL: &str = "google/gemini-4-argon:free";
pub const DEFAULT_MAX_TURNS: usize = 50;

/// Fully resolved runtime config that `main.rs` consumes.
#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub model: String,
    pub base_url: String,
    pub api_key: Option<String>,
    /// Name of the env var the API key was resolved from (`apiKeyEnv` setting);
    /// child processes must not inherit it (MEDIUM-1).
    pub api_key_env: Option<String>,
    /// WebFetch may fetch loopback/link-local/private IPs when true (LOW-1 SSRF guard).
    pub allow_private_network: bool,
    /// True when routed through the local Ollama provider (local inference, $0
    /// marginal cost). Cost ledgers and model tables should treat these runs
    /// as free.
    pub ollama: bool,
    pub fallback_models: Vec<String>,
    pub permissions: PermissionSettings,
    pub permission_mode: PermissionMode,
    pub hooks: serde_json::Value,
    pub env: std::collections::HashMap<String, String>,
    pub auto_compact: Option<serde_json::Value>,
    /// Raw `verify` object: `{ "enabled": bool, "command": "...", "timeoutSecs": n }` (r7).
    pub verify: Option<serde_json::Value>,
    /// Settings `retry.maxWaitSecs`: total backoff budget per model request (default 300 s).
    pub retry_max_wait_secs: u64,
    /// Settings `toolTimeouts`: ms per tool name (`"default"` for the rest, 0 = none) (p0b).
    pub tool_timeouts: std::collections::HashMap<String, u64>,
    /// Settings `maxContinuations`: cut-off replies continued in a row (default 3, 0 = off).
    pub max_continuations: usize,
    pub max_turns: usize,
    /// Config `maxBudgetUsd`: session spend cap in USD (flag/env take precedence).
    pub max_budget_usd: Option<f64>,
    /// Config `budgetWarnAt`: warn-once fraction of the cap (default 0.8).
    pub budget_warn_at: Option<f64>,
    pub context_window: Option<u64>,
    pub loaded_sources: Vec<String>,
    pub warnings: Vec<String>,
    pub setting_sources: Vec<String>,
    pub bypass_forbidden: bool,
    /// Folder trust: project hooks, MCP servers, allow rules, env and verify load only when true
    /// (`--trust-folder`, `ZEN_TRUST_FOLDER=1`, or the folder is in `~/.zen/trusted-folders.json`).
    pub folder_trusted: bool,
    /// What the untrusted run skipped, e.g. `.zen/settings.json: hooks` or `.mcp.json`.
    pub trust_skipped: Vec<String>,
    /// RateGate settings (g3): per-provider daily quota for `:free` tiers.
    pub rate_gate: zen_agent::RateGateSettings,
}

impl ResolvedConfig {
    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    /// An OpenAI-compatible client for this config, with the `retry.maxWaitSecs` budget applied.
    pub fn llm_client(&self, sink: std::sync::Arc<dyn zen_core::EventSink>) -> zen_agent::OpenAiClient {
        let mut c = zen_agent::OpenAiClient::new(self.base_url.clone(), self.api_key.clone(), sink);
        c.max_total_wait_ms = self.retry_max_wait_secs.saturating_mul(1000);
        c
    }

    /// Settings for a [`zen_agent::RateGate`] wrapping this config's client.
    /// `home` is None → the gate counts in memory only (no persistence).
    pub fn rate_gate_for(&self, home: Option<&std::path::Path>) -> (zen_agent::RateGateSettings, Option<std::path::PathBuf>) {
        let store_path = home.map(zen_agent::RateGate::default_store_path);
        (self.rate_gate.clone(), store_path)
    }

    /// The LLM backend for agent runs: an `OpenAiClient` wrapped in a RateGate
    /// (per-provider daily quota, g3). When the quota for the configured provider is
    /// exhausted the gate fails fast, and the agent loop's model-fallback logic moves
    /// on to the next configured model automatically.
    pub fn gated_llm_client(
        &self,
        sink: std::sync::Arc<dyn zen_core::EventSink>,
        home: Option<&std::path::Path>,
    ) -> std::sync::Arc<dyn zen_core::LlmBackend> {
        let client = self.llm_client(sink);
        let (settings, store_path) =
            self.rate_gate_for(home);
        std::sync::Arc::new(zen_agent::RateGate::new(
            std::sync::Arc::new(client),
            self.base_url.clone(),
            settings,
            store_path,
        ))
    }
}

/// Settings `toolTimeouts` (p0b): `{"Bash": 180000, "WebFetch": 30000, "default": 300000}`, values in
/// milliseconds, 0 = no timeout. Entries that are not non-negative integers are ignored (defaults
/// apply: Bash 120 s, other tools 300 s, Task/JobOutput none).
pub fn tool_timeouts(v: Option<&serde_json::Value>) -> std::collections::HashMap<String, u64> {
    v.and_then(|v| v.as_object())
        .map(|o| o.iter().filter_map(|(k, v)| v.as_u64().map(|n| (k.clone(), n))).collect())
        .unwrap_or_default()
}

/// Settings `maxContinuations` (continue on truncation): a non-negative integer, anything else →
/// the default ([`zen_agent::DEFAULT_MAX_CONTINUATIONS`]).
pub fn max_continuations(v: Option<&serde_json::Value>) -> usize {
    v.and_then(|v| v.as_u64()).map_or(zen_agent::DEFAULT_MAX_CONTINUATIONS, |n| n as usize)
}

/// Default for settings `retry.maxWaitSecs`.
pub const DEFAULT_RETRY_MAX_WAIT_SECS: u64 = 300;

/// `{"retry": {"maxWaitSecs": n}}` → n (a non-negative number; anything else → the default).
pub fn retry_max_wait_secs(retry: Option<&serde_json::Value>) -> u64 {
    retry
        .and_then(|r| r.get("maxWaitSecs"))
        .and_then(|v| v.as_u64().or_else(|| v.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64)))
        .unwrap_or(DEFAULT_RETRY_MAX_WAIT_SECS)
}

/// Resolve the file-level `rateGate` settings into runtime [`zen_agent::RateGateSettings`].
/// `{"rateGate": {"enabled": bool, "dailyLimit": n, "limits": {"host": n}}}`; absent values
/// fall back to the RateGate defaults (enabled, 50/day). A `dailyLimit` of 0 disables
/// counting for every provider (equivalent to `enabled: false`).
pub fn rate_gate_settings(cfg: &zen_core::RateGateFileConfig) -> zen_agent::RateGateSettings {
    zen_agent::RateGateSettings {
        enabled: cfg.enabled.unwrap_or(true),
        daily_limit: cfg.daily_limit.unwrap_or(zen_agent::DEFAULT_DAILY_LIMIT),
        limits: cfg.limits.clone(),
    }
}

/// Deep merge `over` onto `base`. Objects merge recursively; arrays concatenate+dedupe;
/// scalars: `over` wins when Some.
pub fn merge_settings(base: &Settings, over: &Settings) -> Settings {
    let mut out = base.clone();
    if over.model.is_some() {
        out.model = over.model.clone();
    }
    if over.base_url.is_some() {
        out.base_url = over.base_url.clone();
    }
    if over.api_key_env.is_some() {
        out.api_key_env = over.api_key_env.clone();
    }
    dedup_append(&mut out.fallback_models, &over.fallback_models);

    dedup_append(&mut out.permissions.allow, &over.permissions.allow);
    dedup_append(&mut out.permissions.ask, &over.permissions.ask);
    dedup_append(&mut out.permissions.deny, &over.permissions.deny);
    if over.permissions.default_mode.is_some() {
        out.permissions.default_mode = over.permissions.default_mode.clone();
    }
    dedup_append(
        &mut out.permissions.additional_directories,
        &over.permissions.additional_directories,
    );

    for (k, v) in &over.env {
        out.env.insert(k.clone(), v.clone());
    }
    if let Some(obj) = over.hooks.as_object() {
        if !out.hooks.is_object() {
            out.hooks = serde_json::Value::Object(Default::default());
        }
        if let Some(dst) = out.hooks.as_object_mut() {
            for (k, v) in obj {
                dst.insert(k.clone(), v.clone());
            }
        }
    }
    if over.auto_compact.is_some() {
        out.auto_compact = over.auto_compact.clone();
    }
    if over.max_turns.is_some() {
        out.max_turns = over.max_turns;
    }
    if over.context_window.is_some() {
        out.context_window = over.context_window;
    }
    // rateGate: `over` wins per-field when set; limits merge by provider host.
    if over.rate_gate.enabled.is_some() {
        out.rate_gate.enabled = over.rate_gate.enabled;
    }
    if over.rate_gate.daily_limit.is_some() {
        out.rate_gate.daily_limit = over.rate_gate.daily_limit;
    }
    for (k, v) in &over.rate_gate.limits {
        out.rate_gate.limits.insert(k.clone(), *v);
    }
    out
}

fn dedup_append<T: PartialEq + Clone>(target: &mut Vec<T>, source: &[T]) {
    for item in source {
        if !target.contains(item) {
            target.push(item.clone());
        }
    }
}



/// Resolve model from CLI > env ZEN_MODEL > settings > default.
fn resolve_model(cli: &Cli, settings_model: Option<&str>) -> String {
    if let Some(m) = &cli.model {
        return m.clone();
    }
    if let Ok(v) = std::env::var("ZEN_MODEL") {
        if !v.is_empty() {
            return v;
        }
    }
    settings_model
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

/// Resolve base_url from CLI > env ZEN_BASE_URL > settings > default.
fn resolve_base_url(cli: &Cli, settings_url: Option<&str>) -> String {
    if let Some(u) = &cli.base_url {
        return u.clone();
    }
    if let Ok(v) = std::env::var("ZEN_BASE_URL") {
        if !v.is_empty() {
            return v;
        }
    }
    settings_url
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
}

/// Resolve api key: ZEN_API_KEY > OPENAI_API_KEY > env named by settings.apiKeyEnv.
fn resolve_api_key(api_key_env: Option<&str>) -> Option<String> {
    if let Ok(v) = std::env::var("ZEN_API_KEY") {
        if !v.is_empty() {
            return Some(v);
        }
    }
    if let Ok(v) = std::env::var("OPENAI_API_KEY") {
        if !v.is_empty() {
            return Some(v);
        }
    }
    if let Some(env_name) = api_key_env {
        if !env_name.is_empty() {
            if let Ok(v) = std::env::var(env_name) {
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
    }
    None
}

/// Resolve permission mode from CLI > settings.defaultMode > "default".
/// Returns an error if bypassPermissions is forbidden but requested.
fn resolve_permission_mode(
    cli: &Cli,
    default_mode: Option<&str>,
    bypass_forbidden: bool,
) -> Result<PermissionMode> {
    if let Some(m) = &cli.permission_mode {
        // Check if bypass is forbidden but user requested bypassPermissions (C1.6)
        if bypass_forbidden && m == &PermissionMode::BypassPermissions {
            anyhow::bail!(
                "bypassPermissions mode is disabled by settings (disableBypassPermissionsMode)"
            );
        }
        return Ok(*m);
    }
    if let Some(m) = default_mode {
        if let Some(parsed) = PermissionMode::parse(m) {
            // Check if bypass is forbidden but settings requested bypassPermissions (C1.6)
            if bypass_forbidden && parsed == PermissionMode::BypassPermissions {
                anyhow::bail!(
                "bypassPermissions mode is disabled by settings (disableBypassPermissionsMode)"
            );
            }
            return Ok(parsed);
        }
    }
    Ok(PermissionMode::Default)
}

/// `--setting-sources user,project,local` → Some([...]); None = every layer.
fn sources_filter(raw: &Option<String>) -> Option<Vec<String>> {
    raw.as_ref().map(|s| {
        s.split(',')
            .map(|p| p.trim().to_lowercase())
            .filter(|p| !p.is_empty())
            .collect::<Vec<String>>()
    })
}

fn split_rules(inputs: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for raw in inputs {
        for part in raw.split(',') {
            let trimmed = part.trim();
            if !trimmed.is_empty() && !out.contains(&trimmed.to_string()) {
                out.push(trimmed.to_string());
            }
        }
    }
    out
}

/// Resolve full config from CLI + settings layers.
/// `home` is None → use dirs::home_dir (real run) or override for tests.
pub fn resolve_config(cli: &Cli, cwd: &std::path::Path) -> anyhow::Result<ResolvedConfig> {
    resolve_config_with_home(cli, cwd, dirs::home_dir().as_deref())
}

pub fn resolve_config_with_home(
    cli: &Cli,
    cwd: &std::path::Path,
    home: Option<&std::path::Path>,
) -> anyhow::Result<ResolvedConfig> {
    let enabled_sources: Vec<String> = match &cli.setting_sources {
        Some(s) => s
            .split(',')
            .map(|p| p.trim().to_lowercase())
            .filter(|p| !p.is_empty())
            .collect(),
        None => vec![
            "user".to_string(),
            "project".to_string(),
            "local".to_string(),
        ],
    };
    let sources_opt = sources_filter(&cli.setting_sources);
    let folder_trusted = folder_trusted(cli, cwd, home);
    let loaded = zen_ext::load_settings_trust(
        cwd,
        home,
        cli.settings.as_deref(),
        sources_opt.as_deref(),
        folder_trusted,
    );
    let trust_skipped = if folder_trusted { Vec::new() } else { zen_ext::gated_project_config(cwd) };
    let merged = loaded.settings;
    let loaded_sources = loaded.sources;
    let mut warnings = loaded.warnings;
    if cli.permission_mode.is_none() && merged.permissions.default_mode.as_deref() == Some("auto") {
        warnings.push("permission mode 'auto' is not supported; using 'default'".to_string());
    }

    // Surface warnings on stderr unless -p (per SPEC §12).
    if !cli.print {
        for w in &warnings {
            eprintln!("warning: {}", w);
        }
    }

    let raw_model = resolve_model(cli, merged.model.as_deref());
    // Named-provider routing: a `provider/model` value (e.g. `zai/glm-5.2`)
    // routes directly to that provider's API (see zen_agent::providers) --
    // but ONLY when the provider's key env var is set. Otherwise the `/`
    // prefix is part of a gateway model ID (e.g. the default
    // `google/gemini-4-argon:free` on the experientiallabs gateway) and the
    // default flow is left untouched.
    let spec = zen_agent::providers::ModelSpec::parse(&raw_model, DEFAULT_BASE_URL, false);
    let provider_key = spec
        .api_key_env
        .as_deref()
        .and_then(|e| std::env::var(e).ok())
        .filter(|v| !v.is_empty());
    let route_direct = spec.provider != "default" && provider_key.is_some();
    let model = if route_direct {
        spec.model.clone() // strip the `provider/` prefix on the wire
    } else {
        raw_model.clone()
    };
    let mut base_url = resolve_base_url(cli, merged.base_url.as_deref());
    if route_direct {
        let base_url_was_default = cli.base_url.is_none()
            && std::env::var("ZEN_BASE_URL")
                .map(|v| v.is_empty())
                .unwrap_or(true)
            && merged.base_url.as_deref().is_none_or(|u| u.is_empty())
            && base_url == DEFAULT_BASE_URL;
        if base_url_was_default {
            base_url = spec.base_url.clone();
        }
    } else if spec.provider != "default"
        && spec.provider != "ollama" // local, keyless: handled by the Ollama routing below
        && (cli.model.is_some()
            || std::env::var("ZEN_MODEL").map(|v| !v.is_empty()).unwrap_or(false))
    {
        warnings.push(format!(
            "provider '{}' recognized but {} is not set; sending '{}' to the default gateway",
            spec.provider,
            spec.api_key_env.as_deref().unwrap_or("its API key"),
            raw_model,
        ));
        warnings.push(format!(
            "set {} for direct routing to {}",
            spec.api_key_env.as_deref().unwrap_or("the API key"),
            spec.provider,
        ));
    }
    let api_key = match provider_key {
        Some(k) => Some(k),
        None => resolve_api_key(merged.api_key_env.as_deref()),
    };

    // ── Ollama provider routing ──────────────────────────────────────
    // `--model ollama/<name>` or `--provider ollama` (also ZEN_PROVIDER)
    // routes to the local Ollama instance: the prefix is stripped so the wire
    // sees the native model id, the base URL defaults to Ollama, and no API
    // key is sent to the local endpoint.
    let provider = cli.provider.clone().or_else(|| {
        std::env::var("ZEN_PROVIDER").ok().filter(|s| !s.is_empty())
    });
    if let Some(p) = provider.as_deref() {
        if p != "ollama" {
            anyhow::bail!("unknown --provider '{p}' (supported providers: ollama)");
        }
    }
    let base_url_explicit = cli.base_url.is_some()
        || std::env::var("ZEN_BASE_URL")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        || merged.base_url.as_deref().is_some_and(|s| !s.is_empty());
    let (model, base_url, api_key, ollama) = crate::ollama::apply_ollama_routing(
        provider.as_deref() == Some("ollama"),
        &model,
        &base_url,
        base_url_explicit,
        api_key,
        &crate::ollama::ollama_base_url(),
    );
    if ollama && model.is_empty() {
        anyhow::bail!("empty model name after 'ollama/' prefix (e.g. --model ollama/qwen3-coder:30b)");
    }

    let mut cli_allow = split_rules(&cli.allowed_tools);
    let mut cli_deny = split_rules(&cli.disallowed_tools);

    let mut allow = merged.permissions.allow.clone();
    dedup_append(&mut allow, &cli_allow);
    cli_allow = allow;
    let mut deny = merged.permissions.deny.clone();
    dedup_append(&mut deny, &cli_deny);
    cli_deny = deny;

    let permission_mode = resolve_permission_mode(cli, merged.permissions.default_mode.as_deref(), loaded.bypass_forbidden)?;

    let max_turns = cli
        .max_turns
        .or(merged.max_turns)
        .unwrap_or(DEFAULT_MAX_TURNS);

    Ok(ResolvedConfig {
        model,
        base_url,
        api_key,
        api_key_env: merged.api_key_env.clone(),
        allow_private_network: merged.allow_private_network,
        ollama,
        fallback_models: merged
            .fallback_models
            .iter()
            .map(|m| crate::ollama::strip_ollama_prefix(m).unwrap_or(m).to_string())
            .collect(),
        permissions: PermissionSettings {
            allow: cli_allow,
            ask: merged.permissions.ask.clone(),
            deny: cli_deny,
            default_mode: Some(permission_mode.as_str().to_string()),
            additional_directories: merged
                .permissions
                .additional_directories
                .clone(),
        },
        permission_mode,
        // `disableAllHooks: true` in any merged layer turns every hook off (C3.1).
        hooks: if merged.extra.get("disableAllHooks") == Some(&serde_json::Value::Bool(true)) {
            serde_json::json!({})
        } else {
            merged.hooks.clone()
        },
        env: merged.env.clone(),
        auto_compact: merged.auto_compact.clone(),
        verify: merged.extra.get("verify").cloned(),
        retry_max_wait_secs: retry_max_wait_secs(merged.extra.get("retry")),
        tool_timeouts: tool_timeouts(merged.extra.get("toolTimeouts")),
        max_continuations: max_continuations(merged.extra.get("maxContinuations")),
        max_turns,
        max_budget_usd: merged.extra.get("maxBudgetUsd").and_then(|v| v.as_f64()),
        budget_warn_at: merged.extra.get("budgetWarnAt").and_then(|v| v.as_f64()),
        context_window: merged.context_window,
        loaded_sources,
        warnings,
        setting_sources: enabled_sources.clone(),
        bypass_forbidden: loaded.bypass_forbidden,
        folder_trusted,
        trust_skipped,
        rate_gate: rate_gate_settings(&merged.rate_gate),
    })
}

/// Folder trust for this run: `--trust-folder`, `ZEN_TRUST_FOLDER=1`, or a saved trust entry.
pub fn folder_trusted(cli: &Cli, cwd: &std::path::Path, home: Option<&std::path::Path>) -> bool {
    cli.trust_folder || zen_ext::trust_env_set() || zen_ext::is_folder_trusted(cwd, home)
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use clap::Parser;
    use std::fs;

    /// Tests read/write process env (ZEN_MODEL, ZEN_BASE_URL) and resolve config from it;
    /// cargo runs tests in parallel threads, so serialize them on one lock.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn base_cli() -> Cli {
        Cli::parse_from(["zen"])
    }

    #[test]
    fn config_precedence_model_cli_over_env_over_default() {
        let _env = env_lock();
        let mut cli = base_cli();
        cli.model = Some("cli-model".into());

        // env var present but CLI should win
        let old = std::env::var("ZEN_MODEL").ok();
        std::env::set_var("ZEN_MODEL", "env-model");
        let cfg = resolve_config(&cli, std::env::temp_dir().as_path()).unwrap();
        std::env::remove_var("ZEN_MODEL");
        if let Some(o) = old {
            std::env::set_var("ZEN_MODEL", o);
        }
        assert_eq!(cfg.model, "cli-model");
    }

    #[test]
    fn config_precedence_env_over_default_when_no_cli() {
        let _env = env_lock();
        let cli = base_cli();
        let old = std::env::var("ZEN_MODEL").ok();
        std::env::set_var("ZEN_MODEL", "env-model");
        let cfg = resolve_config(&cli, std::env::temp_dir().as_path()).unwrap();
        std::env::remove_var("ZEN_MODEL");
        if let Some(o) = old {
            std::env::set_var("ZEN_MODEL", o);
        }
        assert_eq!(cfg.model, "env-model");
    }

    #[test]
    fn config_default_model() {
        let _env = env_lock();
        let cli = base_cli();
        let old_m = std::env::var("ZEN_MODEL").ok();
        let old_b = std::env::var("ZEN_BASE_URL").ok();
        std::env::remove_var("ZEN_MODEL");
        let cfg = resolve_config(&cli, std::env::temp_dir().as_path()).unwrap();
        if let Some(o) = old_m {
            std::env::set_var("ZEN_MODEL", o);
        }
        if let Some(o) = old_b {
            std::env::set_var("ZEN_BASE_URL", o);
        }
        assert_eq!(cfg.model, DEFAULT_MODEL);
        assert_eq!(cfg.base_url, DEFAULT_BASE_URL);
        assert_eq!(cfg.max_turns, DEFAULT_MAX_TURNS);
        assert_eq!(cfg.permission_mode, PermissionMode::Default);
    }

    #[test]
    fn config_permission_mode_precedence() {
        let _env = env_lock();
        let mut cli = base_cli();
        cli.permission_mode = Some(PermissionMode::Plan);
        let cfg = resolve_config(&cli, std::env::temp_dir().as_path()).unwrap();
        assert_eq!(cfg.permission_mode, PermissionMode::Plan);
    }

    #[test]
    fn config_settings_file_merge() {
        let _env = env_lock();
        let dir = std::env::temp_dir().join(format!("zen_cfg_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".zen")).unwrap();
        fs::write(
            dir.join(".zen/settings.json"),
            r#"{"model":"from-file","baseUrl":"https://file.example.com/v1","permissions":{"defaultMode":"acceptEdits","allow":["Read"]}}"#,
        )
        .unwrap();

        let cli = Cli::parse_from(["zen", "--setting-sources", "project", "--trust-folder"]);
        let cfg = resolve_config(&cli, &dir).unwrap();
        assert_eq!(cfg.model, "from-file");
        assert_eq!(cfg.base_url, "https://file.example.com/v1");
        assert_eq!(cfg.permission_mode, PermissionMode::AcceptEdits);
        assert!(cfg.permissions.allow.contains(&"Read".to_string()));
        assert!(!cfg.loaded_sources.is_empty());
    }

    #[test]
    fn config_cli_model_overrides_settings() {
        let _env = env_lock();
        let dir = std::env::temp_dir().join(format!("zen_cfg_test2_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".zen")).unwrap();
        fs::write(
            dir.join(".zen/settings.json"),
            r#"{"model":"from-file"}"#,
        )
        .unwrap();

        let cli = Cli::parse_from(["zen", "--model", "cli-override", "--setting-sources", "project"]);
        let cfg = resolve_config(&cli, &dir).unwrap();
        assert_eq!(cfg.model, "cli-override");
    }

    /// C1.3: `ZEN_MODEL` beats settings `model`; `--model` beats both.
    #[test]
    fn c1_3_env_model_overrides_settings_and_flag_overrides_env() {
        let _env = env_lock();
        let dir = std::env::temp_dir().join(format!("zen_cfg_c13_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".zen")).unwrap();
        fs::write(dir.join(".zen/settings.json"), r#"{"model":"from-file"}"#).unwrap();
        let old = std::env::var("ZEN_MODEL").ok();
        std::env::set_var("ZEN_MODEL", "env-model");
        let cli = Cli::parse_from(["zen", "--setting-sources", "project"]);
        let from_env = resolve_config(&cli, &dir).unwrap().model;
        let cli = Cli::parse_from(["zen", "--model", "flag-model", "--setting-sources", "project"]);
        let from_flag = resolve_config(&cli, &dir).unwrap().model;
        std::env::remove_var("ZEN_MODEL");
        let cli = Cli::parse_from(["zen", "--setting-sources", "project"]);
        let from_file = resolve_config(&cli, &dir).unwrap().model;
        if let Some(o) = old {
            std::env::set_var("ZEN_MODEL", o);
        }
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(from_env, "env-model");
        assert_eq!(from_flag, "flag-model");
        assert_eq!(from_file, "from-file");
    }

    /// C2.11: settings `defaultMode: "manual"` is default; `"auto"` is default plus a warning.
    #[test]
    fn c2_11_settings_mode_aliases() {
        let _env = env_lock();
        let dir = std::env::temp_dir().join(format!("zen_cfg_c211_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".zen")).unwrap();
        let cli = Cli::parse_from(["zen", "-p", "--setting-sources", "project"]);
        fs::write(dir.join(".zen/settings.json"), r#"{"permissions":{"defaultMode":"manual"}}"#).unwrap();
        let manual = resolve_config(&cli, &dir).unwrap();
        fs::write(dir.join(".zen/settings.json"), r#"{"permissions":{"defaultMode":"auto"}}"#).unwrap();
        let auto = resolve_config(&cli, &dir).unwrap();
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(manual.permission_mode, PermissionMode::Default);
        assert!(manual.warnings.is_empty());
        assert_eq!(auto.permission_mode, PermissionMode::Default);
        assert!(auto.warnings.iter().any(|w| w.contains("'auto'")), "{:?}", auto.warnings);
    }

    #[test]
    fn config_allowed_tools_split_and_dedup() {
        let _env = env_lock();
        let cli = Cli::parse_from([
            "zen",
            "--allowedTools",
            "Read,Bash",
            "--allowedTools",
            "Bash,Write",
        ]);
        let cfg = resolve_config(&cli, std::env::temp_dir().as_path()).unwrap();
        assert_eq!(cfg.permissions.allow, vec!["Read".to_string(), "Bash".to_string(), "Write".to_string()]);
    }

    #[test]
    fn config_deny_dedup() {
        let _env = env_lock();
        let cli = Cli::parse_from(["zen", "--disallowedTools", "Write", "--disallowedTools", "Write"]);
        let cfg = resolve_config(&cli, std::env::temp_dir().as_path()).unwrap();
        assert_eq!(cfg.permissions.deny, vec!["Write".to_string()]);
    }

    #[test]
    fn config_no_settings_file_ok() {
        let _env = env_lock();
        let cli = base_cli();
        let dir = std::env::temp_dir().join(format!("zen_cfg_empty_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let cfg = resolve_config(&cli, &dir).unwrap();
        assert!(cfg.warnings.is_empty());
        assert!(cfg.loaded_sources.is_empty());
    }

    #[test]
    fn config_setting_sources_filter() {
        let _env = env_lock();
        let cli = Cli::parse_from(["zen", "--setting-sources", ""]);
        assert!(cli.setting_sources.is_some());
        let cfg = resolve_config(&cli, std::env::temp_dir().as_path()).unwrap();
        assert!(cfg.setting_sources.is_empty());
    }

    #[test]
    fn folder_trust_gates_project_hooks_and_allow_rules() {
        let _env = env_lock();
        let dir = std::env::temp_dir().join(format!("zen_cfg_trust_{}", std::process::id()));
        let home = dir.join("home");
        let proj = dir.join("proj");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(proj.join(".zen")).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::write(
            proj.join(".zen/settings.json"),
            r#"{"model":"pm","hooks":{"Stop":[{"hooks":[{"type":"command","command":"true"}]}]},"permissions":{"allow":["Bash"]}}"#,
        )
        .unwrap();
        let old = std::env::var(zen_ext::TRUST_ENV).ok();
        std::env::remove_var(zen_ext::TRUST_ENV);

        // Untrusted by default: non-exec keys load, hooks and allow rules do not.
        let cfg = resolve_config_with_home(&base_cli(), &proj, Some(&home)).unwrap();
        assert!(!cfg.folder_trusted);
        assert_eq!(cfg.model, "pm");
        assert!(cfg.hooks.get("Stop").is_none());
        assert!(cfg.permissions.allow.is_empty());
        assert_eq!(cfg.trust_skipped, vec![".zen/settings.json: hooks, permissions.allow"]);

        // --trust-folder (one run, not saved).
        let cli = Cli::parse_from(["zen", "--trust-folder"]);
        let cfg = resolve_config_with_home(&cli, &proj, Some(&home)).unwrap();
        assert!(cfg.folder_trusted && cfg.hooks.get("Stop").is_some());
        assert!(!zen_ext::is_folder_trusted(&proj, Some(&home)));

        // ZEN_TRUST_FOLDER=1.
        std::env::set_var(zen_ext::TRUST_ENV, "1");
        let cfg = resolve_config_with_home(&base_cli(), &proj, Some(&home)).unwrap();
        std::env::remove_var(zen_ext::TRUST_ENV);
        assert!(cfg.folder_trusted && cfg.permissions.allow == vec!["Bash"]);

        // Saved trust in the user config.
        zen_ext::trust_folder(&proj, Some(&home)).unwrap();
        assert!(home.join(".zen/trusted-folders.json").is_file());
        let cfg = resolve_config_with_home(&base_cli(), &proj, Some(&home)).unwrap();
        assert!(cfg.folder_trusted && cfg.hooks.get("Stop").is_some() && cfg.trust_skipped.is_empty());

        if let Some(o) = old {
            std::env::set_var(zen_ext::TRUST_ENV, o);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn c3_1_disable_all_hooks_clears_hooks() {
        let _env = env_lock();
        let dir = std::env::temp_dir().join(format!("zen_cfg_nohooks_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".zen")).unwrap();
        fs::write(
            dir.join(".zen/settings.json"),
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"true"}]}]}}"#,
        )
        .unwrap();
        let cli = Cli::parse_from(["zen", "--setting-sources", "project,local", "--trust-folder"]);
        let cfg = resolve_config(&cli, &dir).unwrap();
        assert!(cfg.hooks.get("Stop").is_some());

        fs::write(dir.join(".zen/settings.local.json"), r#"{"disableAllHooks":true}"#).unwrap();
        let cfg = resolve_config(&cli, &dir).unwrap();
        assert_eq!(cfg.hooks, serde_json::json!({}));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn trunc_max_continuations_parse() {
        assert_eq!(max_continuations(None), zen_agent::DEFAULT_MAX_CONTINUATIONS);
        assert_eq!(max_continuations(Some(&serde_json::json!(0))), 0);
        assert_eq!(max_continuations(Some(&serde_json::json!(5))), 5);
        assert_eq!(max_continuations(Some(&serde_json::json!(-1))), zen_agent::DEFAULT_MAX_CONTINUATIONS);
        assert_eq!(max_continuations(Some(&serde_json::json!("x"))), zen_agent::DEFAULT_MAX_CONTINUATIONS);
    }

    #[test]
    fn p0b_2_tool_timeouts_parse() {
        let t = tool_timeouts(Some(&serde_json::json!({"Bash": 180000, "default": 0, "Bad": "x", "Neg": -1})));
        assert_eq!(t.get("Bash"), Some(&180000));
        assert_eq!(t.get("default"), Some(&0));
        assert_eq!(t.len(), 2);
        assert!(tool_timeouts(None).is_empty());
        assert!(tool_timeouts(Some(&serde_json::json!([1]))).is_empty());
    }

    #[test]
    fn p0_4_retry_max_wait_secs_parses_with_default() {
        use serde_json::json;
        assert_eq!(retry_max_wait_secs(None), 300);
        assert_eq!(retry_max_wait_secs(Some(&json!({}))), 300);
        assert_eq!(retry_max_wait_secs(Some(&json!({"maxWaitSecs": 30}))), 30);
        assert_eq!(retry_max_wait_secs(Some(&json!({"maxWaitSecs": 0}))), 0);
        assert_eq!(retry_max_wait_secs(Some(&json!({"maxWaitSecs": "x"}))), 300);
        assert_eq!(retry_max_wait_secs(Some(&json!({"maxWaitSecs": -5}))), 300);
    }
    #[test]
    fn provider_routing_default_model_untouched_without_key() {
        // The default `google/gemini-4-argon:free` is a gateway model ID, not a
        // routing instruction: without GOOGLE_API_KEY the gateway flow stays.
        let _env = env_lock();
        let old = std::env::var("GOOGLE_API_KEY").ok();
        std::env::remove_var("GOOGLE_API_KEY");
        let old_m = std::env::var("ZEN_MODEL").ok();
        std::env::remove_var("ZEN_MODEL");
        let cli = base_cli();
        let cfg = resolve_config(&cli, std::env::temp_dir().as_path()).unwrap();
        assert_eq!(cfg.model, "google/gemini-4-argon:free");
        assert_eq!(cfg.base_url, DEFAULT_BASE_URL);
        assert!(cfg.warnings.iter().all(|w| !w.contains("provider 'google'")));
        if let Some(o) = old {
            std::env::set_var("GOOGLE_API_KEY", o);
        }
        if let Some(o) = old_m {
            std::env::set_var("ZEN_MODEL", o);
        }
    }

    #[test]
    fn provider_routing_direct_when_key_set() {
        // `--model zai/glm-5.2` + ZAI_API_KEY routes directly to Z.ai.
        let _env = env_lock();
        let old = std::env::var("ZAI_API_KEY").ok();
        std::env::set_var("ZAI_API_KEY", "test-key-zai");
        let cli = Cli::parse_from(["zen", "--model", "zai/glm-5.2"]);
        let cfg = resolve_config(&cli, std::env::temp_dir().as_path()).unwrap();
        assert_eq!(cfg.model, "glm-5.2", "prefix stripped on the wire");
        assert_eq!(cfg.base_url, "https://api.z.ai/api/paas/v4");
        assert_eq!(cfg.api_key.as_deref(), Some("test-key-zai"));
        if let Some(o) = old {
            std::env::set_var("ZAI_API_KEY", o);
        } else {
            std::env::remove_var("ZAI_API_KEY");
        }
    }

    #[test]
    fn provider_routing_warns_without_key() {
        // Explicit `--model moonshot/kimi-k2.7` without the key: gateway flow +
        // a warning pointing at `zen auth login`.
        let _env = env_lock();
        let old = std::env::var("MOONSHOT_API_KEY").ok();
        std::env::remove_var("MOONSHOT_API_KEY");
        let cli = Cli::parse_from(["zen", "--model", "moonshot/kimi-k2.7"]);
        let cfg = resolve_config(&cli, std::env::temp_dir().as_path()).unwrap();
        assert_eq!(cfg.model, "moonshot/kimi-k2.7", "full spec kept for gateway");
        assert_eq!(cfg.base_url, DEFAULT_BASE_URL);
        assert!(
            cfg.warnings.iter().any(|w| w.contains("MOONSHOT_API_KEY")),
            "expected key warning, got: {:?}",
            cfg.warnings
        );
        if let Some(o) = old {
            std::env::set_var("MOONSHOT_API_KEY", o);
        }
    }

    #[test]
    fn provider_routing_explicit_base_url_wins() {
        // An explicit --base-url beats the provider default endpoint.
        let _env = env_lock();
        let old = std::env::var("DEEPSEEK_API_KEY").ok();
        std::env::set_var("DEEPSEEK_API_KEY", "test-key-ds");
        let cli = Cli::parse_from([
            "zen",
            "--model",
            "deepseek/deepseek-v4-flash",
            "--base-url",
            "https://proxy.example.com/v1",
        ]);
        let cfg = resolve_config(&cli, std::env::temp_dir().as_path()).unwrap();
        assert_eq!(cfg.model, "deepseek-v4-flash");
        assert_eq!(cfg.base_url, "https://proxy.example.com/v1");
        assert_eq!(cfg.api_key.as_deref(), Some("test-key-ds"));
        if let Some(o) = old {
            std::env::set_var("DEEPSEEK_API_KEY", o);
        } else {
            std::env::remove_var("DEEPSEEK_API_KEY");
        }
    }

    // ── Ollama provider routing ──────────────────────────────────────

    fn clear_ollama_env() {
        for v in ["ZEN_MODEL", "ZEN_BASE_URL", "ZEN_PROVIDER", "OLLAMA_HOST"] {
            std::env::remove_var(v);
        }
    }

    #[test]
    fn ollama_model_prefix_routes_to_local() {
        let _env = env_lock();
        clear_ollama_env();
        let mut cli = base_cli();
        cli.model = Some("ollama/qwen3-coder:30b".into());
        let cfg = resolve_config(&cli, std::env::temp_dir().as_path()).unwrap();
        assert!(cfg.ollama);
        assert_eq!(cfg.model, "qwen3-coder:30b");
        assert_eq!(cfg.base_url, "http://localhost:11434/v1");
        assert_eq!(cfg.api_key, None);
    }

    #[test]
    fn ollama_provider_flag_routes_without_prefix() {
        let _env = env_lock();
        clear_ollama_env();
        let mut cli = base_cli();
        cli.provider = Some("ollama".into());
        cli.model = Some("llama3.1".into());
        let cfg = resolve_config(&cli, std::env::temp_dir().as_path()).unwrap();
        assert!(cfg.ollama);
        assert_eq!(cfg.model, "llama3.1");
        assert_eq!(cfg.base_url, "http://localhost:11434/v1");
    }

    #[test]
    fn ollama_provider_env_routes() {
        let _env = env_lock();
        clear_ollama_env();
        std::env::set_var("ZEN_PROVIDER", "ollama");
        let mut cli = base_cli();
        cli.model = Some("ollama/mistral:7b".into());
        let cfg = resolve_config(&cli, std::env::temp_dir().as_path()).unwrap();
        std::env::remove_var("ZEN_PROVIDER");
        assert!(cfg.ollama);
        assert_eq!(cfg.model, "mistral:7b");
    }

    #[test]
    fn ollama_host_env_overrides_default() {
        let _env = env_lock();
        clear_ollama_env();
        std::env::set_var("OLLAMA_HOST", "192.168.1.5:11434");
        let mut cli = base_cli();
        cli.model = Some("ollama/qwen3-coder:30b".into());
        let cfg = resolve_config(&cli, std::env::temp_dir().as_path()).unwrap();
        std::env::remove_var("OLLAMA_HOST");
        assert!(cfg.ollama);
        assert_eq!(cfg.base_url, "http://192.168.1.5:11434/v1");
    }

    #[test]
    fn ollama_explicit_base_url_wins_over_default() {
        let _env = env_lock();
        clear_ollama_env();
        let mut cli = base_cli();
        cli.model = Some("ollama/qwen3-coder:30b".into());
        cli.base_url = Some("http://gateway.internal:11434/v1".into());
        let cfg = resolve_config(&cli, std::env::temp_dir().as_path()).unwrap();
        assert!(cfg.ollama);
        assert_eq!(cfg.model, "qwen3-coder:30b");
        assert_eq!(cfg.base_url, "http://gateway.internal:11434/v1");
    }

    #[test]
    fn ollama_empty_model_name_rejected() {
        let _env = env_lock();
        clear_ollama_env();
        let mut cli = base_cli();
        cli.model = Some("ollama/".into());
        let err = resolve_config(&cli, std::env::temp_dir().as_path()).unwrap_err();
        assert!(err.to_string().contains("ollama/"), "unexpected: {err}");
    }

    #[test]
    fn unknown_provider_rejected() {
        let _env = env_lock();
        clear_ollama_env();
        let mut cli = base_cli();
        cli.provider = Some("wat".into());
        let err = resolve_config(&cli, std::env::temp_dir().as_path()).unwrap_err();
        assert!(err.to_string().contains("unknown --provider"), "unexpected: {err}");
    }

    #[test]
    fn non_ollama_model_untouched() {
        let _env = env_lock();
        clear_ollama_env();
        let mut cli = base_cli();
        cli.model = Some("gpt-4o".into());
        let cfg = resolve_config(&cli, std::env::temp_dir().as_path()).unwrap();
        assert!(!cfg.ollama);
        assert_eq!(cfg.model, "gpt-4o");
    }
}
