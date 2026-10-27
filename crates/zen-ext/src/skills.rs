use std::path::{Path, PathBuf};
use zen_core::SkillInfo;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Frontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
    pub allowed_tools: Vec<String>,
    pub disable_model_invocation: bool,
    pub extra: std::collections::HashMap<String, String>,
}

pub fn split_frontmatter(content: &str) -> (Option<Frontmatter>, String) {
    let mut lines = content.lines();
    let first = lines.next();
    if first.map(|l| l.trim()) != Some("---") {
        return (None, content.to_string());
    }
    let mut fm_lines: Vec<String> = Vec::new();
    let mut body_lines: Vec<String> = Vec::new();
    let mut in_fm = true;
    for line in lines {
        if in_fm {
            if line.trim() == "---" || line.trim() == "..." {
                in_fm = false;
                continue;
            }
            fm_lines.push(line.to_string());
        } else {
            body_lines.push(line.to_string());
        }
    }
    if in_fm {

        return (None, content.to_string());
    }
    let mut body = body_lines.join("\n");
    if content.ends_with('\n') && !body.is_empty() {
        body.push('\n');
    }
    (Some(parse_frontmatter(&fm_lines)), body)
}

fn parse_frontmatter(lines: &[String]) -> Frontmatter {
    let mut fm = Frontmatter::default();
    let mut current_key: Option<String> = None;
    let mut list_items: Vec<String> = Vec::new();

    let flush = |fm: &mut Frontmatter, key: &Option<String>, items: &mut Vec<String>| {
        if let Some(k) = key {
            if !items.is_empty() {
                let joined = items.join(", ");
                fm.extra.insert(k.clone(), joined);
            }
            items.clear();
        }
    };

    for raw in lines {
        let line = raw.trim_end();
        if line.trim().is_empty() {
            continue;
        }
        let is_item = line.trim_start().starts_with("- ");
        if is_item {
            if let Some(item) = line.trim_start().strip_prefix("- ") {
                list_items.push(unquote(item.trim()));
            }
            continue;
        }
        if let Some((k, v)) = split_key_value(line) {
            flush(&mut fm, &current_key, &mut list_items);
            let key = k.trim().to_ascii_lowercase();
            let value = v.trim().to_string();
            match key.as_str() {
                "name" => fm.name = Some(unquote(&value)),
                "description" => fm.description = Some(unquote(&value)),
                "allowed-tools" | "allowed_tools" | "tools" => {
                    fm.allowed_tools = parse_tool_list(&value);
                }
                "disable-model-invocation" | "disable_model_invocation" => {
                    fm.disable_model_invocation = parse_bool(&value);
                }
                _ => {
                    fm.extra.insert(key.clone(), unquote(&value));
                }
            }
            current_key = Some(key);
        }
    }
    flush(&mut fm, &current_key, &mut list_items);
    fm
}

fn split_key_value(line: &str) -> Option<(&str, &str)> {
    let idx = line.find(':')?;
    let key = &line[..idx];
    if key.trim().is_empty() || key.contains(['[', '{']) {
        return None;
    }
    Some((key, &line[idx + 1..]))
}

fn parse_tool_list(value: &str) -> Vec<String> {
    let v = value.trim();
    let v = v.strip_prefix('[').unwrap_or(v);
    let v = v.strip_suffix(']').unwrap_or(v);
    v.split(',')
        .map(|s| unquote(s.trim()))
        .filter(|s| !s.is_empty())
        .collect()
}

fn parse_bool(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "true" | "yes" | "1" | "on")
}

fn unquote(s: &str) -> String {
    let t = s.trim();
    let t = t.strip_prefix('"').unwrap_or(t);
    let t = t.strip_suffix('"').unwrap_or(t);
    let t = t.strip_prefix('\'').unwrap_or(t);
    let t = t.strip_suffix('\'').unwrap_or(t);
    t.trim().to_string()
}

pub fn discover_skills(cwd: &Path, home: Option<&Path>) -> Vec<SkillInfo> {
    discover_skill_entries(cwd, home)
        .into_iter()
        .filter(|(_, fm)| !fm.disable_model_invocation)
        .map(|(info, _)| info)
        .collect()
}

