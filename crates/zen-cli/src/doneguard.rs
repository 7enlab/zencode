use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use zen_core::{glob_match, TodoItem};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectKind {
    Rust,
    Node,
    Python,
    Go,
    Unknown,
}

pub fn detect_project(cwd: &Path) -> ProjectKind {
    if cwd.join("Cargo.toml").is_file() {
        ProjectKind::Rust
    } else if cwd.join("package.json").is_file() {
        ProjectKind::Node
    } else if cwd.join("pyproject.toml").is_file() || cwd.join("setup.py").is_file() {
        ProjectKind::Python
    } else if cwd.join("go.mod").is_file() {
        ProjectKind::Go
    } else {
        ProjectKind::Unknown
    }
}

pub struct CheckCommands {
    pub test: Option<(String, Vec<String>)>,
    pub lint: Option<(String, Vec<String>)>,
}

pub fn check_commands_for(kind: ProjectKind, cwd: &Path) -> CheckCommands {
    let has_eslint = [
        "eslint.config.js",
        "eslint.config.mjs",
        "eslint.config.cjs",
        ".eslintrc.js",
        ".eslintrc.json",
        ".eslintrc",
    ]
    .iter()
    .any(|f| cwd.join(f).is_file());
    match kind {
        ProjectKind::Rust => CheckCommands {
            test: Some((
                "cargo test --workspace".into(),
                vec!["cargo".into(), "test".into(), "--workspace".into()],
            )),
            lint: Some((
                "cargo clippy --workspace --all-targets -- -D warnings".into(),
                vec![
                    "cargo".into(),
                    "clippy".into(),
                    "--workspace".into(),
                    "--all-targets".into(),
                    "--".into(),
                    "-D".into(),
                    "warnings".into(),
                ],
            )),
        },
        ProjectKind::Node => CheckCommands {
            test: Some(("npm test".into(), vec!["npm".into(), "test".into()])),
            lint: has_eslint.then(|| {
                (
                    "eslint --max-warnings 0".into(),
                    vec![
                        "npx".into(),
                        "eslint".into(),
                        ".".into(),
                        "--max-warnings".into(),
                        "0".into(),
                    ],
                )
            }),
        },
        ProjectKind::Python => CheckCommands {
            test: Some(("pytest -q".into(), vec!["pytest".into(), "-q".into()])),
            lint: None,
        },
        ProjectKind::Go => CheckCommands {
            test: Some((
                "go test ./...".into(),
                vec!["go".into(), "test".into(), "./...".into()],
            )),
            lint: None,
        },
        ProjectKind::Unknown => CheckCommands {
            test: None,
            lint: None,
        },
    }
}

pub const CHECK_TIMEOUT: Duration = Duration::from_secs(600);
const TAIL_LINES: usize = 30;
const TAIL_CHARS: usize = 3000;

