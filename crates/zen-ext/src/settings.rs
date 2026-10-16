//! Settings layering (SPEC §12)

use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use zen_core::Settings;

/// Result of loading and merging settings from all layers.
#[derive(Debug, Clone)]
pub struct LoadedSettings {
    pub settings: Settings,
    /// Human names of layers that loaded, e.g. ["user", "project", "local", "cli"].
    pub sources: Vec<String>,
    /// Problems (invalid JSON etc.). Broken files are skipped, never fatal.
    pub warnings: Vec<String>,
    /// True if any layer has disableBypassPermissionsMode = "disable".
    pub bypass_forbidden: bool,
}

/// Deep merge: objects merge recursively, arrays concatenate+dedupe, scalars: `over` wins.
pub fn merge_json(base: Value, over: Value) -> Value {
    match (base, over) {
        (Value::Object(mut b), Value::Object(o)) => {
            for (k, v) in o {
                let existing = b.remove(&k);
                let merged = match existing {
                    Some(ev) => merge_json(ev, v),
                    None => v,
                };
                b.insert(k, merged);
            }
            Value::Object(b)
        }
        (Value::Array(b), Value::Array(o)) => {
            let mut out = b;
            for v in o {
                if !out.contains(&v) {
                    out.push(v);
                }
            }
            Value::Array(out)
        }
        (Value::Null, over) => over,
        (base, Value::Null) => base,
        (_, over) => over,
    }
}

/// `--setting-sources` selects only the user/project/local files; `--settings` and managed
/// settings always load (SPEC C1.1).
fn is_enabled(sources: Option<&[String]>, name: &str) -> bool {
    if matches!(name, "cli" | "managed") {
        return true;
    }
    sources.is_none_or(|s| s.iter().any(|x| x.eq_ignore_ascii_case(name)))
}

fn single(key: &str, value: Value) -> Value {
    let mut m = Map::new();
    m.insert(key.to_string(), value);
    Value::Object(m)
}

/// Does this fragment deserialize into [`Settings`]?
fn fragment_ok(v: &Value) -> bool {
    serde_json::from_value::<Settings>(v.clone()).is_ok()
}

/// Keep the valid entries of an array found at `wrap(item)`; warn about each dropped one.
/// `None` when the field is not an array field at all (the whole value is skipped).
fn filter_array(
    items: Vec<Value>,
    wrap: &dyn Fn(Value) -> Value,
    what: &str,
    label: &str,
    warnings: &mut Vec<String>,
) -> Option<Value> {
    if !fragment_ok(&wrap(Value::Array(vec![]))) {
        warnings.push(format!("{} settings: invalid value for '{}' skipped", label, what));
        return None;
    }
    let mut kept = Vec::new();
    for item in items {
        if fragment_ok(&wrap(Value::Array(vec![item.clone()]))) {
            kept.push(item);
        } else {
            warnings.push(format!("{} settings: invalid entry {} in '{}' skipped", label, item, what));
        }
    }
    Some(Value::Array(kept))
}

/// Drop individually invalid entries (wrong type) so one bad key never discards the whole
/// layer (SPEC C1.2). Checks top-level keys, then keys of nested objects, then array items.
fn sanitize_layer(layer: Value, label: &str, warnings: &mut Vec<String>) -> Value {
    let Value::Object(map) = layer else { return layer };
    let mut out = Map::new();
    for (k, v) in map {
        if fragment_ok(&single(&k, v.clone())) {
            out.insert(k, v);
            continue;
        }
        match v {
            Value::Object(sub) => {
                let mut kept = Map::new();
                for (sk, sv) in sub {
                    if fragment_ok(&single(&k, single(&sk, sv.clone()))) {
                        kept.insert(sk, sv);
                    } else if let Value::Array(items) = sv {
                        let wrap = |a: Value| single(&k, single(&sk, a));
                        let what = format!("{}.{}", k, sk);
                        if let Some(arr) = filter_array(items, &wrap, &what, label, warnings) {
                            kept.insert(sk.clone(), arr);
                        }
                    } else {
                        warnings.push(format!("{} settings: invalid value for '{}.{}' skipped", label, k, sk));
                    }
                }
                out.insert(k, Value::Object(kept));
            }
            Value::Array(items) => {
                let wrap = |a: Value| single(&k, a);
                if let Some(arr) = filter_array(items, &wrap, &k, label, warnings) {
                    out.insert(k, arr);
                }
            }
            _ => warnings.push(format!("{} settings: invalid value for '{}' skipped", label, k)),
        }
    }
    Value::Object(out)
}

