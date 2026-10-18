# Cold-Start Benchmarks

**Status:** implemented (`scripts/bench_cold_start.py`), first results below.
**Roadmap:** v0.8.0 "Trust" item #13 — *"Cold-start & memory benchmarks, published."*

Fast startup is a structural advantage of a native Rust binary over Electron/Node-based
tools, and it compounds: every `zen -p` invocation in a script, hook, or CI job pays
startup cost. This document defines the methodology, publishes measured numbers, and
describes the CI regression guard.

## What is measured

| Measurement | Command | What it captures |
|---|---|---|
| Startup (cold) | `zen --version` on a **fresh copy** of the binary | Process spawn + dynamic linking + clap parse with a cold page cache (new inode per run) |
| Startup (warm) | `zen --version`, same binary, repeated | Steady-state CLI overhead: parse + print + exit |
| `zen doctor` (warm) | `zen doctor`, repeated | Config resolution path (settings files, env, trust) without network |
| REPL time-to-first-prompt | `zen --bare`, stdin `/quit` | Time from spawn until the first `> ` prompt is printed (banner + agent assembly) |
| Headless time-to-first-token | `zen --bare -p "…"` vs a local mock OpenAI-compatible server | Full headless path: config → session → system prompt → HTTP round-trip → first stdout byte |
| Peak RSS | sampled during warm `--version` runs | Memory footprint at startup (POSIX `ru_maxrss`) |

Environment isolation: `HOME`/`USERPROFILE`/`XDG_*` point at a fresh temp dir per
measurement group, so benchmarks never touch real user state. No API keys are used;
the token test runs against a `127.0.0.1` mock server returning a fixed completion.

Statistics per measurement: median, p95, mean, standard deviation, coefficient of
variation (CV). A run is **unstable** (exit 2) if any warm measurement has CV > 0.30.

## How to run

```sh
# Build the release binary first
cargo build --release -p zen-cli

# Full benchmark (default 15 warm runs, 5 cold runs)
python3 scripts/bench_cold_start.py --zen ./target/release/zen

# Quick validation of the harness itself
python3 scripts/bench_cold_start.py --zen ./target/release/zen --self-test

# CI regression guard (fails if any median exceeds its threshold)
python3 scripts/bench_cold_start.py --zen ./target/release/zen \
    --thresholds benches/cold_start_thresholds.json
```

Results are written to `bench-results/results.json` (raw) and `bench-results/results.md`
(markdown table).

## Results

Measured on the project's Linux build worker (4 vCPU / 8 GB, `x86_64-unknown-linux-gnu`,
release binary, `zen 0.1.0`). Primary run: 15 warm iterations, 5 cold iterations —
all warm measurements stable (CV < 0.20). A second confirmation run on the same
heavily-loaded shared machine reproduced the same medians within ~4 ms; peak RSS
was rock-stable (CV 0.00) across both runs.

| Measurement | Median | p95 | Mean | CV |
|---|---|---|---|---|
| Startup (cold, fresh copy) | 1 ms | 2 ms | 2 ms | 0.20 |
| Startup (warm, `--version`) | 1 ms | 1 ms | 1 ms | 0.08 |
| `zen doctor` (warm) | 1 ms | 1 ms | 1 ms | 0.06 |
| REPL time-to-first-prompt | 3 ms | 4 ms | 3 ms | 0.03 |
| Headless time-to-first-token (mock) | 4 ms | 5 ms | 4 ms | 0.05 |
| Peak RSS (`--version`, warm) | 20.3 MB | 20.3 MB | 20.3 MB | 0.00 |

Headline: **`zen --version` answers in ~1 ms warm (~1–6 ms cold) with a 20 MB
footprint; the REPL is interactive in ~3 ms; a headless `-p` turn reaches its
first token in ~4 ms** (mock server — harness overhead only, no model latency).

## Comparison points

Published cold-start numbers for competing CLIs (for context — measure with the same
script methodology before citing side-by-side):

| Tool | Runtime | Reported cold start |
|---|---|---|
| ZenCode `zen` | Native Rust binary | see table above |
| Codex CLI | Native Rust binary | not measured here yet |
| Claude Code | Node.js | not measured here yet |
| Gemini CLI | Node.js | not measured here yet |
| OpenCode | Bun/Node (desktop: Electron) | not measured here yet |

The honest claim we can make today: a native binary has no interpreter/VM warmup, so
`--version`-class invocations should stay in the low hundreds of milliseconds. The
numbers above are the receipts; update the comparison rows only with locally measured
data using this same methodology.

## CI regression guard

`benches/cold-start.yml` is a GitHub Actions workflow: copy it to
`.github/workflows/cold-start.yml` to enable. (It lives under `benches/` in this
PR because the automation token used to open the PR lacks the `workflow` scope
required to push workflow files directly.) Once enabled, it builds the release
binary on `ubuntu-latest` and runs the benchmark with
`--thresholds benches/cold_start_thresholds.json`. The job fails if:

- any median exceeds its threshold (exit 1) — catches real regressions;
- any warm measurement is unstable, CV > 0.30 (exit 2) — catches flaky environments.

Thresholds are intentionally generous (multiples of measured medians) because CI
runners vary. Tighten them only with data from repeated CI runs. If a legitimate
change moves startup cost (e.g. new global initialization), update the thresholds in
the same PR and say why in the PR description.

## Known limitations

- Cold-start via fresh-copy approximates a cold page cache; a true cold boot (dropped
  caches) needs root and is not attempted.
- `first_token_ms` includes one localhost HTTP round-trip to the mock server; it
  measures harness overhead, not model latency.
- Peak RSS on macOS uses `ru_maxrss` in bytes (handled); on Windows RSS is not
  collected (reported as absent).
- Numbers are machine-specific. Always compare runs on the same machine.
