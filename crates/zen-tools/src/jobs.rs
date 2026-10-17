//! Background Bash jobs (p0b, after dsh `ctx.jobs`).
//!
//! `Bash {background: true}` starts a job: the command's stdout and stderr go to
//! `<scratch>/jobs/<pid>-<job-id>.log` and the call returns at once with the job id. `JobOutput`
//! returns what the job printed since the last read plus its status; `JobKill` stops it. On Unix
//! the job leads its own process group so a kill takes its children too; on Windows the tree is
//! ended with `taskkill /T` and `Child::kill` as the fallback.
//!
//! The registry is process-wide (job ids are unique per process), so it needs no new field on
//! `ToolContext` and survives the per-call context clones the agent makes.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use zen_core::{Tool, ToolContext, ToolOutput};

/// Output cap for one JobOutput reply; longer chunks are spilled to a file.
pub const JOB_OUTPUT_MAX_CHARS: usize = 30_000;
/// Longest JobOutput `wait_ms`.
pub const JOB_MAX_WAIT_MS: u64 = 600_000;

#[derive(Debug, Clone, PartialEq)]
pub enum JobStatus {
    Running,
    Exited(Option<i32>),
    Killed,
    Failed(String),
}

impl JobStatus {
    fn describe(&self) -> String {
        match self {
            JobStatus::Running => "running".into(),
            JobStatus::Exited(Some(c)) => format!("exited with code {}", c),
            JobStatus::Exited(None) => "exited (terminated by a signal)".into(),
            JobStatus::Killed => "killed".into(),
            JobStatus::Failed(e) => format!("failed: {}", e),
        }
    }
}

struct Job {
    command: String,
    log: PathBuf,
    read_pos: u64,
    status: Arc<Mutex<JobStatus>>,
    kill_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

fn jobs() -> &'static Mutex<HashMap<String, Job>> {
    static JOBS: OnceLock<Mutex<HashMap<String, Job>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_id() -> String {
    static N: AtomicU64 = AtomicU64::new(1);
    format!("job-{}", N.fetch_add(1, Ordering::Relaxed))
}

/// A started job, as reported back to Bash.
pub struct Started {
    pub id: String,
    pub pid: Option<u32>,
    pub log: PathBuf,
}

/// Start `command` in the background in `cwd` with `env`, logging to the session scratch dir.
pub fn start_job(command: &str, cwd: &Path, env: &HashMap<String, String>, scratch: &Path) -> Result<Started, String> {
    let id = next_id();
    let dir = scratch.join("jobs");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {}", dir.display(), e))?;
    let log = dir.join(format!("{}-{}.log", std::process::id(), id));
    let out = std::fs::File::create(&log).map_err(|e| format!("cannot create {}: {}", log.display(), e))?;
    let err = out.try_clone().map_err(|e| format!("cannot open {}: {}", log.display(), e))?;

    let shell = zen_core::resolve_shell("bash");
    let mut cmd = tokio::process::Command::new(&shell.program);
    cmd.args(shell.args(command))
        .current_dir(cwd)
        .envs(env.iter())
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .kill_on_drop(false);
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd.spawn().map_err(|e| format!("failed to start {}: {}", shell.program, e))?;
    let pid = child.id();

    let status = Arc::new(Mutex::new(JobStatus::Running));
    let (kill_tx, kill_rx) = tokio::sync::oneshot::channel::<()>();
    let st = status.clone();
    tokio::spawn(async move {
        let end = tokio::select! {
            r = child.wait() => match r {
                Ok(s) => JobStatus::Exited(s.code()),
                Err(e) => JobStatus::Failed(e.to_string()),
            },
            _ = kill_rx => {
                kill_tree(pid);
                let _ = child.kill().await;
                JobStatus::Killed
            }
        };
        *st.lock().unwrap() = end;
    });

    jobs().lock().unwrap().insert(
        id.clone(),
        Job { command: command.to_string(), log: log.clone(), read_pos: 0, status, kill_tx: Some(kill_tx) },
    );
    Ok(Started { id, pid, log })
}

/// Kill the job's whole process tree (best effort; `Child::kill` follows).
fn kill_tree(pid: Option<u32>) {
    let Some(pid) = pid else { return };
    #[cfg(unix)]
    {
        // The job leads its own process group (pgid == pid).
        let _ = std::process::Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", pid)])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn status_of(id: &str) -> Option<JobStatus> {
    jobs().lock().unwrap().get(id).map(|j| j.status.lock().unwrap().clone())
}

/// New output since the last read: `(text, from_byte, to_byte, status, command)`.
/// While the job runs, an incomplete UTF-8 sequence at the end is held back for the next read.
pub async fn read_new(id: &str, wait_ms: u64) -> Result<(String, u64, u64, JobStatus, String), String> {
    if status_of(id).is_none() {
        return Err(unknown(id));
    }
    let deadline = tokio::time::Instant::now() + Duration::from_millis(wait_ms.min(JOB_MAX_WAIT_MS));
    while status_of(id) == Some(JobStatus::Running) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let found = {
        let map = jobs().lock().unwrap();
        map.get(id).map(|j| (j.log.clone(), j.read_pos, j.status.lock().unwrap().clone(), j.command.clone()))
    };
    // (`unknown` takes the registry lock itself, so it is called after the guard is gone.)
    let (log, from, status, command) = found.ok_or_else(|| unknown(id))?;
    let bytes = read_from(&log, from).map_err(|e| format!("cannot read {}: {}", log.display(), e))?;
    let take = match std::str::from_utf8(&bytes) {
        Ok(_) => bytes.len(),
        // Incomplete sequence at the very end of a running job's log: wait for the rest.
        Err(e) if e.error_len().is_none() && status == JobStatus::Running => e.valid_up_to(),
        Err(_) => bytes.len(),
    };
    let text = String::from_utf8_lossy(&bytes[..take]).to_string();
    let to = from + take as u64;
    if let Some(j) = jobs().lock().unwrap().get_mut(id) {
        j.read_pos = to;
    }
    Ok((text, from, to, status, command))
}

fn read_from(path: &Path, from: u64) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(from))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(buf)
}

