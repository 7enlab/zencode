"""Terminal-Bench runner for ZenCode.

Runs a subset of Terminal-Bench 2.0 tasks against the `zen` CLI binary
using a free model, collects pass/fail, and writes a report.

Usage:
    python3 tb_runner.py --repo /path/to/terminal-bench-2 \\
        --zen /path/to/zen --subset subset.json --out results/

Each task runs in its own Docker container from the task's prebuilt image.
The zen binary is copied into the container; the model is reached via
an OpenAI-compatible endpoint (default: Ollama on the host, $0).
"""

import argparse
import dataclasses
import datetime
import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

try:
    import tomllib
except ImportError:
    import tomli as tomllib

@dataclasses.dataclass
class Task:
    name: str
    task_dir: Path
    instruction: str
    image: str
    agent_timeout_sec: float
    difficulty: str
    category: str

@dataclasses.dataclass
class TaskResult:
    name: str
    passed: bool | None
    duration_sec: float
    agent_exit_code: int | None
    error: str | None

def discover_tasks(repo_dir: Path) -> dict[str, Task]:
    """Parse all tasks from a terminal-bench-2 checkout."""
    tasks: dict[str, Task] = {}
    for task_dir in sorted(repo_dir.iterdir()):
        toml_path = task_dir / "task.toml"
        instr_path = task_dir / "instruction.md"
        if not toml_path.exists() or not instr_path.exists():
            continue
        with open(toml_path, "rb") as f:
            cfg = tomllib.load(f)
        tasks[task_dir.name] = Task(
            name=task_dir.name,
            task_dir=task_dir,
            instruction=instr_path.read_text().strip(),
            image=cfg["environment"]["docker_image"],
            agent_timeout_sec=float(cfg["agent"].get("timeout_sec", 900)),
            difficulty=cfg.get("metadata", {}).get("difficulty", "unknown"),
            category=cfg.get("metadata", {}).get("category", "unknown"),
        )
    return tasks

def sh(cmd: list[str], timeout: float | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)

def run_task(task: Task, zen_bin: Path, model: str, base_url: str,
             logs_dir: Path, agent_timeout: float | None = None) -> TaskResult:
    """Run one task end-to-end in Docker. Returns the result."""
    t0 = time.time()
    cname = f"tb-zen-{task.name}-{int(t0)}".replace("_", "-")
    timeout = min(agent_timeout or task.agent_timeout_sec, 1200)
    res = TaskResult(name=task.name, passed=None, duration_sec=0.0,
                     agent_exit_code=None, error=None)
    try:

        r = sh(["docker", "pull", task.image], timeout=600)
        if r.returncode != 0:
            res.error = f"docker pull failed: {r.stderr[:300]}"
            return res

        r = sh(["docker", "run", "-d", "--rm",
                "--add-host=host.docker.internal:host-gateway",
                "--name", cname, task.image, "sleep", "infinity"], timeout=120)
        if r.returncode != 0:
            res.error = f"docker run failed: {r.stderr[:300]}"
            return res

        r = sh(["docker", "cp", str(zen_bin), f"{cname}:/usr/local/bin/zen"], timeout=120)
        if r.returncode != 0:
            res.error = f"docker cp zen failed: {r.stderr[:300]}"
            return res
        sh(["docker", "exec", cname, "chmod", "+x", "/usr/local/bin/zen"], timeout=30)

        with tempfile.NamedTemporaryFile("w", suffix=".md", delete=False) as f:
            f.write(task.instruction)
            instr_host = f.name
        try:
            r = sh(["docker", "cp", instr_host, f"{cname}:/tmp/instruction.md"], timeout=60)
            if r.returncode != 0:
                res.error = f"docker cp instruction failed: {r.stderr[:300]}"
                return res
        finally:
            os.unlink(instr_host)

        wrapper = (
            "cd /app 2>/dev/null || cd /workspace 2>/dev/null || true\n"
            f"timeout {int(timeout)} /usr/local/bin/zen "
            f"--base-url {base_url} --model {model} "
            "'-p' \"$(cat /tmp/instruction.md)\"\n"
        )
        r = sh(["docker", "exec", cname, "bash", "-c", wrapper], timeout=timeout + 180)
        res.agent_exit_code = r.returncode
        (logs_dir / f"{task.name}-agent.log").write_text(
            f"=== STDOUT (tail) ===\n{r.stdout[-20000:]}\n=== STDERR (tail) ===\n{r.stderr[-5000:]}\n")

        r = sh(["docker", "cp", str(task.task_dir / "tests"), f"{cname}:/tests"], timeout=60)
        if r.returncode != 0:
            res.error = f"docker cp tests failed: {r.stderr[:300]}"
            return res
        r = sh(["docker", "exec", cname, "bash", "-c",
                "mkdir -p /logs/verifier && bash /tests/test.sh"],
               timeout=900)
        (logs_dir / f"{task.name}-verify.log").write_text(
            f"exit={r.returncode}\n--- stdout ---\n{r.stdout[-15000:]}\n--- stderr ---\n{r.stderr[-5000:]}\n")

        r = sh(["docker", "exec", cname, "cat", "/logs/verifier/reward.txt"], timeout=30)
        if r.returncode != 0:
            res.error = f"no reward.txt (verify exit unknown): {r.stderr[:200]}"
            return res
        reward = r.stdout.strip()
        res.passed = (reward == "1")
        return res
    except subprocess.TimeoutExpired:
        res.error = "runner timeout"
        return res
    except Exception as e:
        res.error = f"{type(e).__name__}: {e}"
        return res
    finally:
        sh(["docker", "rm", "-f", cname], timeout=60)
        res.duration_sec = time.time() - t0

