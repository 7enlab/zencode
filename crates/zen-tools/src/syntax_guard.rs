













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
        Lang::Shell => {


            let sh = zen_core::resolve_shell("bash");
            if sh.kind != zen_core::ShellKind::Posix {
                return None;
            }
            run_checker(&sh.program, &["-n"], text).await.map(|m| m.replace("bash: ", ""))
        }
        Lang::JavaScript => {


            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("js");
            let tmp = std::env::temp_dir().join(format!("zen-syntax-{}.{}", random_suffix(), ext));


            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp).ok()?;
            {
                use std::io::Write;
                if f.write_all(text.as_bytes()).is_err() {
                    let _ = std::fs::remove_file(&tmp);
                    return None;
                }
            }
            let out = run_checker("node", &["--check", tmp.to_str()?], "").await;
            let _ = tokio::fs::remove_file(&tmp).await;
            out.map(|m| m.replace(tmp.to_str().unwrap_or(""), "file"))
        }
    }
}



fn random_suffix() -> String {
    #[cfg(unix)]
    {
        use std::io::Read;
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            let mut buf = [0u8; 16];
            if f.read_exact(&mut buf).is_ok() {
                return buf.iter().map(|b| format!("{:02x}", b)).collect();
            }
        }
    }



    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    std::process::id().hash(&mut h);
    std::time::SystemTime::now().hash(&mut h);
    std::thread::current().id().hash(&mut h);
    let stack_var = 0u8;
    std::ptr::addr_of!(stack_var).hash(&mut h);
    format!("{:016x}{:016x}", h.finish(), h.finish().wrapping_mul(0x9e3779b97f4a7c15))
}


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


pub async fn guard_write(path: &Path, old_text: Option<&str>, new_text: &str) -> Result<(), String> {
    if std::env::var_os("ZEN_NO_SYNTAX_GUARD").is_some() {
        return Ok(());
    }
    let Some(msg) = syntax_error(path, new_text).await else { return Ok(()) };
    if let Some(old) = old_text {
        if syntax_error(path, old).await.is_some() {
            return Ok(());
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
