use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub const MANAGED_MEMORY_PATH: &str = "/etc/zen/ZEN.md";

pub const MAX_IMPORT_DEPTH: usize = 4;

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryFile {
    pub path: PathBuf,
    pub content: String,
}

pub fn memory_files(cwd: &Path, home: Option<&Path>) -> Vec<(PathBuf, String)> {
    memory_files_with_managed(cwd, home, Some(Path::new(MANAGED_MEMORY_PATH)))
}

pub fn memory_files_with_managed(
    cwd: &Path,
    home: Option<&Path>,
    managed: Option<&Path>,
) -> Vec<(PathBuf, String)> {
    let home_dir = home.map(Path::to_path_buf).or_else(dirs::home_dir);
    let mut loader = Loader {
        home: home_dir.clone(),
        seen: HashSet::new(),
        out: Vec::new(),
    };

    if let Some(m) = managed {
        loader.push_file(m, false);
    }
    if let Some(home) = &home_dir {
        let user = home.join(".zen").join("ZEN.md");
        if user.is_file() {
            loader.push_file(&user, false);
        } else {
            loader.push_file(&home.join(".zen").join("AGENTS.md"), false);
        }
        for rule in rule_files(&home.join(".zen").join("rules")) {
            loader.push_file(&rule, true);
        }
    }

    let cwd_abs = abs_path(cwd);
    let dirs = ancestors_root_first(&cwd_abs);
    for dir in &dirs {
        let zen_files = [dir.join("ZEN.md"), dir.join(".zen").join("ZEN.md"), dir.join("ZEN.local.md")];
        if zen_files.iter().any(|f| f.is_file()) {
            for f in &zen_files {
                loader.push_file(f, false);
            }
        } else {

            loader.push_file(&dir.join("AGENTS.md"), false);
            loader.push_file(&dir.join(".zen").join("AGENTS.md"), false);
        }
    }
    for dir in &dirs {
        for rule in rule_files(&dir.join(".zen").join("rules")) {
            loader.push_file(&rule, true);
        }
    }
    loader.out
}

struct Loader {
    home: Option<PathBuf>,

    seen: HashSet<PathBuf>,
    out: Vec<(PathBuf, String)>,
}

