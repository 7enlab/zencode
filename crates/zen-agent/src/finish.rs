use std::path::Path;
use std::time::Duration;

pub const MAX_VERIFY_ROUNDS: usize = 2;

const TAIL_LINES: usize = 40;
const TAIL_CHARS: usize = 4000;

#[derive(Default)]
pub struct FinishGate {

    changed_since_command: Vec<String>,

    changed_since_verify: Vec<String>,
    reminded: bool,
    verify_rounds: usize,
}

#[derive(Debug, PartialEq)]
pub enum Decision {

    Finish,

    FinishWithNote(String),

    Continue(String),
}

impl FinishGate {

    pub fn observe(&mut self, tool: &str, file_path: Option<&str>, is_error: bool) {
        match tool {
            "Edit" | "MultiEdit" | "Write" if !is_error => {
                if let Some(f) = file_path {
                    for list in [&mut self.changed_since_command, &mut self.changed_since_verify] {
                        if !list.iter().any(|x| x == f) {
                            list.push(f.to_string());
                        }
                    }
                }
            }

            "Bash" => self.changed_since_command.clear(),
            _ => {}
        }
    }

    pub async fn decide(
        &mut self,
        reminder: bool,
        verify_command: Option<&str>,
        timeout: Duration,
        cwd: &Path,
        env: &std::collections::HashMap<String, String>,
    ) -> Decision {
        if let Some(cmd) = verify_command {
            if self.changed_since_verify.is_empty() {
                return Decision::Finish;
            }
            if self.verify_rounds >= MAX_VERIFY_ROUNDS {
                return Decision::FinishWithNote(format!(
                    "verify: `{}` was not re-run after the last changes ({} automatic rounds used)",
                    cmd, MAX_VERIFY_ROUNDS
                ));
            }
            self.verify_rounds += 1;
            self.changed_since_verify.clear();
            self.changed_since_command.clear();
            let (passed, status, tail) = run_command(cmd, cwd, env, timeout).await;
            if passed {
                return Decision::Finish;
            }
            return Decision::Continue(format!(
                "[zen: automatic check {} of {}: `{}` failed ({}) after your changes. Last lines of output:\n{}\nFix the cause, then finish.]",
                self.verify_rounds, MAX_VERIFY_ROUNDS, cmd, status, tail
            ));
        }
        if reminder && !self.reminded && !self.changed_since_command.is_empty() {
            self.reminded = true;
            return Decision::Continue(format!(
                "[zen: you changed {} file(s) ({}) and have not run any command since. Before finishing, run the project's build/test checks (`/done` runs them for you) and compare the result with the request. If no check applies, say so in your answer.]",
                self.changed_since_command.len(),
                self.changed_since_command.join(", ")
            ));
        }
        Decision::Finish
    }
}

pub(crate) async fn run_command(
    cmd: &str,
    cwd: &Path,
    env: &std::collections::HashMap<String, String>,
    timeout: Duration,
) -> (bool, String, String) {

    let shell = zen_core::resolve_shell("sh");
    let mut c = tokio::process::Command::new(&shell.program);
    c.args(shell.args(cmd)).current_dir(cwd).envs(env).kill_on_drop(true);
    c.stdin(std::process::Stdio::null());
    match tokio::time::timeout(timeout, c.output()).await {
        Err(_) => (false, format!("timed out after {}s", timeout.as_secs()), String::new()),
        Ok(Err(e)) => (false, format!("could not start: {}", e), String::new()),
        Ok(Ok(out)) => {
            let mut text = String::from_utf8_lossy(&out.stdout).to_string();
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            let lines: Vec<&str> = text.lines().collect();
            let mut tail = lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n");
            if tail.chars().count() > TAIL_CHARS {
                tail = tail.chars().rev().take(TAIL_CHARS).collect::<Vec<_>>().into_iter().rev().collect();
            }
            let status = match out.status.code() {
                Some(c) => format!("exit {}", c),
                None => "killed by a signal".into(),
            };
            (out.status.success(), status, tail)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    async fn decide(g: &mut FinishGate, reminder: bool, cmd: Option<&str>) -> Decision {
        g.decide(reminder, cmd, Duration::from_secs(10), &std::env::temp_dir(), &HashMap::new()).await
    }

    #[tokio::test]
    async fn one_reminder_after_edits_without_a_command() {
        let mut g = FinishGate::default();
        assert_eq!(decide(&mut g, true, None).await, Decision::Finish, "no edits, no reminder");
        g.observe("Edit", Some("a.rs"), false);
        g.observe("Write", Some("b.rs"), false);
        g.observe("Edit", Some("a.rs"), false);
        match decide(&mut g, true, None).await {
            Decision::Continue(t) => assert!(t.contains("2 file(s) (a.rs, b.rs)"), "{}", t),
            d => panic!("expected a reminder, got {:?}", d),
        }
        g.observe("Edit", Some("c.rs"), false);
        assert_eq!(decide(&mut g, true, None).await, Decision::Finish, "only one reminder per turn");
    }

    #[tokio::test]
    async fn a_command_or_failed_edit_means_no_reminder() {
        let mut g = FinishGate::default();
        g.observe("Edit", Some("a.rs"), false);
        g.observe("Bash", None, true);
        assert_eq!(decide(&mut g, true, None).await, Decision::Finish);
        let mut g = FinishGate::default();
        g.observe("Edit", Some("a.rs"), true);
        assert_eq!(decide(&mut g, true, None).await, Decision::Finish);
        let mut g = FinishGate::default();
        g.observe("Edit", Some("a.rs"), false);
        assert_eq!(decide(&mut g, false, None).await, Decision::Finish, "reminder disabled");
    }

    #[tokio::test]
    async fn verify_command_runs_feeds_back_failure_and_caps_rounds() {
        let mut g = FinishGate::default();
        g.observe("Edit", Some("a.rs"), false);
        match decide(&mut g, true, Some("echo first; echo boom >&2; exit 3")).await {
            Decision::Continue(t) => {
                assert!(t.contains("check 1 of 2") && t.contains("exit 3") && t.contains("boom"), "{}", t)
            }
            d => panic!("{:?}", d),
        }

        assert_eq!(decide(&mut g, true, Some("exit 3")).await, Decision::Finish);
        g.observe("Edit", Some("a.rs"), false);
        assert!(matches!(decide(&mut g, true, Some("exit 3")).await, Decision::Continue(t) if t.contains("check 2 of 2")));
        g.observe("Edit", Some("a.rs"), false);
        assert!(matches!(decide(&mut g, true, Some("exit 3")).await, Decision::FinishWithNote(_)));
    }

    #[tokio::test]
    async fn passing_verify_finishes_without_reminder() {
        let mut g = FinishGate::default();
        g.observe("Edit", Some("a.rs"), false);
        assert_eq!(decide(&mut g, true, Some("true")).await, Decision::Finish);
    }
}
