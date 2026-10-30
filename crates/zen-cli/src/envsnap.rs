use std::path::Path;
use std::process::Command;

pub const MAX_CHARS: usize = 4000;
const MAX_TREE_LINES: usize = 40;
const MAX_CHILDREN: usize = 8;

const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    "__pycache__",
    "venv",
    "vendor",
    "coverage",
];

struct Kind {
    label: &'static str,
    commands: Vec<String>,
    probes: &'static [&'static [&'static str]],
}

pub fn project_snapshot(cwd: &Path) -> Option<String> {
    let tree = tree_lines(cwd);
    if tree.is_empty() {
        return None;
    }
    let kinds = detect_kinds(cwd);
    let mut s = String::from("Project snapshot (taken at session start):\n");
    s.push_str("Files (2 levels, build output and ignored dirs omitted):\n");
    for l in &tree {
        s.push_str(l);
        s.push('\n');
    }
    if kinds.is_empty() {
        s.push_str("Project type: not detected from the usual manifest files.\n");
    }
    for k in &kinds {
        s.push_str(&format!(
            "Project type: {}. Commands: {}\n",
            k.label,
            k.commands.join("; ")
        ));
    }
    let mut versions: Vec<String> = Vec::new();
    for k in &kinds {
        for probe in k.probes {
            if let Some(v) = tool_version(probe) {
                if !versions.contains(&v) {
                    versions.push(v);
                }
            }
        }
    }
    if !versions.is_empty() {
        s.push_str(&format!("Toolchain: {}\n", versions.join("; ")));
    }
    let tests = test_locations(cwd);
    if !tests.is_empty() {
        s.push_str(&format!("Tests live in: {}\n", tests.join(", ")));
    }
    if !kinds.is_empty() {
        s.push_str("Your work is judged by whether these checks pass: after changing code, run the relevant build/test command and read its output before you finish.\n");
    }
    if s.chars().count() > MAX_CHARS {
        s = s.chars().take(MAX_CHARS).collect();
        s.push_str("\n[snapshot truncated]");
    }
    Some(s.trim_end().to_string())
}

fn ignored_names(cwd: &Path) -> Vec<String> {
    std::fs::read_to_string(cwd.join(".gitignore"))
        .unwrap_or_default()
        .lines()
        .map(|l| {
            l.trim()
                .trim_start_matches('/')
                .trim_end_matches('/')
                .to_string()
        })
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with('#')
                && !l.starts_with('!')
                && !l.contains(['*', '?', '[', '/'])
        })
        .collect()
}

fn visible_entries(dir: &Path, ignored: &[String]) -> Vec<(String, bool)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut v: Vec<(String, bool)> = rd
        .filter_map(|e| e.ok())
        .map(|e| {
            (
                e.file_name().to_string_lossy().to_string(),
                e.path().is_dir(),
            )
        })
        .filter(|(name, is_dir)| {
            !name.starts_with('.')
                && !ignored.iter().any(|i| i == name)
                && !(*is_dir && SKIP_DIRS.contains(&name.as_str()))
        })
        .collect();

    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

fn tree_lines(cwd: &Path) -> Vec<String> {
    let ignored = ignored_names(cwd);
    let top = visible_entries(cwd, &ignored);
    let mut lines = Vec::new();
    for (i, (name, is_dir)) in top.iter().enumerate() {
        if lines.len() >= MAX_TREE_LINES {
            lines.push(format!("  ... (+{} more top-level entries)", top.len() - i));
            break;
        }
        if !is_dir {
            lines.push(format!("  {}", name));
            continue;
        }
        lines.push(format!("  {}/", name));
        let kids = visible_entries(&cwd.join(name), &ignored);
        for (kid, kid_dir) in kids.iter().take(MAX_CHILDREN) {
            lines.push(format!("    {}{}", kid, if *kid_dir { "/" } else { "" }));
        }
        if kids.len() > MAX_CHILDREN {
            lines.push(format!("    ... (+{} more)", kids.len() - MAX_CHILDREN));
        }
    }
    lines
}