impl Loader {
    fn push_file(&mut self, path: &Path, is_rule: bool) {
        let path = normalize(path);
        if self.seen.contains(&path) || !path.is_file() {
            return;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else { return };
        let body = if is_rule {
            match split_rule_frontmatter(&raw) {
                (true, _) => return,
                (false, body) => body.to_string(),
            }
        } else {
            raw
        };
        self.seen.insert(path.clone());
        let expanded = self.expand(&body, &path, 0);
        self.out.push((path, expanded));
    }

    fn expand(&mut self, content: &str, file: &Path, depth: usize) -> String {
        let base_dir = file.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
        let mut out = String::with_capacity(content.len());
        let mut fence: Option<(char, usize)> = None;
        let mut in_comment = false;

        for raw_line in content.lines() {

            if let Some((ch, len)) = fence {
                out.push_str(raw_line);
                out.push('\n');
                if closes_fence(raw_line, ch, len) {
                    fence = None;
                }
                continue;
            }

            let mut line = raw_line;
            if in_comment {
                match line.find("-->") {
                    Some(pos) => {
                        in_comment = false;
                        line = &line[pos + 3..];
                        if line.trim().is_empty() {
                            continue;
                        }
                    }
                    None => continue,
                }
            } else if let Some(after) = line.trim_start().strip_prefix("<!--") {
                match after.find("-->") {
                    Some(pos) => {
                        line = &after[pos + 3..];
                        if line.trim().is_empty() {
                            continue;
                        }
                    }
                    None => {
                        in_comment = true;
                        continue;
                    }
                }
            }

            if let Some(open) = opens_fence(line) {
                fence = Some(open);
                out.push_str(line);
                out.push('\n');
                continue;
            }

            let tokens = import_tokens(line);
            let mut imported: Vec<String> = Vec::new();
            for tok in &tokens {
                if let Some(text) = self.import(&base_dir, tok, depth) {
                    imported.push(text);
                }
            }

            let trimmed = line.trim();
            let only_import = imported.len() == 1
                && trimmed.starts_with('@')
                && token_span_len(trimmed) == trimmed.len();
            if !only_import {
                out.push_str(line);
                out.push('\n');
            }
            for text in imported {
                out.push_str(&text);
                if !out.ends_with('\n') {
                    out.push('\n');
                }
            }
        }

        if !content.is_empty() && !content.ends_with('\n') && out.ends_with('\n') {
            out.pop();
        }
        out
    }

    fn import(&mut self, base_dir: &Path, token: &str, depth: usize) -> Option<String> {
        if depth >= MAX_IMPORT_DEPTH {
            return None;
        }
        let path = if let Some(rest) = token.strip_prefix("~/") {
            self.home.as_ref()?.join(rest)
        } else if Path::new(token).is_absolute() {
            PathBuf::from(token)
        } else {
            base_dir.join(token)
        };
        let path = normalize(&path);
        if self.seen.contains(&path) || !path.is_file() {
            return None;
        }
        let raw = std::fs::read_to_string(&path).ok()?;
        self.seen.insert(path.clone());
        Some(self.expand(&raw, &path, depth + 1))
    }
}

fn token_span_len(s: &str) -> usize {
    let bytes: Vec<char> = s.chars().collect();
    let mut i = 1;
    let mut len = 1;
    while i < bytes.len() {
        let c = bytes[i];
        if c == '\\' && bytes.get(i + 1) == Some(&' ') {
            len += 2;
            i += 2;
            continue;
        }
        if c.is_whitespace() {
            break;
        }
        len += c.len_utf8();
        i += 1;
    }
    len
}

fn import_tokens(line: &str) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' {

            let start = i;
            while i < chars.len() && chars[i] == '`' {
                i += 1;
            }
            let n = i - start;
            let mut j = i;
            let mut closed = None;
            while j < chars.len() {
                if chars[j] == '`' {
                    let s = j;
                    while j < chars.len() && chars[j] == '`' {
                        j += 1;
                    }
                    if j - s == n {
                        closed = Some(j);
                        break;
                    }
                } else {
                    j += 1;
                }
            }
            if let Some(end) = closed {
                i = end;
            }
            continue;
        }
        if c == '@' && (i == 0 || chars[i - 1].is_whitespace()) {
            let mut tok = String::new();
            let mut j = i + 1;
            while j < chars.len() {
                if chars[j] == '\\' && chars.get(j + 1) == Some(&' ') {
                    tok.push(' ');
                    j += 2;
                    continue;
                }
                if chars[j].is_whitespace() {
                    break;
                }
                tok.push(chars[j]);
                j += 1;
            }
            if !tok.is_empty() && !tok.starts_with('@') {
                tokens.push(tok);
            }
            i = j;
            continue;
        }
        i += 1;
    }
    tokens
}

fn opens_fence(line: &str) -> Option<(char, usize)> {
    let t = line.trim_start();
    let ch = t.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let len = t.chars().take_while(|c| *c == ch).count();
    (len >= 3).then_some((ch, len))
}

fn closes_fence(line: &str, ch: char, len: usize) -> bool {
    let t = line.trim();
    let run = t.chars().take_while(|c| *c == ch).count();
    run >= len && run == t.chars().count()
}

fn split_rule_frontmatter(raw: &str) -> (bool, &str) {
    let Some(rest) = raw.strip_prefix("---\n").or_else(|| raw.strip_prefix("---\r\n")) else {
        return (false, raw);
    };
    let mut offset = 0;
    let mut has_paths = false;
    for line in rest.split_inclusive('\n') {
        offset += line.len();
        let t = line.trim();
        if t == "---" {
            return (has_paths, &rest[offset..]);
        }
        if t.starts_with("paths:") {
            has_paths = true;
        }
    }
    (false, raw)
}