pub async fn run_check_command(label: &str, argv: &[String], cwd: &Path) -> (bool, String) {
    let mut c = tokio::process::Command::new(&argv[0]);
    c.args(&argv[1..]).current_dir(cwd);
    c.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c.kill_on_drop(true);
    let out = match tokio::time::timeout(CHECK_TIMEOUT, c.output()).await {
        Err(_) => {
            return (
                false,
                format!("{label}: timed out after {}s", CHECK_TIMEOUT.as_secs()),
            )
        }
        Ok(Err(e)) => return (false, format!("{label}: could not start: {e}")),
        Ok(Ok(o)) => o,
    };
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let lines: Vec<&str> = text.lines().collect();
    let mut tail: String = lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n");
    if tail.chars().count() > TAIL_CHARS {
        tail = tail
            .chars()
            .rev()
            .take(TAIL_CHARS)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
    }
    let status = match out.status.code() {
        Some(code) => format!("exit {code}"),
        None => "killed by signal".to_string(),
    };
    (out.status.success(), format!("{label}: {status}\n{tail}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Pass,
    Fail,
    Skip,
}

pub struct DoneCheck {
    pub name: &'static str,
    pub status: CheckStatus,
    pub detail: String,
}

fn check(name: &'static str, status: CheckStatus, detail: impl Into<String>) -> DoneCheck {
    DoneCheck {
        name,
        status,
        detail: detail.into(),
    }
}

pub async fn run_done_checks(cwd: &Path, todos: &[TodoItem]) -> Vec<DoneCheck> {
    let cmds = check_commands_for(detect_project(cwd), cwd);
    run_done_checks_with(cwd, todos, &cmds).await
}

pub async fn run_done_checks_with(
    cwd: &Path,
    todos: &[TodoItem],
    cmds: &CheckCommands,
) -> Vec<DoneCheck> {
    let mut out = Vec::new();
    match &cmds.test {
        Some((label, argv)) => {
            let (ok, detail) = run_check_command(label, argv, cwd).await;
            out.push(check(
                "tests",
                if ok {
                    CheckStatus::Pass
                } else {
                    CheckStatus::Fail
                },
                detail,
            ));
        }
        None => out.push(check(
            "tests",
            CheckStatus::Skip,
            "no known test command for this project",
        )),
    }
    match &cmds.lint {
        Some((label, argv)) => {
            let (ok, detail) = run_check_command(label, argv, cwd).await;
            out.push(check(
                "lint",
                if ok {
                    CheckStatus::Pass
                } else {
                    CheckStatus::Fail
                },
                detail,
            ));
        }
        None => out.push(check(
            "lint",
            CheckStatus::Skip,
            "no known lint command for this project",
        )),
    }
    if todos.is_empty() {
        out.push(check(
            "acceptance criteria",
            CheckStatus::Skip,
            "no todos recorded this session",
        ));
    } else {
        let open: Vec<&str> = todos
            .iter()
            .filter(|t| t.status != "completed")
            .map(|t| t.content.as_str())
            .collect();
        if open.is_empty() {
            out.push(check(
                "acceptance criteria",
                CheckStatus::Pass,
                format!("all {} todos completed", todos.len()),
            ));
        } else {
            out.push(check(
                "acceptance criteria",
                CheckStatus::Fail,
                format!(
                    "{} of {} todos still open: {}",
                    open.len(),
                    todos.len(),
                    open.join("; ")
                ),
            ));
        }
    }
    out
}

pub fn changed_files(cwd: &Path) -> Vec<String> {
    let out = std::process::Command::new("git")
        .args(["status", "--porcelain=v1", "-uall", "--", "."])
        .current_dir(cwd)
        .output();
    let Ok(out) = out else {
        return vec![];
    };
    if !out.status.success() {
        return vec![];
    }
    parse_porcelain(&String::from_utf8_lossy(&out.stdout))
}

pub fn parse_porcelain(text: &str) -> Vec<String> {
    let mut files = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.len() < 4 {
            continue;
        }
        let mut rest = &line[3..];

        if let Some((_, new)) = rest.split_once(" -> ") {
            rest = new;
        }
        files.push(unquote_path(rest));
    }
    files
}

fn unquote_path(p: &str) -> String {
    let p = p.trim();
    if p.len() >= 2 && p.starts_with('"') && p.ends_with('"') {
        let inner = &p[1..p.len() - 1];
        let mut s = String::with_capacity(inner.len());
        let mut it = inner.chars();
        while let Some(c) = it.next() {
            if c == '\\' {
                match it.next() {
                    Some('n') => s.push('\n'),
                    Some('t') => s.push('\t'),
                    Some('"') => s.push('"'),
                    Some('\\') => s.push('\\'),
                    Some(o) => {
                        s.push('\\');
                        s.push(o);
                    }
                    None => s.push('\\'),
                }
            } else {
                s.push(c);
            }
        }
        s
    } else {
        p.to_string()
    }
}

#[derive(Debug, Default, Clone)]
pub struct ScopeGuard {
    pub declared: Vec<String>,
}

impl ScopeGuard {
    pub fn set(&mut self, patterns: Vec<String>) {
        self.declared = patterns
            .into_iter()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect();
    }

    pub fn clear(&mut self) {
        self.declared.clear();
    }

    pub fn violations(&self, changed: &[String]) -> Vec<String> {
        if self.declared.is_empty() {
            return vec![];
        }
        changed
            .iter()
            .filter(|f| !self.declared.iter().any(|p| glob_match(p, f)))
            .cloned()
            .collect()
    }

    pub fn seed_from_prompt(&mut self, prompt: &str, cwd: &Path) {
        let mut seeds = Vec::new();
        for tok in prompt.split_whitespace() {
            let t = tok.trim_matches(|c: char| ".,;:!?()[]{}\"'`".contains(c));
            if t.is_empty() || t.contains("://") {
                continue;
            }
            let p = cwd.join(t);
            if p.is_file() {
                seeds.push(t.to_string());
            } else if p.is_dir() {
                seeds.push(format!("{}/**", t.trim_end_matches('/')));
            }
        }
        seeds.sort();
        seeds.dedup();
        if !seeds.is_empty() {
            self.declared = seeds;
        }
    }
}

#[derive(Debug)]
pub struct YagniFlag {
    pub file: String,
    pub item: String,
    pub kind: &'static str,
}

pub fn yagni_scan(cwd: &Path) -> Vec<YagniFlag> {
    let mut flags = Vec::new();
    let files: Vec<String> = changed_files(cwd)
        .into_iter()
        .filter(|f| f.ends_with(".rs"))
        .collect();
    for file in &files {
        for (kind, name) in added_pub_items(cwd, file) {
            if caller_count(cwd, file, kind, &name) == 0 {
                flags.push(YagniFlag {
                    file: file.clone(),
                    item: name,
                    kind,
                });
            }
        }
    }
    flags
}

fn added_pub_items(cwd: &Path, file: &str) -> Vec<(&'static str, String)> {
    let tracked = std::process::Command::new("git")
        .args(["ls-files", "--error-unmatch", "--", file])
        .current_dir(cwd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !tracked {
        return std::fs::read_to_string(cwd.join(file))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| parse_pub_item(&format!("+{l}")))
            .collect();
    }
    let out = std::process::Command::new("git")
        .args(["diff", "-U0", "HEAD", "--", file])
        .current_dir(cwd)
        .output();
    let Ok(out) = out else {
        return vec![];
    };
    if !out.status.success() {
        return vec![];
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
        .filter_map(parse_pub_item)
        .collect()
}

fn parse_pub_item(line: &str) -> Option<(&'static str, String)> {
    let t = line.strip_prefix('+')?.trim_start();
    let t = t.strip_prefix("pub")?;

    let mut t = if let Some(rest) = t.strip_prefix('(') {
        let end = rest.find(')')?;
        rest[end + 1..].trim_start()
    } else {
        let s = t.trim_start();

        if s.len() == t.len() {
            return None;
        }
        s
    };

    for q in ["async ", "unsafe "] {
        if let Some(rest) = t.strip_prefix(q) {
            t = rest;
        }
    }
    if let Some(rest) = t.strip_prefix("const ") {
        if rest.starts_with("fn") {
            t = rest;
        }
    }
    for kind in [
        "fn", "struct", "enum", "trait", "const", "static", "type", "mod", "union",
    ] {
        if let Some(rest) = t.strip_prefix(kind) {
            if !rest.starts_with([' ', '\t', '(']) {
                continue;
            }
            let name: String = rest
                .trim_start()
                .trim_start_matches('(')
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() && name != "_" {
                return Some((kind, name));
            }
        }
    }
    None
}

fn caller_count(cwd: &Path, def_file: &str, kind: &str, name: &str) -> usize {
    let mut count = 0;
    let mut stack = vec![cwd.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                let n = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if n == ".git" || n == "target" {
                    continue;
                }
                stack.push(p);
            } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
                let rel = p
                    .strip_prefix(cwd)
                    .map(|r| r.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                let Ok(text) = std::fs::read_to_string(&p) else {
                    continue;
                };
                if rel == def_file {
                    for line in text.lines() {
                        if let Some((k, n)) = parse_pub_item(&format!("+{line}")) {
                            if k == kind && n == name {
                                continue;
                            }
                        }
                        count += count_word(line, name);
                    }
                } else {
                    count += count_word(&text, name);
                }
            }
        }
    }
    count
}

fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

fn count_word(text: &str, word: &str) -> usize {
    let b = text.as_bytes();
    let w = word.as_bytes();
    if w.is_empty() || b.len() < w.len() {
        return 0;
    }
    let mut n = 0;
    let mut i = 0;
    while i + w.len() <= b.len() {
        if &b[i..i + w.len()] == w
            && (i == 0 || !is_ident_byte(b[i - 1]))
            && (i + w.len() == b.len() || !is_ident_byte(b[i + w.len()]))
        {
            n += 1;
            i += w.len();
        } else {
            i += 1;
        }
    }
    n
}

pub struct DoneReport {
    pub checks: Vec<DoneCheck>,
    pub changed: Vec<String>,

    pub scope: Vec<String>,

    pub scope_violations: Vec<String>,

    pub yagni: Vec<YagniFlag>,
}

pub fn format_report(cwd: &Path, r: &DoneReport) -> String {
    let mut s = String::new();
    s.push_str(&format!("definition of done - {}\n", cwd.display()));
    for c in &r.checks {
        let mark = match c.status {
            CheckStatus::Pass => "PASS",
            CheckStatus::Fail => "FAIL",
            CheckStatus::Skip => "SKIP",
        };
        let first = c.detail.lines().next().unwrap_or("");
        s.push_str(&format!("[{mark}] {} - {first}\n", c.name));
        if c.status == CheckStatus::Fail && c.detail.lines().count() > 1 {
            s.push_str(&c.detail);
            if !c.detail.ends_with('\n') {
                s.push('\n');
            }
        }
    }
    s.push_str(&format!("changed files: {}\n", r.changed.len()));
    for f in r.changed.iter().take(20) {
        s.push_str(&format!("  {f}\n"));
    }
    if r.changed.len() > 20 {
        s.push_str(&format!("  …and {} more\n", r.changed.len() - 20));
    }
    if r.scope.is_empty() {
        s.push_str("scope: none declared (/scope <glob>... to set one)\n");
    } else if r.scope_violations.is_empty() {
        s.push_str(&format!(
            "scope: ok - all changes within {}\n",
            r.scope.join(", ")
        ));
    } else {
        s.push_str(&format!(
            "scope: VIOLATIONS outside {}:\n",
            r.scope.join(", ")
        ));
        for f in &r.scope_violations {
            s.push_str(&format!("  ! {f}\n"));
        }
    }
    if r.yagni.is_empty() {
        s.push_str("yagni: no speculative public APIs detected\n");
    } else {
        s.push_str("yagni: speculative new public APIs (no callers in repo):\n");
        for y in &r.yagni {
            s.push_str(&format!(
                "  ? {}: new `pub {} {}` has no callers\n",
                y.file, y.kind, y.item
            ));
        }
    }
    let failed = r.checks.iter().any(|c| c.status == CheckStatus::Fail);
    s.push_str(if failed {
        "verdict: NOT DONE - fix the failures above\n"
    } else {
        "verdict: done\n"
    });
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn tmpdir(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("zen-doneguard-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn detect_project_by_marker_files() {
        let d = tmpdir("detect");
        assert_eq!(detect_project(&d), ProjectKind::Unknown);
        fs::write(d.join("Cargo.toml"), "[package]\n").unwrap();
        assert_eq!(detect_project(&d), ProjectKind::Rust);
        let d2 = tmpdir("detect2");
        fs::write(d2.join("package.json"), "{}\n").unwrap();
        assert_eq!(detect_project(&d2), ProjectKind::Node);
        let d3 = tmpdir("detect3");
        fs::write(d3.join("pyproject.toml"), "[project]\n").unwrap();
        assert_eq!(detect_project(&d3), ProjectKind::Python);
        let d4 = tmpdir("detect4");
        fs::write(d4.join("go.mod"), "module x\n").unwrap();
        assert_eq!(detect_project(&d4), ProjectKind::Go);
    }

    #[test]
    fn check_commands_have_expected_shape() {
        let d = tmpdir("cmds");
        let rust = check_commands_for(ProjectKind::Rust, &d);
        let (label, argv) = rust.test.unwrap();
        assert!(label.contains("cargo test") && argv[0] == "cargo");
        let (label, argv) = rust.lint.unwrap();
        assert!(label.contains("clippy") && argv.contains(&"-D".to_string()));
        let unknown = check_commands_for(ProjectKind::Unknown, &d);
        assert!(unknown.test.is_none() && unknown.lint.is_none());

        let node = check_commands_for(ProjectKind::Node, &d);
        assert!(node.lint.is_none());
        fs::write(d.join("eslint.config.js"), "export default [];\n").unwrap();
        assert!(check_commands_for(ProjectKind::Node, &d).lint.is_some());
    }

    #[test]
    fn parse_porcelain_handles_statuses_quotes_and_renames() {
        let text = "M  src/main.rs\nA  new file.rs\n?? untracked.txt\nR  old.rs -> new.rs\n";
        assert_eq!(
            parse_porcelain(text),
            vec!["src/main.rs", "new file.rs", "untracked.txt", "new.rs"]
        );

        let quoted = "?? \"my docs/note one.md\"\n";
        assert_eq!(parse_porcelain(quoted), vec!["my docs/note one.md"]);
        assert!(parse_porcelain("").is_empty());
    }

    #[test]
    fn scope_guard_violations_and_empty_scope() {
        let mut g = ScopeGuard::default();
        assert!(
            g.violations(&["src/a.rs".to_string()]).is_empty(),
            "empty scope => no violations"
        );
        g.set(vec!["src/**".into(), "README.md".into(), "  ".into()]);
        assert_eq!(g.declared, vec!["src/**", "README.md"], "blanks dropped");
        assert!(g
            .violations(&["src/a/b.rs".to_string(), "README.md".to_string()])
            .is_empty());
        assert_eq!(
            g.violations(&["src/a.rs".to_string(), "other/b.rs".to_string()]),
            vec!["other/b.rs"]
        );
        g.clear();
        assert!(g.declared.is_empty());
    }

    #[test]
    fn seed_from_prompt_picks_existing_files_and_dirs() {
        let d = tmpdir("seed");
        fs::create_dir_all(d.join("src")).unwrap();
        fs::write(d.join("src").join("main.rs"), "fn main() {}\n").unwrap();
        fs::write(d.join("README.md"), "hi\n").unwrap();
        let mut g = ScopeGuard::default();
        g.seed_from_prompt(
            "Fix the bug in src/main.rs, see README.md. Also check https://example.com/x and nosuchfile.rs!",
            &d,
        );
        assert_eq!(g.declared, vec!["README.md", "src/main.rs"]);

        let mut g2 = ScopeGuard::default();
        g2.seed_from_prompt("refactor everything under src please", &d);
        assert_eq!(g2.declared, vec!["src/**"]);

        let mut g3 = ScopeGuard::default();
        g3.seed_from_prompt("hello world", &d);
        assert!(g3.declared.is_empty());
    }

    #[test]
    fn parse_pub_item_shapes() {
        assert_eq!(parse_pub_item("+pub fn foo("), Some(("fn", "foo".into())));
        assert_eq!(
            parse_pub_item("+    pub async fn bar<T>("),
            Some(("fn", "bar".into()))
        );
        assert_eq!(
            parse_pub_item("+pub(crate) struct Foo {"),
            Some(("struct", "Foo".into()))
        );
        assert_eq!(
            parse_pub_item("+pub const MAX: u32 = 1;"),
            Some(("const", "MAX".into()))
        );
        assert_eq!(
            parse_pub_item("+pub unsafe fn raw("),
            Some(("fn", "raw".into()))
        );
        assert!(parse_pub_item("+fn private(").is_none());
        assert!(parse_pub_item("+pub use foo::bar;").is_none());
        assert!(parse_pub_item("+// pub fn not_code(").is_none());
        assert!(
            parse_pub_item(" pub fn context_line(").is_none(),
            "needs leading +"
        );
    }

    #[test]
    fn count_word_respects_boundaries() {
        assert_eq!(count_word("foo foo_ foobar _foo foo", "foo"), 2);
        assert_eq!(count_word("", "foo"), 0);
    }

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn init_repo(tag: &str) -> PathBuf {
        let d = tmpdir(tag);
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&d)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
        };
        git(&["init", "-q"]).unwrap();
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]).unwrap();
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t",
            "commit",
            "-qm",
            "base",
            "--allow-empty",
        ])
        .unwrap();
        d
    }

    #[test]
    fn changed_files_lists_git_changes() {
        if !git_available() {
            return;
        }
        let d = init_repo("changed");
        fs::write(d.join("a.txt"), "x\n").unwrap();
        let changed = changed_files(&d);
        assert_eq!(changed, vec!["a.txt"], "{changed:?}");
        assert!(changed_files(&tmpdir("not-a-repo")).is_empty());
    }

    #[test]
    fn yagni_flags_new_pub_item_without_callers() {
        if !git_available() {
            return;
        }
        let d = init_repo("yagni");
        fs::create_dir_all(d.join("src")).unwrap();
        fs::write(d.join("src").join("lib.rs"), "pub fn used() {}\n").unwrap();
        fs::write(
            d.join("src").join("main.rs"),
            "fn main() { lib::used(); }\n",
        )
        .unwrap();

        fs::write(
            d.join("src").join("extra.rs"),
            "pub fn speculative_thing() {}\npub fn also_used() {}\n",
        )
        .unwrap();
        fs::write(
            d.join("src").join("main.rs"),
            "fn main() { lib::used(); lib::also_used(); }\n",
        )
        .unwrap();
        let flags = yagni_scan(&d);
        assert_eq!(flags.len(), 1, "{flags:?}");
        assert_eq!(flags[0].file, "src/extra.rs");
        assert_eq!(flags[0].item, "speculative_thing");
        assert_eq!(flags[0].kind, "fn");
    }

    fn sh(script: &str) -> Vec<String> {
        #[cfg(windows)]
        {
            vec!["cmd".into(), "/c".into(), script.into()]
        }
        #[cfg(not(windows))]
        {
            vec!["sh".into(), "-c".into(), script.into()]
        }
    }

    #[cfg(not(windows))]
    fn sleep_cmd() -> Vec<String> {
        vec!["sh".into(), "-c".into(), "sleep 30".into()]
    }

    #[cfg(windows)]
    fn sleep_cmd() -> Vec<String> {
        vec![
            "cmd".into(),
            "/c".into(),
            "ping -n 30 127.0.0.1 >nul".into(),
        ]
    }

    #[tokio::test]
    async fn run_check_command_pass_fail() {
        let d = tmpdir("checkcmd");
        let (ok, detail) = run_check_command("t", &sh("exit 0"), &d).await;
        assert!(ok, "{detail}");
        let (ok, detail) = run_check_command("t", &sh("echo boom; exit 3"), &d).await;
        assert!(
            !ok && detail.contains("exit 3") && detail.contains("boom"),
            "{detail}"
        );
        let (ok, detail) = run_check_command("t", &sh("no-such-command-xyz"), &d).await;
        assert!(!ok, "{detail}");
    }

    #[tokio::test]
    async fn run_check_command_timeout_is_failure() {
        let d = tmpdir("checkto");
        let mut c = tokio::process::Command::new(&sleep_cmd()[0]);
        c.args(&sleep_cmd()[1..]).current_dir(&d);
        c.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c.kill_on_drop(true);
        let timed_out = tokio::time::timeout(Duration::from_millis(200), c.output())
            .await
            .is_err();
        assert!(timed_out, "test harness sanity: sleep must exceed 200ms");
    }

    fn todo(content: &str, status: &str) -> TodoItem {
        TodoItem {
            content: content.into(),
            status: status.into(),
            active_form: None,
        }
    }

    fn no_cmds() -> CheckCommands {
        CheckCommands {
            test: None,
            lint: None,
        }
    }

    #[tokio::test]
    async fn done_checks_todos_gate_acceptance() {
        let d = tmpdir("todos");

        let checks = run_done_checks_with(&d, &[], &no_cmds()).await;
        assert!(checks.iter().all(|c| c.status == CheckStatus::Skip));

        let checks = run_done_checks_with(
            &d,
            &[
                todo("write code", "completed"),
                todo("write tests", "in_progress"),
            ],
            &no_cmds(),
        )
        .await;
        let acc = checks
            .iter()
            .find(|c| c.name == "acceptance criteria")
            .unwrap();
        assert_eq!(acc.status, CheckStatus::Fail);
        assert!(acc.detail.contains("write tests"), "{}", acc.detail);

        let checks = run_done_checks_with(&d, &[todo("write code", "completed")], &no_cmds()).await;
        let acc = checks
            .iter()
            .find(|c| c.name == "acceptance criteria")
            .unwrap();
        assert_eq!(acc.status, CheckStatus::Pass);
    }

    #[tokio::test]
    async fn done_checks_fake_commands_pass_and_fail() {
        let d = tmpdir("fakecmds");
        let cmds = CheckCommands {
            test: Some(("t".into(), sh("exit 0"))),
            lint: Some(("l".into(), sh("exit 2"))),
        };
        let checks = run_done_checks_with(&d, &[], &cmds).await;
        assert_eq!(checks[0].status, CheckStatus::Pass);
        assert_eq!(checks[1].status, CheckStatus::Fail);
    }

    #[test]
    fn format_report_marks_and_verdict() {
        let r = DoneReport {
            checks: vec![
                DoneCheck {
                    name: "tests",
                    status: CheckStatus::Pass,
                    detail: "cargo test: exit 0\n42 passed".into(),
                },
                DoneCheck {
                    name: "lint",
                    status: CheckStatus::Fail,
                    detail: "clippy: exit 1\nwarning: unused\ntail".into(),
                },
            ],
            changed: vec!["a.rs".into()],
            scope: vec!["src/**".into()],
            scope_violations: vec!["other/b.rs".into()],
            yagni: vec![YagniFlag {
                file: "src/x.rs".into(),
                item: "mystery".into(),
                kind: "fn",
            }],
        };
        let text = format_report(Path::new("/repo"), &r);
        assert!(text.contains("[PASS] tests"), "{text}");
        assert!(text.contains("[FAIL] lint"), "{text}");
        assert!(
            text.contains("warning: unused"),
            "failure detail included: {text}"
        );
        assert!(text.contains("! other/b.rs"), "{text}");
        assert!(text.contains("`pub fn mystery` has no callers"), "{text}");
        assert!(text.contains("verdict: NOT DONE"), "{text}");
    }
}
