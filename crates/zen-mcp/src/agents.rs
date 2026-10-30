use serde_json::Value;
use std::path::{Path, PathBuf};
use zen_core::AgentDefinition;

pub(crate) fn split_frontmatter(text: &str) -> (Vec<(String, String)>, String) {
    let mut fields = Vec::new();
    let mut lines = text.lines();
    let first = lines.next();
    if first.map(|l| l.trim() != "---").unwrap_or(true) {
        return (fields, text.to_string());
    }
    let mut body_lines: Vec<&str> = Vec::new();
    let mut in_body = false;
    for line in lines {
        if !in_body {
            if line.trim() == "---" {
                in_body = true;
                continue;
            }

            if let Some(item) = line.trim_start().strip_prefix("- ") {
                if let Some((_, val)) = fields.last_mut() {
                    let item = item.trim().trim_matches('"').trim_matches('\'');
                    if !val.is_empty() {
                        val.push_str(", ");
                    }
                    val.push_str(item);
                }
                continue;
            }
            if let Some((k, v)) = line.split_once(':') {
                let key = k.trim().to_string();
                let val = v.trim().trim_matches('"').trim_matches('\'').to_string();
                if !key.is_empty() {
                    fields.push((key, val));
                }
            }
        } else {
            body_lines.push(line);
        }
    }
    if !in_body {
        return (Vec::new(), text.to_string());
    }
    (fields, body_lines.join("\n"))
}

fn field<'a>(fields: &'a [(String, String)], key: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.as_str())
}

fn parse_tools(s: &str) -> Option<Vec<String>> {
    let trimmed = s.trim();
    let trimmed = trimmed
        .strip_prefix('[')
        .and_then(|t| t.strip_suffix(']'))
        .unwrap_or(trimmed);
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("all") {
        return None;
    }

    let mut out: Vec<String> = Vec::new();
    for part in trimmed.split([',', ' ']) {
        let p = part.trim().trim_matches('"').trim_matches('\'');
        if !p.is_empty() && !out.iter().any(|x| x == p) {
            out.push(p.to_string());
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn agent_from_markdown(name_hint: &str, text: &str) -> Option<AgentDefinition> {
    let (fields, body) = split_frontmatter(text);
    let name = field(&fields, "name")
        .filter(|s| !s.is_empty())
        .unwrap_or(name_hint)
        .to_string();

    if name.is_empty() {
        return None;
    }

    let description = field(&fields, "description")
        .map(str::trim)
        .filter(|d| !d.is_empty())?
        .to_string();
    let tools = field(&fields, "tools").and_then(parse_tools);
    let model = field(&fields, "model")
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("inherit"))
        .map(|s| s.to_string());
    let disallowed_tools = field(&fields, "disallowedTools")
        .or_else(|| field(&fields, "disallowed-tools"))
        .and_then(parse_tools)
        .unwrap_or_default();
    let permission_mode = field(&fields, "permissionMode")
        .or_else(|| field(&fields, "permission-mode"))
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let max_turns = field(&fields, "maxTurns")
        .or_else(|| field(&fields, "max-turns"))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0);
    let skills = field(&fields, "skills")
        .and_then(parse_tools)
        .unwrap_or_default();

    Some(AgentDefinition {
        name,
        description,
        tools,
        model,
        system_prompt: body.trim().to_string(),
        disallowed_tools,
        permission_mode,
        max_turns,
        skills,
    })
}

fn load_dir(into: &mut Vec<AgentDefinition>, dir: &Path) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "md").unwrap_or(false))
        .collect();
    found.sort();
    for path in found {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Some(def) = agent_from_markdown(&stem, &text) {
                into.retain(|d| d.name != def.name);
                into.push(def);
            } else {
                eprintln!(
                    "zen: skipping agent definition {}: missing description",
                    path.display()
                );
            }
        }
    }
}

fn agent_from_json(name: &str, value: &Value) -> Option<AgentDefinition> {
    let obj = value.as_object()?;

    let description = obj
        .get("description")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|d| !d.is_empty())?
        .to_string();
    let system_prompt = obj
        .get("prompt")
        .or_else(|| obj.get("system_prompt"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let list = |key: &str| -> Option<Vec<String>> {
        obj.get(key).and_then(|v| match v {
            Value::String(s) => parse_tools(s),
            Value::Array(arr) => {
                let list: Vec<String> = arr
                    .iter()
                    .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect();
                if list.is_empty() {
                    None
                } else {
                    Some(list)
                }
            }
            _ => None,
        })
    };
    let tools = list("tools");
    let model = obj
        .get("model")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("inherit"))
        .map(|s| s.to_string());
    let permission_mode = obj
        .get("permissionMode")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let max_turns = obj
        .get("maxTurns")
        .and_then(|v| v.as_u64())
        .filter(|n| *n > 0)
        .map(|n| n as usize);

    Some(AgentDefinition {
        name: name.to_string(),
        description,
        tools,
        model,
        system_prompt,
        disallowed_tools: list("disallowedTools").unwrap_or_default(),
        permission_mode,
        max_turns,
        skills: list("skills").unwrap_or_default(),
    })
}

