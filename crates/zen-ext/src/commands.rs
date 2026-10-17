//! Custom slash commands and user-invocable skills (SPEC C5.2–C5.4).
//!
//! Sources, lowest precedence first:
//! - `~/.zen/commands/**.md` (user), `<cwd>/.zen/commands/**.md` (project overrides user);
//!   a file in a subdirectory is named `dir:name` (`.zen/commands/git/commit.md` → `git:commit`);
//! - skills (`.zen/skills/<name>/SKILL.md`, see `skills.rs`): a skill and a command with the same
//!   `/name` resolve to the skill. Skills with `user-invocable: false` are not `/`-reachable.
//!
//! Frontmatter (all optional for commands): `description`, `argument-hint`, `allowed-tools`,
//! `model`, `disable-model-invocation`.
//!
//! Expansion of a body, in order:
//! 1. `${ZEN_SKILL_DIR}` (skills only), `${ZEN_PROJECT_DIR}`, `${ZEN_SESSION_ID}`;
//! 2. arguments: `$ARGUMENTS[N]` and its shorthand `$N` (0-based, shell-style quoting groups
//!    words), `$ARGUMENTS` (the raw argument string). When the body uses none of them and
//!    arguments were given, they are appended as `ARGUMENTS: <args>` so they are not lost;
//! 3. `@path` inlining (relative to the project dir, `~/` and absolute paths allowed; an
//!    unreadable path is left as written);
//! 4. `` !`cmd` `` pre-execution: run with `sh -c` in the project dir and replaced by its output,
//!    only when [`ExpandContext::allow_shell`] permits that command (else left as written plus a
//!    note).

use crate::skills::{discover_skill_entries, split_frontmatter, Frontmatter};
use std::path::{Path, PathBuf};

/// Where a `/name` came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlashSource {
    /// A `.zen/commands/**.md` file.
    Command,
    /// A skill's `SKILL.md`.
    Skill,
}

/// A user-invocable custom slash command or skill.
#[derive(Debug, Clone, PartialEq)]
pub struct SlashCommand {
    /// Name without the leading `/` (`git:commit`).
    pub name: String,
    pub description: Option<String>,
    pub argument_hint: Option<String>,
    pub allowed_tools: Vec<String>,
    pub model: Option<String>,
    pub disable_model_invocation: bool,
    /// Markdown body after the frontmatter.
    pub body: String,
    /// The `.md` file (for skills, the `SKILL.md`).
    pub path: PathBuf,
    pub source: SlashSource,
}

impl SlashCommand {
    fn from_parts(name: String, fm: Frontmatter, body: String, path: PathBuf, source: SlashSource) -> Self {
        let get = |k: &str| fm.extra.get(k).filter(|v| !v.is_empty()).cloned();
        Self {
            name,
            description: fm.description.clone().filter(|d| !d.is_empty()),
            argument_hint: get("argument-hint").or_else(|| get("argument_hint")),
            allowed_tools: fm.allowed_tools.clone(),
            model: get("model").filter(|m| m != "inherit"),
            disable_model_invocation: fm.disable_model_invocation,
            body,
            path,
            source,
        }
    }

    /// The skill directory (`${ZEN_SKILL_DIR}`) when this is a skill.
    pub fn skill_dir(&self) -> Option<PathBuf> {
        match self.source {
            SlashSource::Skill => self.path.parent().map(Path::to_path_buf),
            SlashSource::Command => None,
        }
    }
}

/// Context for [`expand_command`].
pub struct ExpandContext<'a> {
    /// `${ZEN_PROJECT_DIR}`; also the base for `@path` and the cwd of `` !`cmd` ``.
    pub project_dir: PathBuf,
    /// `${ZEN_SESSION_ID}`.
    pub session_id: String,
    /// Permission gate for `` !`cmd` ``; `None` runs nothing.
    pub allow_shell: Option<&'a (dyn Fn(&str) -> bool + Send + Sync)>,
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" | "on" => Some(true),
        "false" | "no" | "0" | "off" => Some(false),
        _ => None,
    }
}

fn collect_md(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if p.is_dir() {
            collect_md(root, &p, out);
        } else if p.extension().is_some_and(|x| x == "md") {
            let rel = p.strip_prefix(root).unwrap_or(&p).with_extension("");
            let name = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join(":");
            if !name.is_empty() {
                out.push((name, p));
            }
        }
    }
}