fn detect_kinds(cwd: &Path) -> Vec<Kind> {
    let has = |f: &str| cwd.join(f).exists();
    let mut kinds = Vec::new();
    if has("Cargo.toml") {
        kinds.push(Kind {
            label: "Rust (Cargo)",
            commands: vec![
                "build `cargo build`".into(),
                "test `cargo test`".into(),
                "lint `cargo clippy`".into(),
            ],
            probes: &[&["rustc", "--version"], &["cargo", "--version"]],
        });
    }
    if has("package.json") {
        kinds.push(node_kind(cwd));
    }
    if has("pyproject.toml") || has("setup.py") || has("requirements.txt") {
        let pyproject = std::fs::read_to_string(cwd.join("pyproject.toml")).unwrap_or_default();
        let test = if pyproject.contains("pytest")
            || has("pytest.ini")
            || has("tests")
            || has("conftest.py")
        {
            "test `python3 -m pytest`"
        } else {
            "test `python3 -m unittest`"
        };
        let mut commands = vec![test.to_string()];
        if pyproject.contains("ruff") {
            commands.push("lint `ruff check .`".into());
        }
        kinds.push(Kind {
            label: "Python",
            commands,
            probes: &[&["python3", "--version"]],
        });
    }
    if has("go.mod") {
        kinds.push(Kind {
            label: "Go",
            commands: vec![
                "build `go build ./...`".into(),
                "test `go test ./...`".into(),
                "lint `go vet ./...`".into(),
            ],
            probes: &[&["go", "version"]],
        });
    }
    if has("pom.xml") {
        kinds.push(Kind {
            label: "Java (Maven)",
            commands: vec!["test `mvn -q test`".into()],
            probes: &[&["mvn", "-v"]],
        });
    }
    if has("build.gradle") || has("build.gradle.kts") {
        let gw = if has("gradlew") {
            "./gradlew"
        } else {
            "gradle"
        };
        kinds.push(Kind {
            label: "JVM (Gradle)",
            commands: vec![format!("test `{} test`", gw)],
            probes: &[],
        });
    }
    if has("Makefile") || has("makefile") {
        let targets = make_targets(cwd);
        if !targets.is_empty() {
            kinds.push(Kind {
                label: "Makefile",
                commands: vec![format!(
                    "targets: {}",
                    targets
                        .iter()
                        .map(|t| format!("`make {}`", t))
                        .collect::<Vec<_>>()
                        .join(", ")
                )],
                probes: &[&["make", "--version"]],
            });
        }
    }
    kinds
}

fn node_kind(cwd: &Path) -> Kind {
    let pm = if cwd.join("pnpm-lock.yaml").exists() {
        "pnpm"
    } else if cwd.join("yarn.lock").exists() {
        "yarn"
    } else if cwd.join("bun.lockb").exists() || cwd.join("bun.lock").exists() {
        "bun"
    } else {
        "npm"
    };
    let pkg: serde_json::Value = std::fs::read_to_string(cwd.join("package.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let mut commands = Vec::new();
    if let Some(scripts) = pkg.get("scripts").and_then(|s| s.as_object()) {
        for name in ["build", "test", "lint", "typecheck", "check"] {
            if scripts.contains_key(name) {
                let run = if pm == "npm" && name != "test" {
                    format!("npm run {}", name)
                } else {
                    format!(
                        "{} {}",
                        pm,
                        if name == "test" {
                            "test".to_string()
                        } else {
                            format!("run {}", name)
                        }
                    )
                };
                commands.push(format!("{} `{}`", name, run));
            }
        }
    }
    if commands.is_empty() {
        commands.push("no build/test scripts in package.json".into());
    }
    Kind {
        label: "JavaScript/TypeScript (package.json)",
        commands,
        probes: &[&["node", "--version"]],
    }
}

fn make_targets(cwd: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(cwd.join("Makefile"))
        .or_else(|_| std::fs::read_to_string(cwd.join("makefile")))
        .unwrap_or_default();
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        if line.starts_with(['\t', ' ', '.', '#']) {
            continue;
        }
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        if rest.starts_with('=')
            || name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            continue;
        }
        if !out.iter().any(|t| t == name) {
            out.push(name.to_string());
        }
        if out.len() == 8 {
            break;
        }
    }
    out
}

const TOOL_VERSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

fn tool_version(cmd: &[&str]) -> Option<String> {
    use std::io::Read;
    let mut child = Command::new(cmd[0])
        .args(&cmd[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;

    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut err = Vec::new();
        if let Some(o) = stdout.as_mut() {
            let _ = o.read_to_end(&mut out);
        }
        if let Some(e) = stderr.as_mut() {
            let _ = e.read_to_end(&mut err);
        }
        (out, err)
    });
    let deadline = std::time::Instant::now() + TOOL_VERSION_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(5))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let (out, err) = reader.join().unwrap_or_default();
    if !status?.success() {
        return None;
    }

    let text = if out.is_empty() { err } else { out };
    let line = String::from_utf8_lossy(&text)
        .lines()
        .next()?
        .trim()
        .to_string();
    (!line.is_empty()).then(|| line.chars().take(80).collect())
}