def write_report(path: Path, results: list[TaskResult], model: str,
                 base_url: str, zen_version: str) -> None:
    passed = sum(1 for r in results if r.passed)
    failed = sum(1 for r in results if r.passed is False)
    errored = sum(1 for r in results if r.passed is None)
    score = f"{passed}/{len(results)}"
    lines = [
        "# Terminal-Bench subset - ZenCode results",
        "",
        f"- Model: `{model}` (free, $0) via `{base_url}`",
        f"- ZenCode: `{zen_version}`",
        f"- Date: {datetime.date.today().isoformat()}",
        f"- Score: **{score}** ({passed} passed, {failed} failed, {errored} infra errors)",
        "",
        "| Task | Result | Duration | Notes |",
        "|---|---|---|---|",
    ]
    for r in results:
        status = "PASS" if r.passed else ("ERROR" if r.passed is None else "FAIL")
        notes = (r.error or "")[:80].replace("|", "/")
        lines.append(f"| {r.name} | {status} | {r.duration_sec:.0f}s | {notes} |")
    lines += [
        "",
        "## Methodology",
        "",
        "- Each task runs in its own Docker container from the task's prebuilt",
        "  Terminal-Bench 2.0 image (`terminal-bench@2.0` dataset, 89 tasks).",
        "- The `zen` binary is copied into the container and invoked headless",
        "  (`zen -p`) with the task's `instruction.md`.",
        "- The model is reached via an OpenAI-compatible endpoint; no API key, $0 cost.",
        "- Verification uses the task's own `tests/test.sh`; pass = `reward.txt` == 1.",
        "- Per-task agent timeout is capped at 20 minutes.",
        "",
        "## Limitations",
        "",
        "- Small local model (4B params, CPU inference); this validates the",
        "  benchmark pipeline, not ZenCode's ceiling with frontier models.",
        "- Subset only; not comparable to full 89-task leaderboard scores.",
        "- Single run per task; no retries.",
    ]
    path.write_text("\n".join(lines) + "\n")

def main() -> int:
    ap = argparse.ArgumentParser(description="Run Terminal-Bench subset with zen")
    ap.add_argument("--repo", required=True, help="terminal-bench-2 checkout dir")
    ap.add_argument("--zen", required=True, help="path to zen binary")
    ap.add_argument("--subset", required=True, help="JSON list of task names")
    ap.add_argument("--out", required=True, help="output dir for results")
    ap.add_argument("--model", default="qwen3:4b")
    ap.add_argument("--base-url", default="http://host.docker.internal:11434/v1")
    ap.add_argument("--agent-timeout", type=float, default=None,
                    help="override per-task agent timeout (sec)")
    args = ap.parse_args()

    repo, zen_bin, out = Path(args.repo), Path(args.zen), Path(args.out)
    if not zen_bin.exists():
        print(f"ERROR: zen binary not found: {zen_bin}", file=sys.stderr)
        return 2
    out.mkdir(parents=True, exist_ok=True)
    logs_dir = out / "logs"
    logs_dir.mkdir(exist_ok=True)

    tasks = discover_tasks(repo)
    print(f"discovered {len(tasks)} tasks")
    subset_names: list[str] = json.loads(Path(args.subset).read_text())
    missing = [n for n in subset_names if n not in tasks]
    if missing:
        print(f"ERROR: unknown tasks: {missing}", file=sys.stderr)
        return 2

    r = sh([str(zen_bin), "--version"], timeout=30)
    zen_version = (r.stdout or r.stderr).strip().splitlines()[0] if r.stdout or r.stderr else "?"

    results: list[TaskResult] = []
    for name in subset_names:
        task = tasks[name]
        print(f"[{datetime.datetime.now().isoformat()}] running {name} "
              f"(difficulty={task.difficulty}, timeout={task.agent_timeout_sec:.0f}s)",
              flush=True)
        res = run_task(task, zen_bin, args.model, args.base_url, logs_dir,
                       agent_timeout=args.agent_timeout)
        results.append(res)
        status = "PASS" if res.passed else ("ERROR" if res.passed is None else "FAIL")
        print(f"  -> {status} in {res.duration_sec:.0f}s"
              + (f" ({res.error})" if res.error else ""), flush=True)

    with open(out / "results.json", "w") as f:
        json.dump([dataclasses.asdict(r) for r in results], f, indent=2)
    write_report(out / "report.md", results, args.model, args.base_url, zen_version)

    passed = sum(1 for r in results if r.passed)
    errored = sum(1 for r in results if r.passed is None)
    print(f"\nDone: {passed}/{len(results)} passed, {errored} infra errors")
    return 0

if __name__ == "__main__":
    sys.exit(main())