fn read_json_file(path: &Path, label: &str, warnings: &mut Vec<String>) -> Option<Value> {
    if !path.exists() || !path.is_file() {
        return None;
    }
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            warnings.push(format!("Failed to read {} settings {}: {}", label, path.display(), e));
            return None;
        }
    };
    match serde_json::from_str::<Value>(&content) {
        Ok(v @ Value::Object(_)) => Some(sanitize_layer(v, label, warnings)),
        Ok(_) => {
            warnings.push(format!("{} settings {} must be a JSON object", label, path.display()));
            None
        }
        Err(e) => {
            warnings.push(format!("Invalid JSON in {} settings {}: {}", label, path.display(), e));
            None
        }
    }
}

fn load_file_layer(
    path: &Path,
    label: &str,
    merged: &mut Value,
    sources: &mut Vec<String>,
    warnings: &mut Vec<String>,
    bypass_forbidden: &mut bool,
) {
    if let Some(layer) = read_json_file(path, label, warnings) {
        // Check if this layer disables bypass permissions
        if let Some(disable_bypass) = layer.get("disableBypassPermissionsMode") {
            if disable_bypass.as_str() == Some("disable") {
                *bypass_forbidden = true;
            }
        }
        *merged = merge_json(merged.clone(), layer);
        sources.push(label.to_string());
    }
}

fn load_file_layer_bypass(
    path: &Path,
    label: &str,
    merged: &mut Value,
    sources: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    if let Some(mut layer) = read_json_file(path, label, warnings) {
        // For project and local layers, ignore permissions.defaultMode = "bypassPermissions" (C1.5)
        if let Some(Value::Object(permissions)) = layer.get_mut("permissions") {
            if let Some(Value::String(default_mode)) = permissions.get_mut("defaultMode") {
                if default_mode == "bypassPermissions" {
                    // Remove this setting so it doesn't override higher priority layers
                    permissions.remove("defaultMode");
                }
            }
        }
        *merged = merge_json(merged.clone(), layer);
        sources.push(label.to_string());
    }
}

fn parse_inline_json(json_str: &str, label: &str, warnings: &mut Vec<String>) -> Option<Value> {
    match serde_json::from_str::<Value>(json_str.trim()) {
        Ok(v @ Value::Object(_)) => Some(sanitize_layer(v, label, warnings)),
        Ok(_) => {
            warnings.push(format!("{} settings must be a JSON object", label));
            None
        }
        Err(e) => {
            warnings.push(format!("Invalid inline {} JSON: {}", label, e));
            None
        }
    }
}

fn load_cli_layer(
    cli_settings: Option<&str>,
    cwd: &Path,
    merged: &mut Value,
    sources: &mut Vec<String>,
    warnings: &mut Vec<String>,
    bypass_forbidden: &mut bool,
) {
    let Some(cli_settings) = cli_settings else { return };
    let trimmed = cli_settings.trim();
    if trimmed.is_empty() {
        return;
    }

    // Check if it's a path that exists (relative to cwd or absolute)
    let path = PathBuf::from(trimmed);
    let candidate = if path.is_absolute() {
        path.clone()
    } else {
        cwd.join(&path)
    };

    if candidate.exists() && candidate.is_file() {
        load_file_layer(&candidate, "cli", merged, sources, warnings, bypass_forbidden);
    } else if raw_path_exists(&path) {
        load_file_layer(&path, "cli", merged, sources, warnings, bypass_forbidden);
    } else {
        // Treat as inline JSON
        if let Some(layer) = parse_inline_json(trimmed, "cli", warnings) {
            // Check if this layer disables bypass permissions
            if let Some(disable_bypass) = layer.get("disableBypassPermissionsMode") {
                if disable_bypass.as_str() == Some("disable") {
                    *bypass_forbidden = true;
                }
            }
            *merged = merge_json(merged.clone(), layer);
            sources.push("cli".to_string());
        }
    }
}

fn raw_path_exists(path: &Path) -> bool {
    path.exists() && path.is_file()
}

fn default_managed_path() -> PathBuf {
    PathBuf::from("/etc/zen/managed-settings.json")
}

fn load_managed_layer(
    merged: &mut Value,
    sources: &mut Vec<String>,
    warnings: &mut Vec<String>,
    custom_path: Option<&Path>,
    bypass_forbidden: &mut bool,
) {
    let path = custom_path.map(|p| p.to_path_buf()).unwrap_or_else(default_managed_path);
    load_file_layer(&path, "managed", merged, sources, warnings, bypass_forbidden);
}

