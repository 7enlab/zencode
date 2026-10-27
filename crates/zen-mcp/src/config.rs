use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use regex::Regex;

pub use crate::manager::SkipReason;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpServerConfig {

    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

impl McpServerConfig {
    pub fn is_http(&self) -> bool {
        self.kind.as_deref().map(|k| k.eq_ignore_ascii_case("http")).unwrap_or(false)
            || (self.command.is_none() && self.url.is_some())
    }

    pub fn expand_with_missing(&self, missing: &mut Vec<String>) -> Option<McpServerConfig> {
        let expand_value = |value: &str, missing: &mut Vec<String>| -> Option<String> {
            let re = Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)(?:\:-(.*?))?\}").unwrap();
            let mut result = value.to_string();
            let mut any_missing = false;
            for cap in re.captures_iter(value) {
                let full = cap.get(0).unwrap().as_str();
                let var = cap.get(1).unwrap().as_str();
                let default = cap.get(2).map(|m| m.as_str());
                let replacement = std::env::var(var).ok().or(default.map(|s| s.to_string()));
                if let Some(repl) = replacement {
                    result = result.replace(full, &repl);
                } else {

                    missing.push(var.to_string());
                    any_missing = true;
                }
            }
            if any_missing {
                None
            } else {
                Some(result)
            }
        };

        let mut cfg = self.clone();
        if let Some(cmd) = &cfg.command {
            cfg.command = Some(expand_value(cmd, missing)?);
        }
        if !cfg.args.is_empty() {
            let mut expanded_args = Vec::with_capacity(cfg.args.len());
            let mut args_missing = false;
            for arg in &cfg.args {
                if let Some(expanded) = expand_value(arg, missing) {
                    expanded_args.push(expanded);
                } else {
                    args_missing = true;
                }
            }
            if args_missing {
                return None;
            }
            cfg.args = expanded_args;
        }
        if !cfg.env.is_empty() {
            let mut new_env = std::collections::BTreeMap::new();
            let mut env_missing = false;
            for (k, v) in &cfg.env {
                if let Some(expanded) = expand_value(v, missing) {
                    new_env.insert(k.clone(), expanded);
                } else {
                    env_missing = true;
                }
            }
            if env_missing {
                return None;
            }
            cfg.env = new_env;
        }
        if let Some(url) = &cfg.url {
            cfg.url = Some(expand_value(url, missing)?);
        }

        let mut expanded_headers = BTreeMap::new();
        let mut headers_missing = false;
        for (k, v) in &cfg.headers {
            if let Some(expanded_k) = expand_value(k, missing) {
                if let Some(expanded_v) = expand_value(v, missing) {
                    expanded_headers.insert(expanded_k, expanded_v);
                } else {
                    headers_missing = true;
                }
            } else {
                headers_missing = true;
            }
        }
        if headers_missing {
            return None;
        }
        cfg.headers = expanded_headers;

        Some(cfg)
    }

    pub fn expand_env(&self) -> Option<McpServerConfig> {
        let mut missing = Vec::new();
        self.expand_with_missing(&mut missing)
    }
}

fn parse_servers(text: &str) -> anyhow::Result<BTreeMap<String, McpServerConfig>> {
    let value: Value = serde_json::from_str(text)?;
    let map = match value.get("mcpServers").and_then(|v| v.as_object()) {
        Some(obj) => obj.clone(),
        None => match value.as_object() {
            Some(obj) => obj.clone(),
            None => return Err(anyhow::anyhow!("MCP config must be a JSON object")),
        },
    };
    let mut out = BTreeMap::new();
    for (name, cfg) in map {
        let parsed: McpServerConfig = serde_json::from_value(cfg)
            .map_err(|e| anyhow::anyhow!("invalid MCP server '{}': {}", name, e))?;
        out.insert(name, parsed);
    }
    Ok(out)
}

fn merge_file(
    into: &mut BTreeMap<String, McpServerConfig>,
    path: &Path,
) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let text = std::fs::read_to_string(path)?;
    let servers = parse_servers(&text)?;
    for (name, cfg) in servers {
        into.insert(name, cfg);
    }
    Ok(())
}

