"""Cold-start benchmarks for the `zen` binary.

Measures, with no third-party dependencies (stdlib only, cross-platform):

  1. ``startup_cold_ms``  - wall time for ``zen --version`` on a *fresh copy* of the
     binary (new inode -> page cache cold), first run only per copy.
  2. ``startup_warm_ms``  - wall time for ``zen --version`` on the same binary,
     repeated (page cache warm).
  3. ``doctor_warm_ms``    - wall time for ``zen doctor`` (config resolution path).
  4. ``repl_prompt_ms``    - time from process spawn until the REPL prints its
     first ``> `` prompt (stdin fed ``/quit`` afterwards).
  5. ``first_token_ms``    - time from process spawn until the first stdout byte
     of ``zen -p <prompt>`` against a local mock OpenAI-compatible server.
  6. ``peak_rss_mb``       - peak resident set size of the warm runs (POSIX via
     ``os.wait4``; ``None`` where unsupported).

Environment isolation: HOME/USERPROFILE/XDG_* are pointed at a fresh temp dir
per measurement group so real user state (~/.zen) is never touched.

Usage:
    python3 scripts/bench_cold_start.py --zen ./target/release/zen --runs 15
    python3 scripts/bench_cold_start.py --zen ./target/release/zen --self-test
    python3 scripts/bench_cold_start.py --zen ./target/release/zen \\
        --thresholds benches/cold_start_thresholds.json   # CI regression guard

Exit codes: 0 = ok (+ all thresholds held), 1 = threshold regression or error,
2 = a measurement was unstable (CV above limit) or failed.
"""

import argparse
import json
import math
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

CV_LIMIT = 0.30
PROMPT_MARKER = b"> "
REPL_TIMEOUT_S = 30
TOKEN_TIMEOUT_S = 60

MOCK_COMPLETION = {
    "id": "bench-1",
    "object": "chat.completion",
    "created": 1,
    "model": "bench",
    "choices": [
        {
            "index": 0,
            "message": {"role": "assistant", "content": "hello"},
            "finish_reason": "stop",
        }
    ],
    "usage": {"prompt_tokens": 10, "completion_tokens": 1, "total_tokens": 11},
}

