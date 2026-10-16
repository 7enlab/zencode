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
    pub fallback_models: Vec<String>,
    pub permissions: PermissionSettings,
    pub permission_mode: PermissionMode,
    pub hooks: serde_json::Value,
    pub env: std::collections::HashMap<String, String>,
    pub auto_compact: Option<serde_json::Value>,
    /// Raw `verify` object: `{ "enabled": bool, "command": "...", "timeoutSecs": n }` (r7).
    pub verify: Option<serde_json::Value>,
    pub max_turns: usize,
    pub context_window: Option<u64>,
    pub loaded_sources: Vec<String>,
    pub warnings: Vec<String>,
    pub setting_sources: Vec<String>,
    pub bypass_forbidden: bool,
}

impl ResolvedConfig {
    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
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
    let loaded = zen_ext::load_settings(cwd, home, cli.settings.as_deref(), sources_opt.as_deref());
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

    let model = resolve_model(cli, merged.model.as_deref());
    let base_url = resolve_base_url(cli, merged.base_url.as_deref());
    let api_key = resolve_api_key(merged.api_key_env.as_deref());

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
        fallback_models: merged.fallback_models.clone(),
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
        max_turns,
        context_window: merged.context_window,
        loaded_sources,
        warnings,
        setting_sources: enabled_sources.clone(),
        bypass_forbidden: loaded.bypass_forbidden,
    })
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

        let cli = Cli::parse_from(["zen", "--setting-sources", "project"]);
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
        let cli = Cli::parse_from(["zen", "--setting-sources", "project,local"]);
        let cfg = resolve_config(&cli, &dir).unwrap();
        assert!(cfg.hooks.get("Stop").is_some());

        fs::write(dir.join(".zen/settings.local.json"), r#"{"disableAllHooks":true}"#).unwrap();
        let cfg = resolve_config(&cli, &dir).unwrap();
        assert_eq!(cfg.hooks, serde_json::json!({}));
        let _ = fs::remove_dir_all(&dir);
    }
}