fn load_cli_entry(
    into: &mut BTreeMap<String, McpServerConfig>,
    entry: &str,
) -> anyhow::Result<()> {
    let trimmed = entry.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let text = if trimmed.starts_with('{') {
        trimmed.to_string()
    } else {
        std::fs::read_to_string(PathBuf::from(trimmed))?
    };
    let servers = parse_servers(&text)?;
    for (name, cfg) in servers {
        into.insert(name, cfg);
    }
    Ok(())
}

fn home_dir(home: Option<&Path>) -> Option<PathBuf> {
    match home {
        Some(h) => Some(h.to_path_buf()),
        None => dirs::home_dir(),
    }
}

pub fn load_mcp_config_file(path: &Path) -> anyhow::Result<BTreeMap<String, McpServerConfig>> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let text = std::fs::read_to_string(path)?;
    parse_servers(&text)
}

pub fn save_mcp_config_file(path: &Path, configs: &BTreeMap<String, McpServerConfig>) -> anyhow::Result<()> {

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut map = serde_json::Map::new();
    let mut servers_map = serde_json::Map::new();
    for (name, cfg) in configs {
        servers_map.insert(name.clone(), serde_json::to_value(cfg)?);
    }
    map.insert("mcpServers".to_string(), serde_json::Value::Object(servers_map));

    let json = serde_json::to_string_pretty(&serde_json::Value::Object(map))?;
    std::fs::write(path, json)?;
    Ok(())
}

pub fn mcp_add(scope_path: &Path, name: &str, cfg: McpServerConfig) -> anyhow::Result<()> {
    let mut configs = load_mcp_config_file(scope_path)?;
    configs.insert(name.to_string(), cfg);
    save_mcp_config_file(scope_path, &configs)?;
    Ok(())
}

pub fn mcp_remove(scope_path: &Path, name: &str) -> anyhow::Result<()> {
    let mut configs = load_mcp_config_file(scope_path)?;
    configs.remove(name);
    save_mcp_config_file(scope_path, &configs)?;
    Ok(())
}

pub fn mcp_get(scope_path: &Path, name: &str) -> Option<McpServerConfig> {
    match load_mcp_config_file(scope_path) {
        Ok(configs) => configs.get(name).cloned(),
        Err(_) => None,
    }
}

pub fn mcp_list(scope_path: &Path) -> Vec<(String, McpServerConfig)> {
    match load_mcp_config_file(scope_path) {
        Ok(configs) => configs.into_iter().collect(),
        Err(_) => Vec::new(),
    }
}

fn merge_file_with_skips(
    into: &mut BTreeMap<String, McpServerConfig>,
    path: PathBuf,
    _skipped: &mut [SkipReason],
) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("failed to read config '{}': {}", path.display(), e))?;
    let servers = parse_servers(&text).map_err(|e| anyhow::anyhow!("invalid MCP config '{}': {}", path.display(), e))?;
    for (name, cfg) in servers {
        into.insert(name, cfg);
    }
    Ok(())
}

fn load_cli_entry_with_skips(
    into: &mut BTreeMap<String, McpServerConfig>,
    entry: &str,
) -> anyhow::Result<()> {
    let trimmed = entry.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let text = if trimmed.starts_with('{') {
        trimmed.to_string()
    } else {
        std::fs::read_to_string(PathBuf::from(trimmed))
            .map_err(|e| anyhow::anyhow!("failed to read CLI config '{}': {}", trimmed, e))?
    };
    let servers = parse_servers(&text).map_err(|e| anyhow::anyhow!("invalid CLI config '{}': {}", trimmed, e))?;
    for (name, cfg) in servers {
        into.insert(name, cfg);
    }
    Ok(())
}

pub fn scope_path(scope: &str, cwd: &Path, home: Option<&Path>) -> anyhow::Result<PathBuf> {
    match scope {
        "local" => Ok(cwd.join(".zen").join("mcp.json")),
        "project" => Ok(cwd.join(".mcp.json")),
        "user" => home_dir(home)
            .ok_or_else(|| anyhow::anyhow!("home directory not available"))
            .map(|h| h.join(".zen").join("mcp.json")),
        _ => Err(anyhow::anyhow!("unknown scope '{}' (expected local, project or user)", scope)),
    }
}