fn test_locations(cwd: &Path) -> Vec<String> {
    const NAMES: &[&str] = &["tests", "test", "__tests__", "spec"];
    let ignored = ignored_names(cwd);
    let mut found = Vec::new();
    for (name, is_dir) in visible_entries(cwd, &ignored) {
        if !is_dir {
            continue;
        }
        if NAMES.contains(&name.as_str()) {
            found.push(format!("{}/", name));
            continue;
        }
        for (kid, kid_dir) in visible_entries(&cwd.join(&name), &ignored) {
            if kid_dir && NAMES.contains(&kid.as_str()) {
                found.push(format!("{}/{}/", name, kid));
            }
        }
        if found.len() >= 6 {
            break;
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(tag: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("zen-envsnap-{}-{}", tag, uuid::Uuid::new_v4()));
        for (path, body) in files {
            let p = dir.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn cargo_project_lists_commands_tree_and_tests_but_not_ignored_dirs() {
        let dir = fixture(
            "cargo",
            &[
                ("Cargo.toml", "[package]\nname='x'"),
                ("src/main.rs", "fn main(){}"),
                ("tests/it.rs", ""),
                ("target/debug/junk", ""),
                ("generated/out.txt", ""),
                (".gitignore", "/generated/\n*.log\n"),
                (".git/HEAD", ""),
            ],
        );
        let s = project_snapshot(&dir).unwrap();
        assert!(
            s.contains("Rust (Cargo)") && s.contains("`cargo test`"),
            "{}",
            s
        );
        assert!(s.contains("  src/\n    main.rs"), "{}", s);
        assert!(s.contains("Tests live in: tests/"), "{}", s);
        assert!(
            !s.contains("target") && !s.contains("generated") && !s.contains(".git"),
            "{}",
            s
        );
        assert!(s.contains("judged by whether these checks pass"), "{}", s);
    }

    #[test]
    fn npm_scripts_and_package_manager_are_detected() {
        let dir = fixture(
            "npm",
            &[
                (
                    "package.json",
                    r#"{"scripts":{"test":"vitest","build":"tsc","dev":"vite"}}"#,
                ),
                ("pnpm-lock.yaml", ""),
                ("node_modules/left-pad/index.js", ""),
            ],
        );
        let s = project_snapshot(&dir).unwrap();
        assert!(
            s.contains("build `pnpm run build`") && s.contains("test `pnpm test`"),
            "{}",
            s
        );
        assert!(!s.contains("dev `") && !s.contains("node_modules"), "{}", s);
    }

    #[test]
    fn python_and_makefile_detection() {
        let dir = fixture(
            "py",
            &[
                ("pyproject.toml", "[tool.pytest.ini_options]\n[tool.ruff]\n"),
                (
                    "Makefile",
                    "VAR := 1\n.PHONY: test\ntest:\n\tpytest\nlint: test\n\truff .\n",
                ),
                ("pkg/mod.py", ""),
            ],
        );
        let s = project_snapshot(&dir).unwrap();
        assert!(
            s.contains("Python. Commands: test `python3 -m pytest`; lint `ruff check .`"),
            "{}",
            s
        );
        assert!(s.contains("targets: `make test`, `make lint`"), "{}", s);
        assert!(!s.contains("make VAR"), "{}", s);
    }

    #[test]
    fn empty_dir_has_no_snapshot_and_big_dirs_are_capped() {
        let empty = fixture("empty", &[]);
        assert_eq!(project_snapshot(&empty), None);

        let names: Vec<(String, &str)> = (0..300)
            .map(|i| (format!("d{:03}/f{:03}.txt", i, i), ""))
            .collect();
        let refs: Vec<(&str, &str)> = names.iter().map(|(a, b)| (a.as_str(), *b)).collect();
        let big = fixture("big", &refs);
        let s = project_snapshot(&big).unwrap();
        assert!(
            s.chars().count() <= MAX_CHARS + 30,
            "{} chars",
            s.chars().count()
        );
        assert!(s.contains("more top-level entries"), "{}", s);
        assert!(s.lines().count() < 60, "{} lines", s.lines().count());
    }
}