pub fn discover_skill_entries(cwd: &Path, home: Option<&Path>) -> Vec<(SkillInfo, Frontmatter)> {
    let home_dir = home.map(|p| p.to_path_buf()).or_else(dirs::home_dir);
    let mut by_name: Vec<(SkillInfo, Frontmatter)> = Vec::new();

    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(home) = &home_dir {
        roots.push(home.join(".zen").join("skills"));
    }
    roots.push(cwd.join(".zen").join("skills"));

    for root in roots {
        for entry in scan_skill_dir(&root) {

            if let Some(existing) = by_name.iter_mut().find(|s| s.0.name == entry.0.name) {
                *existing = entry;
            } else {
                by_name.push(entry);
            }
        }
    }

    by_name.sort_by(|a, b| a.0.name.cmp(&b.0.name));
    by_name
}

fn scan_skill_dir(root: &Path) -> Vec<(SkillInfo, Frontmatter)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else { return out };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        if let Some(entry) = load_skill_entry(&dir) {
            out.push(entry);
        }
    }
    out
}

pub fn load_skill(dir: &Path) -> Option<SkillInfo> {
    load_skill_entry(dir).map(|(info, _)| info)
}

pub fn load_skill_entry(dir: &Path) -> Option<(SkillInfo, Frontmatter)> {
    let path = dir.join("SKILL.md");
    if !path.is_file() {
        return None;
    }
    let content = std::fs::read_to_string(&path).ok()?;
    let (fm, body) = split_frontmatter(&content);
    let fm = fm.unwrap_or_default();

    let dir_name = dir.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let name = fm.name.clone().filter(|s| !s.is_empty()).unwrap_or(dir_name);
    if name.is_empty() {
        return None;
    }

    let description = fm
        .description
        .clone()
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())?;

    let info = SkillInfo {
        name,
        description,
        path,
        body,
        allowed_tools: fm.allowed_tools.clone(),
    };
    Some((info, fm))
}

pub fn skills_prompt(skills: &[SkillInfo]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut out = String::from("Available skills (load with the Skill tool):\n");
    for s in skills {
        out.push_str(&format!("- {}: {}\n", s.name, s.description));
    }
    out.trim_end().to_string()
}

use async_trait::async_trait;
use serde_json::Value;
use zen_core::{Tool, ToolContext, ToolOutput};

pub struct SkillTool {
    pub skills: Vec<SkillInfo>,
}

impl SkillTool {
    pub fn new(skills: Vec<SkillInfo>) -> Self {
        Self { skills }
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "Skill"
    }
    fn description(&self) -> &str {
        "Load the full instructions of a named skill."
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({"type":"object","properties":{"name":{"type":"string"}},"required":["name"]})
    }
    fn is_read_only(&self) -> bool {
        true
    }
    async fn call(&self, args: Value, _ctx: &ToolContext) -> ToolOutput {
        let Some(name) = args.get("name").and_then(|v| v.as_str()) else {
            return ToolOutput::err("Skill tool requires a \"name\" argument");
        };
        match self.skills.iter().find(|s| s.name == name) {
            Some(skill) => {
                let dir = skill
                    .path
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                let body = skill.body.trim_end().replace("${ZEN_SKILL_DIR}", &dir);
                ToolOutput::ok(format!("{}\n\nSkill directory: {}", body, dir))
            }
            None => {
                let available: Vec<&str> = self.skills.iter().map(|s| s.name.as_str()).collect();
                ToolOutput::err(format!(
                    "Unknown skill \"{}\". Available: {}",
                    name,
                    if available.is_empty() { "(none)".to_string() } else { available.join(", ") }
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use crate::testutil::TempDir;

    fn write_skill(root: &Path, name: &str, frontmatter: &str, body: &str) {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), format!("---\n{}\n---\n{}", frontmatter, body)).unwrap();
    }

    #[test]
    fn test_discover_skills_basic() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        let skills = proj.join(".zen/skills");
        write_skill(&skills, "alpha", "name: alpha\ndescription: Alpha skill", "alpha body");
        write_skill(&skills, "beta", "name: beta\ndescription: Beta skill", "beta body");

        let found = discover_skills(&proj, Some(&tmp.path().join("nohome")));
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].name, "alpha");
        assert_eq!(found[0].description, "Alpha skill");
        assert_eq!(found[0].body.trim(), "alpha body");
    }