def stats(samples):
    """Return dict with median/p95/mean/stdev/cv for a sample list."""
    s = sorted(samples)
    n = len(s)
    median = s[n // 2] if n % 2 else (s[n // 2 - 1] + s[n // 2]) / 2
    p95 = s[min(n - 1, math.ceil(0.95 * n) - 1)]
    mean = statistics.fmean(s)
    stdev = statistics.pstdev(s) if n > 1 else 0.0
    return {
        "n": n,
        "median": median,
        "p95": p95,
        "mean": mean,
        "stdev": stdev,
        "cv": (stdev / mean) if mean else 0.0,
        "min": s[0],
        "max": s[-1],
    }

def run_timed(argv, env, stdin_data=None, timeout=60):
    """Run argv, return (elapsed_ms, returncode, stdout_bytes, stderr_bytes, rss_mb).

    Peak RSS via os.wait4 on POSIX; None where unsupported. NOTE: we must reap
    the child with wait4 ourselves (not via communicate(), which reaps first and
    makes rusage unavailable) - stdout/stderr are drained on threads.
    """
    import threading

    start = time.perf_counter()
    stdin_cfg = subprocess.PIPE if stdin_data is not None else subprocess.DEVNULL
    proc = subprocess.Popen(
        argv, env=env, stdin=stdin_cfg, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    out_chunks, err_chunks = [], []

    def drain(pipe, chunks):
        try:
            while True:
                data = pipe.read(65536)
                if not data:
                    break
                chunks.append(data)
        except ValueError:
            pass

    t1 = threading.Thread(target=drain, args=(proc.stdout, out_chunks))
    t2 = threading.Thread(target=drain, args=(proc.stderr, err_chunks))
    t1.start()
    t2.start()
    if stdin_cfg == subprocess.PIPE:
        try:
            proc.stdin.write(stdin_data)
            proc.stdin.close()
        except BrokenPipeError:
            pass

    deadline = start + timeout
    ru = None
    status = None
    if not hasattr(os, "wait4"):

        try:
            out_b, err_b = proc.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            proc.kill()
            raise RuntimeError(f"timeout running {' '.join(argv)}")
        elapsed_ms = (time.perf_counter() - start) * 1000.0
        return elapsed_ms, proc.returncode, out_b, err_b, None
    while time.perf_counter() < deadline:
        try:
            pid, status, ru = os.wait4(proc.pid, os.WNOHANG)
        except ChildProcessError:
            break
        if pid != 0:
            break
        time.sleep(0.005)
    if status is None:
        proc.kill()
        raise RuntimeError(f"timeout running {' '.join(argv)}")
    elapsed_ms = (time.perf_counter() - start) * 1000.0
    t1.join(timeout=5)
    t2.join(timeout=5)
    rc = os.waitstatus_to_exitcode(status) if hasattr(os, "waitstatus_to_exitcode") else proc.poll()
    rss_mb = None
    if ru is not None:
        rss_mb = ru.ru_maxrss / 1024.0
        if sys.platform == "darwin":
            rss_mb = ru.ru_maxrss / (1024.0 * 1024.0)
    return elapsed_ms, rc, b"".join(out_chunks), b"".join(err_chunks), rss_mb

def isolated_env(extra=None):
    """Env with HOME etc. pointed at a fresh temp dir."""
    home = tempfile.mkdtemp(prefix="zen-bench-home-")
    env = dict(os.environ)
    env["HOME"] = home
    env["USERPROFILE"] = home
    env["XDG_CONFIG_HOME"] = os.path.join(home, ".config")
    env["XDG_DATA_HOME"] = os.path.join(home, ".local", "share")

    for k in ("ZEN_API_KEY", "OPENAI_API_KEY", "ANTHROPIC_API_KEY"):
        env.pop(k, None)
    if extra:
        env.update(extra)
    return env, home

class _MockHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    request_count = 0

    def log_message(self, *a):
        pass

    def do_POST(self):
        if self.path != "/chat/completions":
            self.send_error(404)
            return
        length = int(self.headers.get("Content-Length", 0))
        self.rfile.read(length)
        body = json.dumps(MOCK_COMPLETION).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
        type(self).request_count += 1

def start_mock_server():
    server = ThreadingHTTPServer(("127.0.0.1", 0), _MockHandler)
    port = server.server_address[1]
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server, port

def measure_cold_startup(zen, runs):
    """Fresh copy per iteration -> page cache cold."""
    samples = []
    for _ in range(runs):
        with tempfile.TemporaryDirectory(prefix="zen-bench-cold-") as d:
            dest = os.path.join(d, "zen" + (".exe" if sys.platform == "win32" else ""))
            shutil.copy2(zen, dest)
            if os.name == "posix":
                os.chmod(dest, 0o755)
            env, _ = isolated_env()
            ms, rc, out, _, _ = run_timed([dest, "--version"], env, timeout=30)
            assert rc == 0, f"cold --version failed rc={rc}"
            assert out.strip(), "cold --version printed nothing"
            samples.append(ms)
    return samples

def measure_warm(zen, args, runs):
    samples, rss = [], []
    for _ in range(runs):
        env, _ = isolated_env()
        ms, rc, out, _, r = run_timed([zen] + args, env, timeout=30)
        assert rc == 0, f"{' '.join(args)} failed rc={rc}"
        samples.append(ms)
        if r is not None:
            rss.append(r)
    return samples, rss

def measure_repl_prompt(zen, runs):
    """Time from spawn until the REPL prints its first '> ' prompt."""
    samples = []
    for _ in range(runs):
        env, home = isolated_env()
        workdir = tempfile.mkdtemp(prefix="zen-bench-cwd-")
        start = time.perf_counter()
        proc = subprocess.Popen(
            [zen, "--bare"], env=env, cwd=workdir,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        )
        marker_at = None
        buf = b""
        deadline = start + REPL_TIMEOUT_S
        try:

            while time.perf_counter() < deadline:
                chunk = proc.stdout.read(1)
                if not chunk:
                    break
                buf += chunk
                if buf.endswith(PROMPT_MARKER) or PROMPT_MARKER in buf[-8:]:
                    marker_at = time.perf_counter()
                    break
            else:
                raise RuntimeError("REPL prompt not seen within timeout")
        finally:
            try:
                proc.stdin.write(b"/quit\n")
                proc.stdin.flush()
            except BrokenPipeError:
                pass
            try:
                proc.wait(timeout=15)
            except subprocess.TimeoutExpired:
                proc.kill()
        if marker_at is None:
            raise RuntimeError(f"REPL exited without printing a prompt (out={buf[-200:]!r})")
        samples.append((marker_at - start) * 1000.0)
        shutil.rmtree(workdir, ignore_errors=True)
        shutil.rmtree(home, ignore_errors=True)
    return samples

def measure_first_token(zen, runs):
    """Time from spawn until the first stdout byte of `zen -p` vs mock server."""
    server, port = start_mock_server()
    samples = []
    try:
        for _ in range(runs):
            env, home = isolated_env({
                "ZEN_BASE_URL": f"http://127.0.0.1:{port}",
                "ZEN_API_KEY": "bench",
                "ZEN_MODEL": "bench-model",
            })
            workdir = tempfile.mkdtemp(prefix="zen-bench-cwd-")
            start = time.perf_counter()
            proc = subprocess.Popen(
                [zen, "--bare", "-p", "Reply with exactly: hello"],
                env=env, cwd=workdir, stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            )
            first_at = None
            try:
                chunk = proc.stdout.read(1)
                if chunk:
                    first_at = time.perf_counter()
                proc.wait(timeout=TOKEN_TIMEOUT_S)
            except subprocess.TimeoutExpired:
                proc.kill()
                raise RuntimeError("zen -p timed out against mock server")
            finally:
                if proc.poll() is None:
                    proc.kill()
            if first_at is None:
                raise RuntimeError("zen -p produced no stdout against mock server")
            if proc.returncode != 0:
                raise RuntimeError(f"zen -p exited rc={proc.returncode}")
            samples.append((first_at - start) * 1000.0)
            shutil.rmtree(workdir, ignore_errors=True)
            shutil.rmtree(home, ignore_errors=True)
    finally:
        server.shutdown()
    return samples, _MockHandler.request_count

def run_all(zen, runs, cold_runs):
    results = {"binary": zen, "runs": runs, "cold_runs": cold_runs,
               "platform": sys.platform}

    print(f"[1/5] cold startup (--version, fresh copy) x{cold_runs} ...", flush=True)
    results["startup_cold_ms"] = stats(measure_cold_startup(zen, cold_runs))

    print(f"[2/5] warm startup (--version) x{runs} ...", flush=True)
    s, rss = measure_warm(zen, ["--version"], runs)
    results["startup_warm_ms"] = stats(s)
    if rss:
        results["peak_rss_mb"] = stats(rss)

    print(f"[3/5] warm doctor x{runs} ...", flush=True)
    s, _ = measure_warm(zen, ["doctor"], runs)
    results["doctor_warm_ms"] = stats(s)

    print(f"[4/5] REPL time-to-first-prompt x{runs} ...", flush=True)
    results["repl_prompt_ms"] = stats(measure_repl_prompt(zen, runs))

    print(f"[5/5] headless time-to-first-token (mock server) x{runs} ...", flush=True)
    s, reqs = measure_first_token(zen, runs)
    results["first_token_ms"] = stats(s)
    results["mock_requests_total"] = reqs

    return results

def check_stability(results):
    """Fail (exit 2) if any warm measurement has excessive variance."""
    bad = []
    for key in ("startup_warm_ms", "doctor_warm_ms", "repl_prompt_ms", "first_token_ms"):
        cv = results[key]["cv"]
        if cv > CV_LIMIT:
            bad.append(f"{key}: cv={cv:.2f} > {CV_LIMIT}")
    return bad

def check_thresholds(results, thresholds):
    """Fail (exit 1) if any median exceeds its threshold."""
    bad = []
    for key, limit in thresholds.items():
        if key in results and results[key]["median"] > limit:
            bad.append(
                f"{key}: median={results[key]['median']:.1f}ms > threshold={limit}ms"
            )
    return bad

def markdown_table(results):
    lines = [
        "| Measurement | Median | p95 | Mean | CV |",
        "|---|---|---|---|---|",
    ]
    for key, label in [
        ("startup_cold_ms", "Startup (cold, fresh copy)"),
        ("startup_warm_ms", "Startup (warm, `--version`)"),
        ("doctor_warm_ms", "`zen doctor` (warm)"),
        ("repl_prompt_ms", "REPL time-to-first-prompt"),
        ("first_token_ms", "Headless time-to-first-token (mock)"),
    ]:
        r = results[key]
        lines.append(
            f"| {label} | {r['median']:.0f} ms | {r['p95']:.0f} ms | "
            f"{r['mean']:.0f} ms | {r['cv']:.2f} |"
        )
    if "peak_rss_mb" in results:
        r = results["peak_rss_mb"]
        lines.append(
            f"| Peak RSS (`--version`, warm) | {r['median']:.1f} MB | "
            f"{r['p95']:.1f} MB | {r['mean']:.1f} MB | {r['cv']:.2f} |"
        )
    return "\n".join(lines)

def self_test(zen):
    """Quick validation: stats math, mock server shape, one smoke timing each."""
    assert stats([10, 20, 30, 40])["median"] == 25
    assert abs(stats([1, 1, 1, 1])["cv"]) < 1e-9
    server, port = start_mock_server()
    try:
        import urllib.request
        req = urllib.request.Request(
            f"http://127.0.0.1:{port}/chat/completions",
            data=b"{}", headers={"Content-Type": "application/json"}, method="POST",
        )
        with urllib.request.urlopen(req, timeout=10) as resp:
            body = json.loads(resp.read())
        assert body["choices"][0]["message"]["content"] == "hello"
        assert body["choices"][0]["finish_reason"] == "stop"
    finally:
        server.shutdown()
    env, _ = isolated_env()
    ms, rc, out, _, _ = run_timed([zen, "--version"], env, timeout=30)
    assert rc == 0 and out.strip() and ms > 0
    ms, rc, out, _, _ = run_timed([zen, "doctor"], env, timeout=30)
    assert rc == 0 and b"zen doctor" in out
    r = measure_repl_prompt(zen, 1)
    assert r[0] > 0
    s, reqs = measure_first_token(zen, 1)
    assert s[0] > 0 and reqs >= 1
    print("self-test: OK (stats, mock server, --version, doctor, REPL, -p)")

def main():
    ap = argparse.ArgumentParser(description="Cold-start benchmarks for zen")
    ap.add_argument("--zen", default="./target/release/zen", help="path to zen binary")
    ap.add_argument("--runs", type=int, default=15, help="warm iterations")
    ap.add_argument("--cold-runs", type=int, default=5, help="cold iterations")
    ap.add_argument("--out-dir", default="bench-results", help="results directory")
    ap.add_argument("--thresholds", default=None, help="JSON thresholds file (CI guard)")
    ap.add_argument("--self-test", action="store_true", help="quick validation only")
    args = ap.parse_args()

    zen = os.path.abspath(args.zen)
    if not os.path.isfile(zen):
        print(f"error: zen binary not found: {zen}", file=sys.stderr)
        return 1
    if args.self_test:
        self_test(zen)
        return 0

    results = run_all(zen, args.runs, args.cold_runs)

    os.makedirs(args.out_dir, exist_ok=True)
    with open(os.path.join(args.out_dir, "results.json"), "w") as f:
        json.dump(results, f, indent=2)
    with open(os.path.join(args.out_dir, "results.md"), "w") as f:
        f.write("# Cold-start benchmark results\n\n")
        f.write(f"Binary: `{results['binary']}` - platform `{results['platform']}`\n\n")
        f.write(markdown_table(results) + "\n")

    print("\n" + markdown_table(results))

    unstable = check_stability(results)
    if unstable:
        print("UNSTABLE: " + "; ".join(unstable), file=sys.stderr)
        return 2

    if args.thresholds:
        with open(args.thresholds) as f:
            thresholds = json.load(f)
        regressed = check_thresholds(results, thresholds)
        if regressed:
            print("REGRESSION: " + "; ".join(regressed), file=sys.stderr)
            return 1
        print("thresholds: all held")

    print(f"\nresults written to {args.out_dir}/")
    return 0

if __name__ == "__main__":
    sys.exit(main())