/// Custom commands only (no skills): user then project, project overriding user by name.
pub fn discover_custom_commands(cwd: &Path, home: Option<&Path>) -> Vec<SlashCommand> {
    let home_dir = home.map(Path::to_path_buf).or_else(dirs::home_dir);
    let mut roots = Vec::new();
    if let Some(h) = &home_dir {
        roots.push(h.join(".zen").join("commands"));
    }
    roots.push(cwd.join(".zen").join("commands"));
    let mut out: Vec<SlashCommand> = Vec::new();
    for root in roots {
        let mut files = Vec::new();
        collect_md(&root, &root, &mut files);
        for (name, path) in files {
            let Ok(raw) = std::fs::read_to_string(&path) else { continue };
            let (fm, body) = split_frontmatter(&raw);
            let cmd = SlashCommand::from_parts(name, fm.unwrap_or_default(), body, path, SlashSource::Command);
            out.retain(|c| c.name != cmd.name);
            out.push(cmd);
        }
    }
    out
}

/// Every `/`-invocable custom command and skill, sorted by name. On a name clash the skill wins;
/// skills with `user-invocable: false` are left out.
pub fn discover_slash_commands(cwd: &Path, home: Option<&Path>) -> Vec<SlashCommand> {
    let mut out = discover_custom_commands(cwd, home);
    for (info, fm) in discover_skill_entries(cwd, home) {
        let user_invocable = fm
            .extra
            .get("user-invocable")
            .or_else(|| fm.extra.get("user_invocable"))
            .and_then(|v| parse_bool(v))
            .unwrap_or(true);
        out.retain(|c| c.name != info.name);
        if !user_invocable {
            continue;
        }
        out.push(SlashCommand::from_parts(info.name, fm, info.body, info.path, SlashSource::Skill));
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Look up one `/name` (without the slash).
pub fn find_slash_command(cwd: &Path, home: Option<&Path>, name: &str) -> Option<SlashCommand> {
    discover_slash_commands(cwd, home).into_iter().find(|c| c.name == name)
}

/// Shell-style split: whitespace separates, `"…"` and `'…'` group, `\` escapes outside `'`.
pub fn split_args(args: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut has = false;
    let mut quote: Option<char> = None;
    let mut chars = args.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') | (None, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                    has = true;
                }
            }
            (Some(_), c) => cur.push(c),
            (None, '"') | (None, '\'') => {
                quote = Some(c);
                has = true;
            }
            (None, c) if c.is_whitespace() => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (None, c) => {
                cur.push(c);
                has = true;
            }
        }
    }
    if has {
        out.push(cur);
    }
    out
}

/// Single pass over `body` replacing `$ARGUMENTS[N]`, `$N` and `$ARGUMENTS`. Returns the new text
/// and whether any placeholder was present.
fn substitute_args(body: &str, args: &str) -> (String, bool) {
    let parts = split_args(args);
    let nth = |i: usize| parts.get(i).cloned().unwrap_or_default();
    let mut out = String::with_capacity(body.len());
    let mut used = false;
    let mut rest = body;
    while let Some(pos) = rest.find('$') {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 1..];
        if let Some(tail) = after.strip_prefix("ARGUMENTS") {
            if let Some(idx_part) = tail.strip_prefix('[') {
                if let Some(close) = idx_part.find(']') {
                    if let Ok(i) = idx_part[..close].trim().parse::<usize>() {
                        out.push_str(&nth(i));
                        used = true;
                        rest = &idx_part[close + 1..];
                        continue;
                    }
                }
            }
            out.push_str(args);
            used = true;
            rest = tail;
            continue;
        }
        let mut digits = after.chars();
        if let Some(d) = digits.next().filter(char::is_ascii_digit) {
            // `$10` is not a placeholder (only $0..$9).
            if !after[1..].starts_with(|c: char| c.is_ascii_digit()) {
                out.push_str(&nth(d as usize - '0' as usize));
                used = true;
                rest = &after[1..];
                continue;
            }
        }
        out.push('$');
        rest = after;
    }
    out.push_str(rest);
    (out, used)
}