/// Stop a job; returns its final status (waits up to 5 s for the process to go).
pub async fn kill_job(id: &str) -> Result<JobStatus, String> {
    let tx = {
        let mut map = jobs().lock().unwrap();
        map.get_mut(id).map(|j| j.kill_tx.take())
    };
    let tx = tx.ok_or_else(|| unknown(id))?;
    if let Some(tx) = tx {
        let _ = tx.send(());
    }
    for _ in 0..100 {
        match status_of(id) {
            Some(JobStatus::Running) => tokio::time::sleep(Duration::from_millis(50)).await,
            Some(s) => return Ok(s),
            None => return Err(unknown(id)),
        }
    }
    Ok(status_of(id).unwrap_or(JobStatus::Running))
}

fn unknown(id: &str) -> String {
    let mut ids: Vec<String> = jobs().lock().unwrap().keys().cloned().collect();
    ids.sort();
    if ids.is_empty() {
        format!("Error: no background job '{}' (no jobs started in this session)", id)
    } else {
        format!("Error: no background job '{}' (known: {})", id, ids.join(", "))
    }
}

fn job_id_arg(args: &Value) -> Option<String> {
    args.get("job_id").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}

// ───────────────────────────── tools ─────────────────────────────

/// Read new output of a background Bash job.
pub struct JobOutputTool;

#[async_trait]
impl Tool for JobOutputTool {
    fn name(&self) -> &str {
        "JobOutput"
    }
    fn description(&self) -> &str {
        "Read a background Bash job (started with Bash background: true): returns the output \
         printed since your last JobOutput call for that job and whether it is still running or \
         its exit code. Set wait_ms to wait for the job to finish first."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "job_id": {"type": "string", "description": "Job id returned by Bash, e.g. job-1."},
                "wait_ms": {"type": "integer", "description": "Wait up to this many milliseconds for the job to finish before reading (default 0, max 600000)."}
            },
            "required": ["job_id"],
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        true
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let Some(id) = job_id_arg(&args) else {
            return ToolOutput::err("Error: missing required parameter 'job_id'");
        };
        let wait_ms = args.get("wait_ms").and_then(|v| v.as_u64()).unwrap_or(0);
        match read_new(&id, wait_ms).await {
            Err(e) => ToolOutput::err(e),
            Ok((text, from, to, status, command)) => {
                let head = format!("Job {} ({}): {}", id, command, status.describe());
                if text.is_empty() {
                    ToolOutput::ok(format!("{}\n(no new output; {} bytes so far)", head, to))
                } else {
                    let body = crate::builtin::spill_output(&text, JOB_OUTPUT_MAX_CHARS, ctx);
                    ToolOutput::ok(format!("{}\nNew output (bytes {}-{}):\n{}", head, from, to, body))
                }
            }
        }
    }
}

/// Stop a background Bash job.
pub struct JobKillTool;

#[async_trait]
impl Tool for JobKillTool {
    fn name(&self) -> &str {
        "JobKill"
    }
    fn description(&self) -> &str {
        "Stop a background Bash job (and the processes it started) by its job id."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "job_id": {"type": "string", "description": "Job id returned by Bash, e.g. job-1."}
            },
            "required": ["job_id"],
            "additionalProperties": false
        })
    }
    fn is_read_only(&self) -> bool {
        false
    }
    async fn call(&self, args: Value, _ctx: &ToolContext) -> ToolOutput {
        let Some(id) = job_id_arg(&args) else {
            return ToolOutput::err("Error: missing required parameter 'job_id'");
        };
        match kill_job(&id).await {
            Err(e) => ToolOutput::err(e),
            Ok(JobStatus::Running) => ToolOutput::err(format!("Error: job {} did not stop within 5 s", id)),
            Ok(s) => ToolOutput::ok(format!("Job {}: {}", id, s.describe())),
        }
    }
}