fn load_agents_from_json(into: &mut Vec<AgentDefinition>, text: &str) -> anyhow::Result<()> {
    let value: Value =
        serde_json::from_str(text).map_err(|e| anyhow::anyhow!("invalid --agents JSON: {}", e))?;

    let map = match value.get("agents").and_then(|v| v.as_object()) {
        Some(obj) => obj.clone(),
        None => value
            .as_object()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("--agents JSON must be an object"))?,
    };
    for (name, cfg) in map {
        if let Some(def) = agent_from_json(&name, &cfg) {
            into.retain(|d| d.name != def.name);
            into.push(def);
        } else {
            eprintln!(
                "zen: skipping --agents entry '{}': missing description",
                name
            );
        }
    }
    Ok(())
}

pub(crate) fn builtin_general_purpose() -> AgentDefinition {
    AgentDefinition {
        name: "general-purpose".to_string(),
        description: "Use for a self-contained task that benefits from a fresh context: research, multi-file searches, or an independent chunk of work. Returns only its final answer.".to_string(),
        tools: None,
        model: None,
        system_prompt: "You are a sub-agent invoked by the main zen assistant. You have your own fresh context. Complete the task you are given using the tools available to you, then reply with a concise final answer containing everything the parent needs. Do not ask clarifying questions; make reasonable assumptions and state them.".to_string(),
        ..Default::default()
    }
}

const READ_ONLY_TOOLS: &[&str] = &["Read", "Glob", "Grep", "WebFetch", "WebSearch", "TodoWrite"];

pub fn builtin_agents() -> Vec<AgentDefinition> {
    let read_only = Some(
        READ_ONLY_TOOLS
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
    );
    vec![
        builtin_general_purpose(),
        AgentDefinition {
            name: "Explore".to_string(),
            description: "Read-only agent for quickly finding files, symbols and answers in the codebase. Cannot edit files or run commands.".to_string(),
            tools: read_only.clone(),
            system_prompt: "You are a read-only exploration sub-agent. Search and read the codebase to answer the question you were given. Never modify files. Reply with the relevant paths, line references and a short answer.".to_string(),
            permission_mode: Some("plan".to_string()),
            ..Default::default()
        },
        AgentDefinition {
            name: "Plan".to_string(),
            description: "Read-only research agent that gathers the context needed to write an implementation plan. Cannot edit files or run commands.".to_string(),
            tools: read_only,
            system_prompt: "You are a read-only planning sub-agent. Investigate the code relevant to the task and reply with the findings a plan needs: affected files, current behaviour, risks, and suggested steps. Never modify files.".to_string(),
            permission_mode: Some("plan".to_string()),
            ..Default::default()
        },
    ]
}

fn home_dir(home: Option<&Path>) -> Option<PathBuf> {
    match home {
        Some(h) => Some(h.to_path_buf()),
        None => dirs::home_dir(),
    }
}

pub fn load_agents(
    cwd: &Path,
    home: Option<&Path>,
    cli_agents: Option<&str>,
) -> Vec<AgentDefinition> {
    let mut out: Vec<AgentDefinition> = Vec::new();

    if let Some(h) = home_dir(home) {
        load_dir(&mut out, &h.join(".zen").join("agents"));
    }
    load_dir(&mut out, &cwd.join(".zen").join("agents"));

    add_cli_agents(&mut out, cli_agents);
    out
}

pub fn load_cli_agents(cli_agents: Option<&str>) -> Vec<AgentDefinition> {
    let mut out = Vec::new();
    add_cli_agents(&mut out, cli_agents);
    out
}

fn add_cli_agents(out: &mut Vec<AgentDefinition>, cli_agents: Option<&str>) {
    if let Some(raw) = cli_agents {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            let json_text = if trimmed.starts_with('{') {
                Some(trimmed.to_string())
            } else {
                std::fs::read_to_string(PathBuf::from(trimmed)).ok()
            };
            if let Some(text) = json_text {
                if let Err(e) = load_agents_from_json(out, &text) {
                    eprintln!("zen: {}", e);
                }
            }
        }
    }

    let mut builtins: Vec<AgentDefinition> = builtin_agents()
        .into_iter()
        .filter(|b| !out.iter().any(|d| d.name == b.name))
        .collect();
    builtins.append(out);
    *out = builtins;
}