    #[test]
    fn test_skill_allowed_tools_parsed() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        let skills = proj.join(".zen/skills");
        write_skill(
            &skills,
            "withtools",
            "name: withtools\ndescription: Has tools\nallowed-tools: Read, Write, Bash",
            "body",
        );
        let found = discover_skills(&proj, Some(&tmp.path().join("nohome")));
        assert_eq!(found[0].allowed_tools, vec!["Read", "Write", "Bash"]);
    }

    #[test]
    fn test_skill_missing_description_skipped() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        let skills = proj.join(".zen/skills");
        write_skill(&skills, "nodesc", "name: nodesc", "body");
        write_skill(&skills, "good", "name: good\ndescription: fine", "body");
        let found = discover_skills(&proj, Some(&tmp.path().join("nohome")));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "good");
    }

    #[test]
    fn test_skill_project_overrides_user() {
        let tmp = TempDir::new();
        let home = tmp.path().join("home");
        let proj = tmp.path().join("proj");
        write_skill(&home.join(".zen/skills"), "dup", "name: dup\ndescription: from user", "user body");
        write_skill(&proj.join(".zen/skills"), "dup", "name: dup\ndescription: from project", "project body");

        let found = discover_skills(&proj, Some(&home));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].description, "from project");
    }

    #[test]
    fn test_skills_prompt() {
        let skills = vec![
            SkillInfo {
                name: "a".into(),
                description: "desc a".into(),
                path: PathBuf::from("/x/a/SKILL.md"),
                body: "body".into(),
                allowed_tools: vec![],
            },
        ];
        let p = skills_prompt(&skills);
        assert!(p.contains("a: desc a"));
        assert_eq!(skills_prompt(&[]), "");
    }

    #[tokio::test]
    async fn test_skill_tool_returns_body_and_dir() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        let skills = proj.join(".zen/skills");
        write_skill(&skills, "gamma", "name: gamma\ndescription: Gamma", "gamma instructions");

        let found = discover_skills(&proj, Some(&tmp.path().join("nohome")));
        let tool = SkillTool::new(found);
        let ctx = ToolContext::new(proj.clone(), "s1");
        let out = tool.call(serde_json::json!({"name": "gamma"}), &ctx).await;
        assert!(!out.is_error);
        assert!(out.content.contains("gamma instructions"));
        assert!(out.content.contains("Skill directory:"));
        assert!(out.content.contains("gamma"));
    }

    #[test]
    fn c5_3_disable_model_invocation_hidden_from_model_but_kept_as_entry() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        let skills = proj.join(".zen/skills");
        write_skill(
            &skills,
            "manual",
            "name: manual\ndescription: Manual only\ndisable-model-invocation: true\nuser-invocable: true\nargument-hint: [file]\nmodel: m1",
            "body",
        );
        write_skill(&skills, "auto", "name: auto\ndescription: Auto", "body");
        let home = tmp.path().join("nohome");
        let model_facing = discover_skills(&proj, Some(&home));
        assert_eq!(model_facing.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), vec!["auto"]);
        assert!(!skills_prompt(&model_facing).contains("manual"));
        let all = discover_skill_entries(&proj, Some(&home));
        let (info, fm) = all.iter().find(|(i, _)| i.name == "manual").unwrap();
        assert_eq!(info.description, "Manual only");
        assert!(fm.disable_model_invocation);
        assert_eq!(fm.extra.get("argument-hint").map(String::as_str), Some("[file]"));
        assert_eq!(fm.extra.get("model").map(String::as_str), Some("m1"));
    }

    #[tokio::test]
    async fn c5_3_skill_tool_substitutes_skill_dir() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        write_skill(&proj.join(".zen/skills"), "s", "name: s\ndescription: S", "run ${ZEN_SKILL_DIR}/x.sh");
        let tool = SkillTool::new(discover_skills(&proj, Some(&tmp.path().join("nohome"))));
        let ctx = ToolContext::new(proj.clone(), "s1");
        let out = tool.call(serde_json::json!({"name": "s"}), &ctx).await;
        let dir = proj.join(".zen/skills/s").display().to_string();
        let norm = |s: &str| s.replace('\\', "/");
        assert!(norm(&out.content).contains(&norm(&format!("run {}/x.sh", dir))), "{}", out.content);
    }

    #[tokio::test]
    async fn test_skill_tool_unknown() {
        let tmp = TempDir::new();
        let tool = SkillTool::new(vec![]);
        let ctx = ToolContext::new(tmp.path().to_path_buf(), "s1");
        let out = tool.call(serde_json::json!({"name": "nope"}), &ctx).await;
        assert!(out.is_error);
    }
}