fn resolve_ref(raw: &str, base: &Path) -> Option<PathBuf> {
    let p = if let Some(rest) = raw.strip_prefix("~/") {
        dirs::home_dir()?.join(rest)
    } else {
        let p = PathBuf::from(raw);
        if p.is_absolute() { p } else { base.join(p) }
    };
    p.is_file().then_some(p)
}

/// Replace `@path` tokens (at start or after whitespace) with the file's contents.
fn inline_files(text: &str, base: &Path) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_ws = true;
    let mut i = 0;
    while i < text.len() {
        let c = text[i..].chars().next().unwrap();
        if c == '@' && prev_ws {
            let end = text[i + 1..]
                .find(|ch: char| ch.is_whitespace())
                .map(|e| i + 1 + e)
                .unwrap_or(text.len());
            let token = &text[i + 1..end];
            // Trailing punctuation is not part of the path.
            let trimmed = token.trim_end_matches([',', '.', ';', ':', ')', '!', '?']);
            if !trimmed.is_empty() {
                if let Some(content) = resolve_ref(trimmed, base).and_then(|p| std::fs::read_to_string(p).ok()) {
                    out.push_str(content.trim_end_matches('\n'));
                    out.push_str(&token[trimmed.len()..]);
                    i = end;
                    prev_ws = false;
                    continue;
                }
            }
        }
        out.push(c);
        prev_ws = c.is_whitespace();
        i += c.len_utf8();
    }
    out
}