#[cfg(test)]
mod c6_tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("zen-c6-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    #[test]
    fn c6_1_frontmatter_fields_and_required_description() {
        let cwd = tmp("fm");
        let home = cwd.join("home");
        write(
            &cwd.join(".zen/agents/reviewer.md"),
            "---\nname: reviewer\ndescription: Reviews code\ntools:\n  - Read\n  - Grep\ndisallowedTools: Bash, Write\nmodel: inherit\npermissionMode: plan\nmaxTurns: 7\nskills: [style-guide, \"lint\"]\n---\nReview carefully.\n",
        );
        write(
            &cwd.join(".zen/agents/nodesc.md"),
            "---\nname: nodesc\ntools: Read\n---\nbody\n",
        );
        write(
            &cwd.join(".zen/agents/blankdesc.md"),
            "---\nname: blankdesc\ndescription:   \n---\nbody\n",
        );
        let agents = load_agents(
            &cwd,
            Some(&home),
            Some(
                r#"{"j":{"prompt":"no description"},"k":{"description":"K","disallowedTools":["Bash"],"permissionMode":"acceptEdits","maxTurns":3,"skills":"a, b"}}"#,
            ),
        );
        let names: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
        assert!(
            !names.contains(&"nodesc") && !names.contains(&"blankdesc") && !names.contains(&"j"),
            "{:?}",
            names
        );
        let r = agents.iter().find(|a| a.name == "reviewer").unwrap();
        assert_eq!(r.description, "Reviews code");
        assert_eq!(r.tools, Some(vec!["Read".to_string(), "Grep".to_string()]));
        assert_eq!(r.disallowed_tools, vec!["Bash", "Write"]);
        assert_eq!(r.model, None, "inherit → parent model");
        assert_eq!(r.permission_mode.as_deref(), Some("plan"));
        assert_eq!(r.max_turns, Some(7));
        assert_eq!(r.skills, vec!["style-guide", "lint"]);
        assert_eq!(r.system_prompt, "Review carefully.");
        let k = agents.iter().find(|a| a.name == "k").unwrap();
        assert_eq!(k.disallowed_tools, vec!["Bash"]);
        assert_eq!(k.permission_mode.as_deref(), Some("acceptEdits"));
        assert_eq!(k.max_turns, Some(3));
        assert_eq!(k.skills, vec!["a", "b"]);
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn c6_2_scope_priority_cli_project_user_builtin() {
        let cwd = tmp("prio");
        let home = cwd.join("home");
        for (dir, who) in [
            (home.join(".zen/agents"), "user"),
            (cwd.join(".zen/agents"), "project"),
        ] {
            write(
                &dir.join("a.md"),
                &format!("---\nname: a\ndescription: {who}\n---\n"),
            );
            write(
                &dir.join("Explore.md"),
                &format!("---\nname: Explore\ndescription: {who} explore\n---\n"),
            );
        }
        write(
            &home.join(".zen/agents/u.md"),
            "---\nname: u\ndescription: user only\n---\n",
        );
        let get = |agents: &[AgentDefinition], n: &str| {
            agents
                .iter()
                .find(|a| a.name == n)
                .map(|a| a.description.clone())
        };

        let agents = load_agents(&cwd, Some(&home), None);
        assert_eq!(
            get(&agents, "a").as_deref(),
            Some("project"),
            "project > user"
        );
        assert_eq!(get(&agents, "u").as_deref(), Some("user only"));
        assert_eq!(
            get(&agents, "Explore").as_deref(),
            Some("project explore"),
            "files > built-in"
        );
        assert_eq!(agents.iter().filter(|a| a.name == "Explore").count(), 1);
        let plan = agents.iter().find(|a| a.name == "Plan").unwrap();
        assert_eq!(plan.permission_mode.as_deref(), Some("plan"));
        let plan_tools = plan.tools.clone().unwrap();
        assert!(plan_tools.contains(&"Read".to_string()));
        assert!(
            !plan_tools
                .iter()
                .any(|t| t == "Write" || t == "Edit" || t == "Bash" || t == "Task"),
            "built-in Plan is read-only"
        );
        assert!(agents
            .iter()
            .any(|a| a.name == "general-purpose" && a.tools.is_none()));

        let agents = load_agents(
            &cwd,
            Some(&home),
            Some(r#"{"a":{"description":"cli"},"general-purpose":{"description":"mine"}}"#),
        );
        assert_eq!(
            get(&agents, "a").as_deref(),
            Some("cli"),
            "--agents > project"
        );
        assert_eq!(get(&agents, "general-purpose").as_deref(), Some("mine"));
        assert_eq!(
            agents
                .iter()
                .filter(|a| a.name == "general-purpose")
                .count(),
            1
        );

        let bare = load_cli_agents(None);
        let names: Vec<&str> = bare.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["general-purpose", "Explore", "Plan"]);
        let _ = std::fs::remove_dir_all(&cwd);
    }
}