fn rule_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|e| e.to_str()) == Some("md") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn abs_path(p: &Path) -> PathBuf {
    if p.is_absolute() {
        normalize(p)
    } else {
        let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        normalize(&base.join(p))
    }
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn ancestors_root_first(dir: &Path) -> Vec<PathBuf> {
    let mut chain: Vec<PathBuf> = dir.ancestors().map(|p| p.to_path_buf()).collect();
    chain.reverse();
    chain
}

pub fn append_memory_note(cwd: &Path, note: &str) -> anyhow::Result<PathBuf> {
    let path = cwd.join("ZEN.md");
    let note = note.trim();
    let mut existing = if path.exists() {
        std::fs::read_to_string(&path)?
    } else {
        String::new()
    };
    if !existing.is_empty() && !existing.ends_with('\n') {
        existing.push('\n');
    }
    existing.push_str("- ");
    existing.push_str(note);
    existing.push('\n');
    std::fs::write(&path, existing)?;
    Ok(path)
}

fn render_memory_section(path: &Path, content: &str) -> String {
    format!("# Memory: {}\n\n{}\n\n", path.display(), content.trim_end())
}

pub fn memory_prompt(cwd: &Path, home: Option<&Path>) -> String {
    let files = memory_files(cwd, home);
    if files.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for (path, content) in &files {
        if content.trim().is_empty() {
            continue;
        }
        out.push_str(&render_memory_section(path, content));
    }
    out.trim_end().to_string()
}

fn section_tokens(section: &str) -> u64 {
    section.len() as u64 / 4
}

fn display_path(path: &Path, home: Option<&Path>) -> String {
    let home_dir = home.map(Path::to_path_buf).or_else(dirs::home_dir);
    if let Some(h) = home_dir {
        if let Ok(rel) = path.strip_prefix(&h) {
            return format!("~/{}", rel.display());
        }
    }
    path.display().to_string()
}

pub fn memories_report(cwd: &Path, home: Option<&Path>) -> String {
    let files = memory_files(cwd, home);
    let mut sections: Vec<(PathBuf, String)> = Vec::new();
    for (path, content) in &files {
        if content.trim().is_empty() {
            continue;
        }
        sections.push((path.clone(), render_memory_section(path, content)));
    }
    if sections.is_empty() {
        return "(no memory files loaded)".to_string();
    }
    let mut total = 0u64;
    let mut out = String::new();
    for (path, section) in &sections {
        let toks = section_tokens(section);
        total += toks;
        out.push_str(&format!("{}  ~{} tokens\n", display_path(path, home), toks));
    }
    out.push_str(&format!(
        "total: {} file(s), ~{} tokens",
        sections.len(),
        total
    ));
    out
}

pub fn detect_import_candidates(cwd: &Path) -> Vec<PathBuf> {
    let cwd_abs = abs_path(cwd);
    let mut out = Vec::new();
    for dir in ancestors_root_first(&cwd_abs) {
        let claude = dir.join("CLAUDE.md");
        if claude.is_file() {
            out.push(normalize(&claude));
        }
        let has_zen = ["ZEN.md", ".zen/ZEN.md", "ZEN.local.md"]
            .iter()
            .any(|f| dir.join(f).is_file());
        if !has_zen {
            for name in ["AGENTS.md", ".zen/AGENTS.md"] {
                let p = dir.join(name);
                if p.is_file() {
                    out.push(normalize(&p));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use crate::testutil::TempDir;

    #[test]
    fn test_memory_order_root_to_cwd() {
        let tmp = TempDir::new();
        let home = tmp.path().join("home");
        let proj = tmp.path().join("proj");
        let sub = proj.join("sub");
        fs::create_dir_all(home.join(".zen")).unwrap();
        fs::create_dir_all(proj.join(".zen")).unwrap();
        fs::create_dir_all(&sub).unwrap();

        fs::write(home.join(".zen/ZEN.md"), "user memory").unwrap();
        fs::write(proj.join("ZEN.md"), "project memory").unwrap();
        fs::write(proj.join(".zen/ZEN.md"), "project dotzen memory").unwrap();
        fs::write(sub.join("ZEN.md"), "sub memory").unwrap();

        let files = memory_files(&sub, Some(&home));
        let names: Vec<String> = files.iter().map(|(p, _)| p.file_name().unwrap().to_string_lossy().to_string()).collect();

        assert_eq!(files[0].1.trim(), "user memory");
        let contents: Vec<&str> = files.iter().map(|(_, c)| c.trim()).collect();
        assert!(contents.contains(&"project memory"));
        assert!(contents.contains(&"project dotzen memory"));
        assert!(contents.contains(&"sub memory"));

        assert_eq!(files.last().unwrap().1.trim(), "sub memory");
        assert!(!names.is_empty());
    }

    #[test]
    fn test_memory_local_last() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("ZEN.md"), "shared").unwrap();
        fs::write(proj.join("ZEN.local.md"), "personal").unwrap();

        let files = memory_files(&proj, None);
        assert_eq!(files.last().unwrap().1.trim(), "personal");
    }

    #[test]
    fn test_memory_imports_relative() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        fs::create_dir_all(proj.join("docs")).unwrap();
        fs::write(proj.join("ZEN.md"), "top\n@docs/extra.md\nbottom\n").unwrap();
        fs::write(proj.join("docs/extra.md"), "imported content").unwrap();

        let files = memory_files(&proj, None);
        let top = files.iter().find(|(p, _)| p.ends_with("ZEN.md")).unwrap();
        assert!(top.1.contains("top"));
        assert!(top.1.contains("imported content"));
        assert!(top.1.contains("bottom"));
        assert!(!top.1.contains("@docs/extra.md"));
    }

    #[test]
    fn test_memory_import_missing_ignored() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("ZEN.md"), "before\n@does/not/exist.md\nafter\n").unwrap();

        let files = memory_files(&proj, None);
        let top = &files[0].1;
        assert!(top.contains("before"));
        assert!(top.contains("after"));
    }

    #[test]
    fn test_memory_import_cycle_safe() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("ZEN.md"), "root\n@b.md\n").unwrap();
        fs::write(proj.join("b.md"), "b\n@ZEN.md\n").unwrap();

        let files = memory_files(&proj, None);

        let joined = files.iter().map(|(_, c)| c.clone()).collect::<Vec<_>>().join("\n");
        assert!(joined.contains('b'));
    }

    #[test]
    fn test_memory_import_max_depth() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("ZEN.md"), "@l1.md\n").unwrap();
        fs::write(proj.join("l1.md"), "l1\n@l2.md\n").unwrap();
        fs::write(proj.join("l2.md"), "l2\n@l3.md\n").unwrap();
        fs::write(proj.join("l3.md"), "l3\n@l4.md\n").unwrap();
        fs::write(proj.join("l4.md"), "l4\n@l5.md\n").unwrap();
        fs::write(proj.join("l5.md"), "l5\n@l6.md\n").unwrap();
        fs::write(proj.join("l6.md"), "l6-deepest").unwrap();

        let files = memory_files(&proj, None);
        let joined = files.iter().map(|(_, c)| c.clone()).collect::<Vec<_>>().join("\n");

        assert!(joined.contains("l4\n"));
        assert!(!joined.contains("l5\n"));
        assert!(!joined.contains("l6-deepest"));
    }

    #[test]
    fn test_memory_prompt_headers() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("ZEN.md"), "hello memory").unwrap();

        let prompt = memory_prompt(&proj, None);
        assert!(prompt.contains("# Memory:"));
        assert!(prompt.contains("ZEN.md"));
        assert!(prompt.contains("hello memory"));
    }

    #[test]
    fn test_memory_prompt_empty() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        fs::create_dir_all(&proj).unwrap();

        let prompt = memory_prompt(&proj, Some(&tmp.path().join("nohome")));
        assert_eq!(prompt, "");
    }

    #[test]
    fn test_append_memory_note_creates() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        fs::create_dir_all(&proj).unwrap();
        let p = append_memory_note(&proj, "remember this").unwrap();
        assert!(p.exists());
        let content = fs::read_to_string(&p).unwrap();
        assert_eq!(content.trim(), "- remember this");
    }

    #[test]
    fn test_append_memory_note_appends() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("ZEN.md"), "- first").unwrap();
        append_memory_note(&proj, "second").unwrap();
        let content = fs::read_to_string(proj.join("ZEN.md")).unwrap();
        assert!(content.contains("- first"));
        assert!(content.contains("- second"));
    }

    fn mem(proj: &Path, home: &Path) -> Vec<(PathBuf, String)> {
        memory_files_with_managed(proj, Some(home), None)
    }

    #[test]
    fn c4_4_agents_md_fallback_per_directory() {
        let tmp = TempDir::new();
        let home = tmp.path().join("home");
        let proj = tmp.path().join("proj");
        let sub = proj.join("sub");
        fs::create_dir_all(home.join(".zen")).unwrap();
        fs::create_dir_all(proj.join(".zen")).unwrap();
        fs::create_dir_all(&sub).unwrap();

        fs::write(home.join(".zen/AGENTS.md"), "user-agents").unwrap();

        fs::write(proj.join("AGENTS.md"), "proj-agents").unwrap();
        fs::write(proj.join(".zen/AGENTS.md"), "proj-dot-agents").unwrap();

        fs::write(sub.join("ZEN.md"), "sub-zen").unwrap();
        fs::write(sub.join("AGENTS.md"), "sub-agents").unwrap();
        let got: Vec<String> = mem(&sub, &home).into_iter().map(|(_, c)| c).collect();
        assert_eq!(got, vec!["user-agents", "proj-agents", "proj-dot-agents", "sub-zen"]);
    }

    #[test]
    fn c4_4_user_zen_md_suppresses_user_agents_md() {
        let tmp = TempDir::new();
        let home = tmp.path().join("home");
        let proj = tmp.path().join("proj");
        fs::create_dir_all(home.join(".zen")).unwrap();
        fs::create_dir_all(&proj).unwrap();
        fs::write(home.join(".zen/ZEN.md"), "user-zen").unwrap();
        fs::write(home.join(".zen/AGENTS.md"), "user-agents").unwrap();

        fs::write(proj.join("ZEN.local.md"), "proj-local").unwrap();
        fs::write(proj.join("AGENTS.md"), "proj-agents").unwrap();
        let got: Vec<String> = mem(&proj, &home).into_iter().map(|(_, c)| c).collect();
        assert_eq!(got, vec!["user-zen", "proj-local"]);
    }

    #[test]
    fn c4_1_full_load_order() {
        let tmp = TempDir::new();
        let managed = tmp.path().join("etc/ZEN.md");
        let home = tmp.path().join("home");
        let proj = tmp.path().join("proj");
        let sub = proj.join("sub");
        fs::create_dir_all(managed.parent().unwrap()).unwrap();
        fs::create_dir_all(home.join(".zen/rules/nested")).unwrap();
        fs::create_dir_all(proj.join(".zen/rules")).unwrap();
        fs::create_dir_all(&sub).unwrap();
        fs::write(&managed, "managed").unwrap();
        fs::write(home.join(".zen/ZEN.md"), "user").unwrap();
        fs::write(home.join(".zen/rules/a.md"), "user-rule-a").unwrap();
        fs::write(home.join(".zen/rules/nested/b.md"), "user-rule-b").unwrap();
        fs::write(proj.join("ZEN.md"), "proj").unwrap();
        fs::write(proj.join(".zen/ZEN.md"), "proj-dot").unwrap();
        fs::write(proj.join("ZEN.local.md"), "proj-local").unwrap();
        fs::write(sub.join("ZEN.md"), "sub").unwrap();
        fs::write(proj.join(".zen/rules/style.md"), "---\ndescription: style\n---\nproj-rule").unwrap();
        fs::write(proj.join(".zen/rules/api.md"), "---\npaths:\n  - \"src/api/**\"\n---\nscoped-rule").unwrap();

        let files = memory_files_with_managed(&sub, Some(&home), Some(&managed));
        let contents: Vec<&str> = files.iter().map(|(_, c)| c.trim()).collect();
        assert_eq!(
            contents,
            vec!["managed", "user", "user-rule-a", "user-rule-b", "proj", "proj-dot", "proj-local", "sub", "proj-rule"]
        );
    }

    #[test]
    fn c4_1_home_inside_ancestors_not_duplicated() {
        let tmp = TempDir::new();
        let home = tmp.path().join("home");
        let proj = home.join("proj");
        fs::create_dir_all(home.join(".zen/rules")).unwrap();
        fs::create_dir_all(&proj).unwrap();
        fs::write(home.join(".zen/ZEN.md"), "user").unwrap();
        fs::write(home.join(".zen/rules/r.md"), "rule").unwrap();
        let files = mem(&proj, &home);
        let contents: Vec<&str> = files.iter().map(|(_, c)| c.trim()).collect();
        assert_eq!(contents, vec!["user", "rule"]);
    }

    #[test]
    fn c4_2_imports_absolute_home_and_escaped_space() {
        let tmp = TempDir::new();
        let home = tmp.path().join("home");
        let proj = tmp.path().join("proj");
        fs::create_dir_all(home.join("notes")).unwrap();
        fs::create_dir_all(proj.join("my docs")).unwrap();
        let abs = tmp.path().join("abs.md");
        fs::write(&abs, "ABS").unwrap();
        fs::write(home.join("notes/prefs.md"), "HOME-PREFS").unwrap();
        fs::write(proj.join("my docs/guide.md"), "GUIDE").unwrap();
        fs::write(
            proj.join("ZEN.md"),
            format!("@{}\n@~/notes/prefs.md\n@my\\ docs/guide.md\n", abs.display()),
        )
        .unwrap();
        let files = mem(&proj, &home);
        let top = &files.iter().find(|(p, _)| p.ends_with("proj/ZEN.md")).unwrap().1;
        assert_eq!(top, "ABS\nHOME-PREFS\nGUIDE\n");
    }

    #[test]
    fn c4_2_inline_import_keeps_text_and_appends_content() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("README.md"), "readme body").unwrap();
        fs::write(proj.join("ZEN.md"), "See @README.md for an overview. Mail me@example.com\n").unwrap();
        let files = mem(&proj, &tmp.path().join("nohome"));
        assert_eq!(
            files[0].1,
            "See @README.md for an overview. Mail me@example.com\nreadme body\n"
        );
    }

    #[test]
    fn c4_2_imports_in_code_are_ignored() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("x.md"), "SHOULD-NOT-APPEAR").unwrap();
        fs::write(
            proj.join("ZEN.md"),
            "Use `@x.md` literally\n```\n@x.md\n```\n~~~~\n@x.md\n~~~~\nend\n",
        )
        .unwrap();
        let files = mem(&proj, &tmp.path().join("nohome"));
        assert!(!files[0].1.contains("SHOULD-NOT-APPEAR"));
        assert!(files[0].1.contains("```\n@x.md\n```"));
        assert!(files[0].1.ends_with("end\n"));
    }

    #[test]
    fn c4_2_depth_counts_hops_per_top_level_file() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        fs::create_dir_all(proj.join("d")).unwrap();

        fs::write(proj.join("ZEN.md"), "@d/a.md\n").unwrap();
        fs::write(proj.join("d/a.md"), "A\n@b.md\n").unwrap();
        fs::write(proj.join("d/b.md"), "B\n").unwrap();
        let files = mem(&proj, &tmp.path().join("nohome"));
        assert_eq!(files[0].1, "A\nB\n");
    }

    #[test]
    fn c4_2_unresolved_import_left_as_text() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("ZEN.md"), "@missing.md\nping @team\n").unwrap();
        let files = mem(&proj, &tmp.path().join("nohome"));
        assert_eq!(files[0].1, "@missing.md\nping @team\n");
    }

    #[test]
    fn c4_3_block_html_comments_stripped() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("secret.md"), "SECRET").unwrap();
        fs::write(
            proj.join("ZEN.md"),
            "a\n<!-- one line -->\n<!-- multi\n@secret.md\nline --> tail\nkeep <!-- inline --> text\n```html\n<!-- in code -->\n```\nz\n",
        )
        .unwrap();
        let files = mem(&proj, &tmp.path().join("nohome"));
        assert_eq!(
            files[0].1,
            "a\n tail\nkeep <!-- inline --> text\n```html\n<!-- in code -->\n```\nz\n"
        );
    }

    #[test]
    fn c4_3_comments_stripped_in_imported_files() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("inc.md"), "<!-- hidden -->\nvisible\n").unwrap();
        fs::write(proj.join("ZEN.md"), "@inc.md\n").unwrap();
        let files = mem(&proj, &tmp.path().join("nohome"));
        assert_eq!(files[0].1, "visible\n");
    }

    #[test]
    fn memories_report_lists_files_with_token_counts() {
        let tmp = TempDir::new();
        let home = tmp.path().join("home");
        let proj = tmp.path().join("proj");
        fs::create_dir_all(home.join(".zen")).unwrap();
        fs::create_dir_all(&proj).unwrap();
        fs::write(home.join(".zen/ZEN.md"), "user rules here").unwrap();
        fs::write(proj.join("ZEN.md"), "project rules here").unwrap();

        let report = memories_report(&proj, Some(&home));
        let lines: Vec<&str> = report.lines().collect();

        assert_eq!(lines.len(), 3, "report:\n{}", report);
        assert!(lines[0].contains('~') && lines[0].contains("tokens"), "line0: {}", lines[0]);
        assert!(lines[1].contains('~') && lines[1].contains("tokens"), "line1: {}", lines[1]);
        assert!(lines[2].starts_with("total: 2 file(s), ~"), "total: {}", lines[2]);

        let parse = |l: &str| -> u64 {
            l.rsplit('~').next().unwrap().split_whitespace().next().unwrap().parse().unwrap()
        };
        let sum: u64 = lines[..2].iter().map(|l| parse(l)).sum();
        let total: u64 = parse(lines[2]);
        assert_eq!(sum, total);

        assert!(lines[0].contains(".zen/ZEN.md"), "order: {}", lines[0]);
    }

    #[test]
    fn memories_report_empty() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        fs::create_dir_all(&proj).unwrap();
        let report = memories_report(&proj, Some(&tmp.path().join("nohome")));
        assert_eq!(report, "(no memory files loaded)");
    }

    #[test]
    fn detect_import_candidates_finds_claude_and_agents_root_first() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        let sub = proj.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(proj.join("CLAUDE.md"), "claude rules").unwrap();
        fs::write(sub.join("AGENTS.md"), "agents rules").unwrap();

        let cands = detect_import_candidates(&sub);
        assert_eq!(cands.len(), 2, "candidates: {:?}", cands);

        assert!(cands[0].ends_with("proj/CLAUDE.md"), "first: {:?}", cands[0]);
        assert!(cands[1].ends_with("sub/AGENTS.md"), "second: {:?}", cands[1]);
    }

    #[test]
    fn detect_import_candidates_skips_agents_md_where_zen_exists() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("ZEN.md"), "zen rules").unwrap();
        fs::write(proj.join("AGENTS.md"), "agents rules").unwrap();
        fs::write(proj.join("CLAUDE.md"), "claude rules").unwrap();

        let cands = detect_import_candidates(&proj);

        assert_eq!(cands.len(), 1, "candidates: {:?}", cands);
        assert!(cands[0].ends_with("CLAUDE.md"));
    }

    #[test]
    fn detect_import_candidates_none() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        fs::create_dir_all(&proj).unwrap();
        assert!(detect_import_candidates(&proj).is_empty());
    }
}