fn config_files(cwd: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Some(h) = home_dir(home) {
        files.push(h.join(".zen.json"));
        files.push(h.join(".zen").join("mcp.json"));
    }
    files.push(cwd.join(".mcp.json"));
    files.push(cwd.join(".zen").join("mcp.json"));
    files
}

pub fn parse_env_kv(entries: &[String]) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for entry in entries {
        if let Some((k, v)) = entry.split_once('=') {
            if !k.is_empty() {
                map.insert(k.to_string(), v.to_string());
            }
        }
    }
    map
}

pub fn parse_headers(entries: &[String]) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    for entry in entries {
        if let Some((k, v)) = entry.split_once(':') {
            let key = k.trim().to_string();
            let value = v.trim().to_string();
            if !key.is_empty() {
                headers.push((key, value));
            }
        }
    }
    headers
}

pub fn mcp_add_json(path: &Path, name: &str, json_str: &str) -> anyhow::Result<()> {
    let mut configs = load_mcp_config_file(path)?;
    let cfg: McpServerConfig = serde_json::from_str(json_str)
        .map_err(|e| anyhow::anyhow!("invalid JSON for MCP server config: {}", e))?;
    configs.insert(name.to_string(), cfg);
    save_mcp_config_file(path, &configs)?;
    Ok(())
}

pub fn load_mcp_configs_ex(
    cwd: &Path,
    home: Option<&Path>,
    cli_configs: &[String],
    strict: bool,
) -> (BTreeMap<String, McpServerConfig>, Vec<SkipReason>) {
    let mut out = BTreeMap::new();
    let mut skipped = Vec::new();

    if !strict {

        for f in config_files(cwd, home) {
            let _ = merge_file_with_skips(&mut out, f, &mut skipped);
        }
    }

    for entry in cli_configs {
        let _ = load_cli_entry_with_skips(&mut out, entry);
    }

    let mut final_skipped = Vec::new();
    let mut validated = BTreeMap::new();
    for (name, cfg) in out {
        let mut missing = Vec::new();
        let expanded = cfg.expand_with_missing(&mut missing);
        if let Some(expanded_cfg) = expanded {

            if expanded_cfg.url.is_some() && expanded_cfg.kind.is_none() {
                final_skipped.push(SkipReason {
                    server_name: name.clone(),
                    reason: "url_missing_type".to_string(),
                });
                continue;
            }

            if let Some(ref k) = expanded_cfg.kind {
                let k_lower = k.to_lowercase();
                if k_lower != "stdio" && k_lower != "http" {
                    final_skipped.push(SkipReason {
                        server_name: name.clone(),
                        reason: "unknown_type".to_string(),
                    });
                    continue;
                }
            }
            validated.insert(name, expanded_cfg);
        } else {

            for var in missing {
                final_skipped.push(SkipReason {
                    server_name: name.clone(),
                    reason: format!("missing_env:{}", var),
                });
            }
        }
    }

    (validated, final_skipped)
}

pub fn load_mcp_configs(
    cwd: &Path,
    home: Option<&Path>,
    cli_configs: &[String],
    strict: bool,
) -> anyhow::Result<BTreeMap<String, McpServerConfig>> {
    load_mcp_configs_scoped(cwd, home, cli_configs, strict, true)
}

pub fn load_mcp_configs_scoped(
    cwd: &Path,
    home: Option<&Path>,
    cli_configs: &[String],
    strict: bool,
    include_project: bool,
) -> anyhow::Result<BTreeMap<String, McpServerConfig>> {
    let mut out = BTreeMap::new();
    if !strict {

        let project = [cwd.join(".mcp.json"), cwd.join(".zen").join("mcp.json")];
        for f in config_files(cwd, home) {
            if include_project || !project.contains(&f) {
                merge_file(&mut out, &f)?;
            }
        }
    }
    for entry in cli_configs {
        load_cli_entry(&mut out, entry)?;
    }
    Ok(out)
}
