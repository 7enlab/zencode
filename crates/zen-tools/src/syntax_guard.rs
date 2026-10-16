//! Syntax-guarded edits (r8).
//!
//! Source: SWE-agent, "Agent-Computer Interfaces Enable Automated Software Engineering"
//! (Yang et al., NeurIPS 2024, arXiv:2405.15793, §3 and Table 3): the edit command runs a linter
//! and does not apply a change that introduces a syntax error; the agent is shown the error with
//! the surrounding lines and asked to edit again. Their ablation lost 3.0 pp without it, and they
//! note that repeated broken edits are what usually start an edit loop.
//!
//! Here: before Write/Edit/MultiEdit writes a file whose type we can check cheaply (Python,
//! JavaScript, JSON, shell), the new content is checked. When it fails and the old content passed
//! (or the file is new), the write is refused with the checker's message and the numbered lines
//! around the error. A file that was already broken is written as asked (the model may be in the
//! middle of fixing it), and a missing checker never blocks anything.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, PartialEq, Debug)]
enum Lang {
    Python,
    JavaScript,
    Json,
    Shell,
}

fn lang_of(path: &Path) -> Option<Lang> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "py" => Some(Lang::Python),
        "js" | "cjs" | "mjs" => Some(Lang::JavaScript),
        "json" => Some(Lang::Json),
        "sh" | "bash" => Some(Lang::Shell),
        _ => None,
    }
}

/// `Some(message)` when `text` does not parse as the language of `path`; `None` when it parses,
/// the type is not checked, or no checker is available.
pub async fn syntax_error(path: &Path, text: &str) -> Option<String> {
    let lang = lang_of(path)?;
    match lang {
        Lang::Json => serde_json::from_str::<serde_json::Value>(text).err().map(|e| e.to_string()),
        Lang::Python => run_checker(
            "python3",
            &["-c", "import ast,sys\nsrc=sys.stdin.read()\ntry:\n ast.parse(src)\nexcept SyntaxError as e:\n print('line %s: %s' % (e.lineno, e.msg)); sys.exit(1)"],
            text,
        )
        .await,
        Lang::Shell => run_checker("bash", &["-n"], text).await.map(|m| m.replace("bash: ", "")),
        Lang::JavaScript => {
            // `node --check` needs a file; check a sibling-named temp copy so the module type
            // (.mjs/.cjs) is the same.
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("js");
            let tmp = std::env::temp_dir().join(format!("zen-syntax-{}.{}", uuid_like(), ext));
            tokio::fs::write(&tmp, text).await.ok()?;
            let out = run_checker("node", &["--check", tmp.to_str()?], "").await;
            let _ = tokio::fs::remove_file(&tmp).await;
            out.map(|m| m.replace(tmp.to_str().unwrap_or(""), "file"))
        }
    }
}

fn uuid_like() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{}-{}", std::process::id(), t)
}

/// Run `cmd` with `text` on stdin; `Some(output)` when it exits non-zero.
async fn run_checker(cmd: &str, args: &[&str], text: &str) -> Option<String> {
    let mut child = tokio::process::Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes()).await;
    }
    let out = tokio::time::timeout(CHECK_TIMEOUT, child.wait_with_output()).await.ok()?.ok()?;
    if out.status.success() {
        return None;
    }
    let mut msg = String::from_utf8_lossy(&out.stdout).to_string();
    msg.push_str(&String::from_utf8_lossy(&out.stderr));
    let msg = msg.trim().to_string();
    Some(if msg.is_empty() { "syntax check failed".into() } else { msg.chars().take(1500).collect() })
}

/// First line number mentioned in a checker message (`line 12`, `:12:`, `line 12,`).
fn error_line(msg: &str) -> Option<usize> {
    let lower = msg.to_ascii_lowercase();
    if let Some(i) = lower.find("line ") {
        let digits: String = lower[i + 5..].chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = digits.parse() {
            return Some(n);
        }
    }
    msg.split(':').find_map(|p| p.trim().parse::<usize>().ok())
}

/// Error text for a refused write: the checker message plus numbered lines around the error.
fn refusal(path: &Path, new_text: &str, msg: &str) -> String {
    let mut out = format!(
        "Error: this change was NOT applied because it would leave {} with a syntax error:\n{}\n",
        path.display(),
        msg
    );
    if let Some(line) = error_line(msg) {
        let lines: Vec<&str> = new_text.lines().collect();
        let from = line.saturating_sub(4).max(1);
        let to = (line + 3).min(lines.len());
        if from <= to {
            out.push_str("Your version around that line:\n");
            for n in from..=to {
                out.push_str(&format!("{:>6}{}\t{}\n", n, if n == line { ">" } else { " " }, lines[n - 1]));
            }
        }
    }
    out.push_str("The file on disk is unchanged. Fix the syntax (indentation, brackets, quotes) and edit again.");
    out
}

/// Decide whether writing `new_text` over `old_text` (None = new file) may proceed.
pub async fn guard_write(path: &Path, old_text: Option<&str>, new_text: &str) -> Result<(), String> {
    if std::env::var_os("ZEN_NO_SYNTAX_GUARD").is_some() {
        return Ok(());
    }
    let Some(msg) = syntax_error(path, new_text).await else { return Ok(()) };
    if let Some(old) = old_text {
        if syntax_error(path, old).await.is_some() {
            return Ok(()); // already broken before this edit: do not block a repair in progress
        }
    }
    Err(refusal(path, new_text, &msg))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has(cmd: &str) -> bool {
        std::process::Command::new(cmd).arg("--version").output().is_ok()
    }

    #[tokio::test]
    async fn python_edit_that_breaks_syntax_is_refused_with_context() {
        if !has("python3") {
            return;
        }
        let p = Path::new("m.py");
        let old = "def f():\n    return 1\n";
        assert!(guard_write(p, Some(old), "def f():\n    return 2\n").await.is_ok());
        let err = guard_write(p, Some(old), "def f():\n    if x\n        return 2\n").await.unwrap_err();
        assert!(err.contains("NOT applied") && err.contains("line 2"), "{}", err);
        assert!(err.contains("     2>\t    if x"), "{}", err);
        // New file with broken syntax is refused too; an already-broken file is not blocked.
        assert!(guard_write(p, None, "def (:\n").await.is_err());
        assert!(guard_write(p, Some("def (:\n"), "def (:\n  x\n").await.is_ok());
    }

    #[tokio::test]
    async fn json_shell_and_js_are_checked_and_other_types_are_not() {
        assert!(guard_write(Path::new("a.json"), Some("{}"), "{\"a\": 1,}").await.is_err());
        assert!(guard_write(Path::new("a.json"), Some("{}"), "{\"a\": 1}").await.is_ok());
        assert!(guard_write(Path::new("a.txt"), Some("x"), "{{{").await.is_ok());
        assert!(guard_write(Path::new("a.rs"), Some("fn a(){}"), "fn a({").await.is_ok());
        if has("bash") {
            assert!(guard_write(Path::new("s.sh"), Some("echo hi\n"), "if then fi\n").await.is_err());
            assert!(guard_write(Path::new("s.sh"), Some("echo hi\n"), "echo ok\n").await.is_ok());
        }
        if has("node") {
            assert!(guard_write(Path::new("f.js"), Some("let a = 1;\n"), "let a = ;\n").await.is_err());
            assert!(guard_write(Path::new("f.js"), Some("let a = 1;\n"), "let a = 2;\n").await.is_ok());
        }
    }
}