/// Run every `` !`cmd` `` (permission-gated) and splice its output in.
fn run_shell_blocks(text: &str, ctx: &ExpandContext<'_>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find("!`") {
        let after = &rest[pos + 2..];
        let Some(close) = after.find('`') else { break };
        out.push_str(&rest[..pos]);
        let cmd = &after[..close];
        let allowed = ctx.allow_shell.is_some_and(|f| f(cmd));
        if cmd.trim().is_empty() {
            out.push_str(&rest[pos..pos + 2 + close + 1]);
        } else if !allowed {
            out.push_str(&format!("!`{}` (not run: permission denied)", cmd));
        } else {
            match std::process::Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(&ctx.project_dir)
                .env("ZEN_PROJECT_DIR", &ctx.project_dir)
                .env("ZEN_SESSION_ID", &ctx.session_id)
                .stdin(std::process::Stdio::null())
                .output()
            {
                Ok(o) => {
                    let mut s = String::from_utf8_lossy(&o.stdout).to_string();
                    if !o.status.success() {
                        let err = String::from_utf8_lossy(&o.stderr);
                        if !err.trim().is_empty() {
                            if !s.is_empty() && !s.ends_with('\n') {
                                s.push('\n');
                            }
                            s.push_str(err.trim_end());
                        }
                        s.push_str(&format!("\n(exit {})", o.status.code().unwrap_or(-1)));
                    }
                    out.push_str(s.trim_end_matches('\n'));
                }
                Err(e) => out.push_str(&format!("(failed to run `{}`: {})", cmd, e)),
            }
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Expand a command or skill body with the user's argument string (see module docs).
pub fn expand_command(cmd: &SlashCommand, args: &str, ctx: &ExpandContext<'_>) -> String {
    let mut body = cmd.body.clone();
    if let Some(dir) = cmd.skill_dir() {
        body = body.replace("${ZEN_SKILL_DIR}", &dir.display().to_string());
    }
    body = body
        .replace("${ZEN_PROJECT_DIR}", &ctx.project_dir.display().to_string())
        .replace("${ZEN_SESSION_ID}", &ctx.session_id);
    let (mut body, used) = substitute_args(&body, args);
    if !used && !args.trim().is_empty() {
        let trimmed = body.trim_end().len();
        body.truncate(trimmed);
        body.push_str(&format!("\n\nARGUMENTS: {}", args.trim()));
    }
    let body = inline_files(&body, &ctx.project_dir);
    run_shell_blocks(&body, ctx)
}

/// If `prompt` is `/name args` for a known custom command or skill, return it with its expansion
/// (for `-p` and the REPL, SPEC C5.4). Built-ins are not handled here and return `None`.
pub fn expand_slash_prompt(
    prompt: &str,
    cwd: &Path,
    home: Option<&Path>,
    ctx: &ExpandContext<'_>,
) -> Option<(SlashCommand, String)> {
    let trimmed = prompt.trim_start();
    let rest = trimmed.strip_prefix('/')?;
    let mut it = rest.splitn(2, char::is_whitespace);
    let name = it.next().unwrap_or("");
    if name.is_empty() {
        return None;
    }
    let args = it.next().unwrap_or("").trim();
    let cmd = find_slash_command(cwd, home, name)?;
    let text = expand_command(&cmd, args, ctx);
    Some((cmd, text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::fs;

    fn ctx(dir: &Path) -> ExpandContext<'static> {
        ExpandContext { project_dir: dir.to_path_buf(), session_id: "sess-1".into(), allow_shell: None }
    }

    fn cmd(body: &str) -> SlashCommand {
        SlashCommand {
            name: "t".into(),
            description: None,
            argument_hint: None,
            allowed_tools: vec![],
            model: None,
            disable_model_invocation: false,
            body: body.into(),
            path: PathBuf::from("/x/t.md"),
            source: SlashSource::Command,
        }
    }

    fn write(p: &Path, s: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, s).unwrap();
    }

    #[test]
    fn c5_2_discovery_subdir_namespace_frontmatter_and_project_override() {
        let tmp = TempDir::new();
        let home = tmp.path().join("home");
        let proj = tmp.path().join("proj");
        write(&home.join(".zen/commands/hello.md"), "USER $ARGUMENTS");
        write(&home.join(".zen/commands/only-user.md"), "u");
        write(&proj.join(".zen/commands/hello.md"), "PROJECT $ARGUMENTS");
        write(
            &proj.join(".zen/commands/git/commit.md"),
            "---\ndescription: Commit\nargument-hint: [msg]\nallowed-tools: Bash(git *), Read\nmodel: m-x\ndisable-model-invocation: true\n---\nCommit: $0\n",
        );
        write(&proj.join(".zen/commands/notes.txt"), "ignored");
        let all = discover_slash_commands(&proj, Some(&home));
        let names: Vec<&str> = all.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["git:commit", "hello", "only-user"]);
        let hello = all.iter().find(|c| c.name == "hello").unwrap();
        assert!(hello.body.starts_with("PROJECT"));
        let gc = all.iter().find(|c| c.name == "git:commit").unwrap();
        assert_eq!(gc.description.as_deref(), Some("Commit"));
        assert_eq!(gc.argument_hint.as_deref(), Some("[msg]"));
        assert_eq!(gc.allowed_tools, vec!["Bash(git *)", "Read"]);
        assert_eq!(gc.model.as_deref(), Some("m-x"));
        assert!(gc.disable_model_invocation);
        assert_eq!(gc.body.trim(), "Commit: $0");
    }

    #[test]
    fn c5_2_argument_placeholders() {
        let t = TempDir::new();
        let c = ctx(t.path());
        let out = expand_command(&cmd("a=$0 b=$1 c=$ARGUMENTS[2] all=$ARGUMENTS miss=$5 ten=$10 cost=$x"), "one \"two words\" three", &c);
        assert_eq!(out, "a=one b=two words c=three all=one \"two words\" three miss= ten=$10 cost=$x");
        // Substituted text is not re-expanded.
        assert_eq!(expand_command(&cmd("[$0]"), "$1", &c), "[$1]");
        // No placeholder → args appended.
        assert_eq!(expand_command(&cmd("Review this.\n"), "src/a.rs", &c), "Review this.\n\nARGUMENTS: src/a.rs");
        assert_eq!(expand_command(&cmd("Plain"), "", &c), "Plain");
    }

    #[test]
    fn c5_2_env_vars_and_file_inlining() {
        let t = TempDir::new();
        write(&t.path().join("notes/a.txt"), "ALPHA\n");
        let c = ctx(t.path());
        let out = expand_command(&cmd("dir=${ZEN_PROJECT_DIR} id=${ZEN_SESSION_ID} see @notes/a.txt, mail a@notes/a.txt @missing.txt"), "", &c);
        assert_eq!(
            out,
            format!("dir={} id=sess-1 see ALPHA, mail a@notes/a.txt @missing.txt", t.path().display())
        );
        // @file inside arguments is inlined too.
        assert_eq!(expand_command(&cmd("Fix $ARGUMENTS"), "@notes/a.txt", &c), "Fix ALPHA");
    }

    #[test]
    fn c5_2_shell_pre_execution_is_permission_gated() {
        let t = TempDir::new();
        write(&t.path().join("f.txt"), "x");
        let allow_ls = |c: &str| c.starts_with("ls");
        let c = ExpandContext { project_dir: t.path().to_path_buf(), session_id: "s9".into(), allow_shell: Some(&allow_ls) };
        let out = expand_command(&cmd("Files: !`ls` sid: !`echo $ZEN_SESSION_ID` arg: $0"), "z", &c);
        assert!(out.starts_with("Files: f.txt sid: !`echo $ZEN_SESSION_ID` (not run: permission denied) arg: z"), "{}", out);
        let all = |_: &str| true;
        let c = ExpandContext { project_dir: t.path().to_path_buf(), session_id: "s9".into(), allow_shell: Some(&all) };
        assert_eq!(expand_command(&cmd("sid=!`echo $ZEN_SESSION_ID`"), "", &c), "sid=s9");
        let fail = expand_command(&cmd("!`echo oops >&2; exit 3`"), "", &c);
        assert!(fail.contains("oops") && fail.contains("(exit 3)"), "{}", fail);
        // Without a gate nothing runs.
        assert!(expand_command(&cmd("!`echo hi`"), "", &ctx(t.path())).contains("not run"));
    }

    #[test]
    fn c5_3_skill_wins_clash_user_invocable_false_hidden_and_skill_dir() {
        let tmp = TempDir::new();
        let home = tmp.path().join("home");
        let proj = tmp.path().join("proj");
        write(&proj.join(".zen/commands/deploy.md"), "COMMAND deploy");
        write(&proj.join(".zen/commands/bg.md"), "COMMAND bg");
        write(
            &proj.join(".zen/skills/deploy/SKILL.md"),
            "---\nname: deploy\ndescription: Deploy it\nargument-hint: [env]\n---\nSKILL deploy $0 from ${ZEN_SKILL_DIR}\n",
        );
        write(
            &proj.join(".zen/skills/bg/SKILL.md"),
            "---\nname: bg\ndescription: Background knowledge\nuser-invocable: false\n---\nBG\n",
        );
        let all = discover_slash_commands(&proj, Some(&home));
        let deploy = all.iter().find(|c| c.name == "deploy").unwrap();
        assert_eq!(deploy.source, SlashSource::Skill);
        assert_eq!(deploy.argument_hint.as_deref(), Some("[env]"));
        assert!(all.iter().all(|c| c.name != "bg"), "user-invocable:false skill hides /bg (and shadows the command)");
        let c = ctx(&proj);
        let (_, text) = expand_slash_prompt("/deploy prod", &proj, Some(&home), &c).unwrap();
        // Separator-independent (Windows renders the joined path with `\`).
        let want = format!("SKILL deploy prod from {}", proj.join(".zen/skills/deploy").display());
        assert_eq!(text.trim().replace('\\', "/"), want.replace('\\', "/"));
        // Hidden-from-model skills are still /-invocable.
        write(
            &proj.join(".zen/skills/manual/SKILL.md"),
            "---\nname: manual\ndescription: M\ndisable-model-invocation: true\n---\nMANUAL\n",
        );
        assert!(find_slash_command(&proj, Some(&home), "manual").is_some());
    }

    #[test]
    fn c5_4_expand_slash_prompt_only_for_known_names() {
        let tmp = TempDir::new();
        let proj = tmp.path().join("proj");
        let home = tmp.path().join("home");
        write(&proj.join(".zen/commands/greet.md"), "Hello $0 and $1!");
        let c = ctx(&proj);
        let (cmd, text) = expand_slash_prompt("  /greet Alice Bob", &proj, Some(&home), &c).unwrap();
        assert_eq!(cmd.name, "greet");
        assert_eq!(text, "Hello Alice and Bob!");
        assert!(expand_slash_prompt("/nope x", &proj, Some(&home), &c).is_none());
        assert!(expand_slash_prompt("greet x", &proj, Some(&home), &c).is_none());
        assert!(expand_slash_prompt("/", &proj, Some(&home), &c).is_none());
    }
}