/// Load settings from all layers with precedence:
/// user < project < local < cli < managed
/// `sources` filter = Some(["user","project","local"]) from --setting-sources; None = all.
/// `home` overridable for tests (None = dirs::home_dir()).
pub fn load_settings(
    cwd: &Path,
    home: Option<&Path>,
    cli_settings: Option<&str>,
    sources: Option<&[String]>,
) -> LoadedSettings {
    let home_dir = home.map(|p| p.to_path_buf()).or_else(dirs::home_dir);
    let managed_path = std::env::var_os("ZEN_MANAGED_SETTINGS").map(PathBuf::from);
    load_settings_impl(cwd, home_dir.as_deref(), cli_settings, sources, managed_path.as_deref())
}

/// Variant that accepts an explicit managed settings path (for tests).
pub fn load_settings_with_managed(
    cwd: &Path,
    home: Option<&Path>,
    cli_settings: Option<&str>,
    sources: Option<&[String]>,
    managed_path: Option<&Path>,
) -> LoadedSettings {
    load_settings_impl(cwd, home, cli_settings, sources, managed_path)
}

fn load_settings_impl(
    cwd: &Path,
    home: Option<&Path>,
    cli_settings: Option<&str>,
    sources: Option<&[String]>,
    managed_path: Option<&Path>,
) -> LoadedSettings {
    let mut merged = Value::Object(Map::new());
    let mut loaded_sources = Vec::new();
    let mut warnings = Vec::new();
    let mut bypass_forbidden = false;

    // 1. User ~/.zen/settings.json
    if is_enabled(sources, "user") {
        if let Some(home) = home {
            let path = home.join(".zen").join("settings.json");
            load_file_layer(&path, "user", &mut merged, &mut loaded_sources, &mut warnings, &mut bypass_forbidden);
        }
    }

    // 2. Project shared .zen/settings.json
    if is_enabled(sources, "project") {
        let path = cwd.join(".zen").join("settings.json");
        load_file_layer_bypass(&path, "project", &mut merged, &mut loaded_sources, &mut warnings);
    }

    // 3. Project local .zen/settings.local.json
    if is_enabled(sources, "local") {
        let path = cwd.join(".zen").join("settings.local.json");
        load_file_layer_bypass(&path, "local", &mut merged, &mut loaded_sources, &mut warnings);
    }

    // 4. CLI --settings <file-or-json>
    if is_enabled(sources, "cli") {
        load_cli_layer(cli_settings, cwd, &mut merged, &mut loaded_sources, &mut warnings, &mut bypass_forbidden);
    }

    // 5. Managed /etc/zen/managed-settings.json (highest file layer)
    if is_enabled(sources, "managed") {
        load_managed_layer(&mut merged, &mut loaded_sources, &mut warnings, managed_path, &mut bypass_forbidden);
    }

    // Deserialize into Settings (unknown keys land in extra via flatten)
    let settings = match serde_json::from_value::<Settings>(merged) {
        Ok(s) => s,
        Err(e) => {
            warnings.push(format!("Settings did not match schema: {}", e));
            Settings::default()
        }
    };

    LoadedSettings {
        settings,
        sources: loaded_sources,
        warnings,
        bypass_forbidden,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use crate::testutil::TempDir;

    #[test]
    fn test_merge_json_scalars() {
        let base = json!({"a": 1, "b": 2});
        let over = json!({"b": 3, "c": 4});
        let result = merge_json(base, over);
        assert_eq!(result, json!({"a": 1, "b": 3, "c": 4}));
    }

    #[test]
    fn test_merge_json_objects() {
        let base = json!({"a": {"x": 1, "y": 2}, "b": 5});
        let over = json!({"a": {"y": 3, "z": 4}, "c": 6});
        let result = merge_json(base, over);
        assert_eq!(result, json!({"a": {"x": 1, "y": 3, "z": 4}, "b": 5, "c": 6}));
    }

    #[test]
    fn test_merge_json_arrays_dedupe() {
        let base = json!({"arr": [1, 2, 3]});
        let over = json!({"arr": [3, 4, 5]});
        let result = merge_json(base, over);
        assert_eq!(result, json!({"arr": [1, 2, 3, 4, 5]}));
    }

    #[test]
    fn test_merge_json_null_handling() {
        assert_eq!(merge_json(Value::Null, json!(1)), json!(1));
        assert_eq!(merge_json(json!(1), Value::Null), json!(1));
        assert_eq!(merge_json(Value::Null, Value::Null), Value::Null);
    }

    #[test]
    fn test_load_settings_precedence_all_layers() {
        let tmp = TempDir::new();
        let home = tmp.path().join("home");
        let proj = tmp.path().join("proj");
        fs::create_dir_all(home.join(".zen")).unwrap();
        fs::create_dir_all(proj.join(".zen")).unwrap();

        // User settings
        fs::write(
            home.join(".zen/settings.json"),
            r#"{"model": "user-model", "fallbackModels": ["a", "b"], "env": {"A": "user", "B": "user"}, "permissions": {"allow": ["Read"]}, "maxTurns": 5}"#,
        ).unwrap();

        // Project settings
        fs::write(
            proj.join(".zen/settings.json"),
            r#"{"model": "project-model", "fallbackModels": ["b", "c"], "env": {"A": "project"}, "permissions": {"allow": ["Edit"], "defaultMode": "plan"}}"#,
        ).unwrap();

        // Local settings
        fs::write(
            proj.join(".zen/settings.local.json"),
            r#"{"baseUrl": "local-url", "maxTurns": 10}"#,
        ).unwrap();

        // CLI file
        let cli_file = tmp.path().join("cli.json");
        fs::write(
            &cli_file,
            r#"{"model": "cli-model", "permissions": {"allow": ["Write"]}}"#,
        ).unwrap();

        // Managed settings
        let managed_file = tmp.path().join("managed.json");
        fs::write(
            &managed_file,
            r#"{"model": "managed-model", "maxTurns": 20}"#,
        ).unwrap();

        let loaded = load_settings_with_managed(
            &proj,
            Some(&home),
            Some(cli_file.to_str().unwrap()),
            None,
            Some(&managed_file),
        );

        let s = &loaded.settings;
        assert_eq!(s.model, Some("managed-model".to_string())); // managed wins
        assert_eq!(s.base_url, Some("local-url".to_string())); // local wins over project/user
        assert_eq!(s.fallback_models, vec!["a", "b", "c"]); // concat + dedupe
        assert_eq!(s.env.get("A").map(|s| s.as_str()), Some("project")); // project overrides user
        assert_eq!(s.env.get("B").map(|s| s.as_str()), Some("user")); // user preserved
        assert_eq!(s.permissions.allow, vec!["Read", "Edit", "Write"]); // concat + dedupe
        assert_eq!(s.permissions.default_mode, Some("plan".to_string()));
        assert_eq!(s.max_turns, Some(20)); // managed wins
        assert_eq!(loaded.sources.len(), 5);
    }

    #[test]
    fn test_load_settings_sources_filter() {
        let tmp = TempDir::new();
        let home = tmp.path().join("home");
        let proj = tmp.path().join("proj");
        fs::create_dir_all(home.join(".zen")).unwrap();
        fs::create_dir_all(proj.join(".zen")).unwrap();

        fs::write(home.join(".zen/settings.json"), r#"{"model": "user-model"}"#).unwrap();
        fs::write(proj.join(".zen/settings.json"), r#"{"model": "project-model"}"#).unwrap();

        // Only project source enabled
        let loaded = load_settings(
            &proj,
            Some(&home),
            None,
            Some(&["project".to_string()]),
        );

        assert_eq!(loaded.settings.model, Some("project-model".to_string()));
        assert_eq!(loaded.sources, vec!["project"]);
    }

    #[test]
    fn test_load_settings_invalid_file_warning() {
        let tmp = TempDir::new();
        let home = tmp.path().join("home");
        let proj = tmp.path().join("proj");
        fs::create_dir_all(home.join(".zen")).unwrap();
        fs::create_dir_all(proj.join(".zen")).unwrap();

        fs::write(home.join(".zen/settings.json"), r#"{"model": "user-model"}"#).unwrap();
        fs::write(proj.join(".zen/settings.json"), "not valid json").unwrap();

        let loaded = load_settings(&proj, Some(&home), None, None);

        assert!(!loaded.warnings.is_empty());
        assert_eq!(loaded.settings.model, Some("user-model".to_string())); // user still loads
    }

    #[test]
    fn test_load_settings_cli_inline_json() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        fs::create_dir_all(proj.join(".zen")).unwrap();

        let loaded = load_settings(
            &proj,
            None,
            Some(r#"{"model": "inline-model"}"#),
            None,
        );

        assert_eq!(loaded.settings.model, Some("inline-model".to_string()));
        assert!(loaded.sources.contains(&"cli".to_string()));
    }

    #[test]
    fn test_load_settings_unknown_keys_in_extra() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        fs::create_dir_all(proj.join(".zen")).unwrap();

        fs::write(
            proj.join(".zen/settings.json"),
            r#"{"model": "test", "myCustomThing": {"x": 1}}"#,
        ).unwrap();

        let loaded = load_settings(&proj, None, None, None);
        assert_eq!(loaded.settings.extra.get("myCustomThing"), Some(&json!({"x": 1})));
    }

    #[test]
    fn test_hooks_merge_across_layers() {
        let tmp = TempDir::new();
        let home = tmp.path().join("home");
        let proj = tmp.path().join("proj");
        fs::create_dir_all(home.join(".zen")).unwrap();
        fs::create_dir_all(proj.join(".zen")).unwrap();

        fs::write(
            home.join(".zen/settings.json"),
            r#"{"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "echo user"}]}]}}"#,
        ).unwrap();
        fs::write(
            proj.join(".zen/settings.json"),
            r#"{"hooks": {"PreToolUse": [{"matcher": "Edit", "hooks": [{"type": "command", "command": "echo project"}]}]}}"#,
        ).unwrap();

        let loaded = load_settings(&proj, Some(&home), None, None);
        let arr = loaded.settings.hooks.get("PreToolUse").and_then(|v| v.as_array()).unwrap();
        // Arrays concatenate: both layers present.
        assert_eq!(arr.len(), 2);
    }

    #[test]
    fn c1_2_invalid_entries_skipped_rest_kept() {
        let tmp = TempDir::new();
        let home = tmp.path().join("home");
        let proj = tmp.path().join("proj");
        fs::create_dir_all(home.join(".zen")).unwrap();
        fs::create_dir_all(proj.join(".zen")).unwrap();
        fs::write(home.join(".zen/settings.json"), r#"{"model": "user-model", "maxTurns": 7}"#).unwrap();
        fs::write(
            proj.join(".zen/settings.json"),
            r#"{"model": 42, "maxTurns": "lots", "baseUrl": "http://p",
                "permissions": {"allow": ["Read", 5, "Edit"], "defaultMode": ["x"], "deny": ["Bash"]},
                "env": {"A": "1", "B": 2}, "fallbackModels": ["m1", null]}"#,
        )
        .unwrap();
        let loaded = load_settings(&proj, Some(&home), None, None);
        let s = &loaded.settings;
        assert_eq!(s.model.as_deref(), Some("user-model"), "{:?}", loaded.warnings); // bad project model skipped
        assert_eq!(s.max_turns, Some(7));
        assert_eq!(s.base_url.as_deref(), Some("http://p")); // valid sibling kept
        assert_eq!(s.permissions.allow, vec!["Read", "Edit"]);
        assert_eq!(s.permissions.deny, vec!["Bash"]);
        assert_eq!(s.permissions.default_mode, None);
        assert_eq!(s.env.get("A").map(String::as_str), Some("1"));
        assert!(!s.env.contains_key("B"));
        assert_eq!(s.fallback_models, vec!["m1"]);
        assert_eq!(loaded.sources, vec!["user", "project"]);
        assert!(loaded.warnings.len() >= 6, "{:?}", loaded.warnings);
    }

    #[test]
    fn c1_1_setting_sources_does_not_disable_cli_or_managed() {
        let tmp = TempDir::new();
        let home = tmp.path().join("home");
        let proj = tmp.path().join("proj");
        fs::create_dir_all(home.join(".zen")).unwrap();
        fs::create_dir_all(proj.join(".zen")).unwrap();
        fs::write(home.join(".zen/settings.json"), r#"{"model": "user-model", "maxTurns": 3}"#).unwrap();
        fs::write(proj.join(".zen/settings.json"), r#"{"model": "project-model"}"#).unwrap();
        let managed = tmp.path().join("managed.json");
        fs::write(&managed, r#"{"maxTurns": 9}"#).unwrap();
        let loaded = load_settings_with_managed(
            &proj,
            Some(&home),
            Some(r#"{"model": "cli-model"}"#),
            Some(&["user".to_string()]),
            Some(&managed),
        );
        assert_eq!(loaded.settings.model.as_deref(), Some("cli-model"));
        assert_eq!(loaded.settings.max_turns, Some(9));
        assert_eq!(loaded.sources, vec!["user", "cli", "managed"]);
    }
}
