# Terminal-Bench — ZenCode Benchmark

**Status:** Infrastructure complete and pipeline-validated; live model runs
blocked by worker resource constraints (see §Results).

## Why this benchmark

ZenCode's core differentiator (D2) is a **harness engineered to make weak
models succeed** — verify loop, prompt budget, RateGate, repair of malformed
tool calls, and (in progress) advisor pattern, result pruning, and eco mode.
The claim "free models can do real work with ZenCode" needs public,
reproducible evidence. Terminal-Bench 2.0 is the standard benchmark for
terminal agents (89 tasks, Docker-isolated, outcome-verified), used by
virtually all frontier labs.

The roadmap target: *"Nobody publishes TB scores for free models. Even 40–50%
at $0 is a story."* This page documents our methodology and results honestly —
including the limitations.

## Methodology

- **Dataset:** Terminal-Bench 2.0 (`terminal-bench@2.0`), tasks used unmodified
  from https://github.com/laude-institute/terminal-bench-2.
- **Runner:** `tools/terminal-bench/tb_runner.py` (this repo). For each task:
  1. Pull the task's prebuilt Docker image.
  2. Start a container with `--add-host=host.docker.internal:host-gateway`.
  3. Copy the `zen` binary in; run it headless (`zen -p`) with the task's
     `instruction.md`.
  4. Model: free, via an OpenAI-compatible endpoint (no API key, **$0**).
  5. Verify with the task's own `tests/test.sh`; pass = `reward.txt` == 1.
  6. Per-task agent timeout capped at 20 minutes. Single run, no retries.
- **Subset:** 8 tasks (4 easy + 4 medium, diverse categories). This validates
  the pipeline; it is **not** a full 89-task leaderboard score.

## Results (2026-10-17 run)

**Pipeline: VALIDATED.** The full loop works end-to-end on the worker:
Docker pull → container start → zen binary injection → instruction delivery →
`zen -p` invocation → test.sh verification → reward.txt parsing → report
generation. Container→host model connectivity was verified (`PORT-OK`).

**Model scores: BLOCKED.** The shared worker (4 vCPU / 8 GB RAM, ~17 concurrent
agent builds) had ~139 MB free RAM; the local Ollama model server was
OOM-killed mid-run, so agent calls failed with network errors and the tasks
scored 0/3 before the run was stopped to protect the shared machine.
This is an **infrastructure constraint, not a model result** — no score is
claimed.

| Task | Difficulty | Result | Notes |
|---|---|---|---|
| overfull-hbox | easy | infra-error | Ollama OOM-killed; agent could not reach model |
| fix-git | easy | infra-error | same |
| cobol-modernization | easy | infra-error | same |
| 5 remaining | | not run | run stopped to protect shared worker |

**Next step:** rerun on a machine with headroom (or against a hosted free
endpoint, e.g. Experiential Labs / OpenRouter `:free` via `ZEN_API_KEY`)
with:

```sh
python3 tools/terminal-bench/tb_runner.py \
  --repo ./terminal-bench-2 \
  --zen ./target/release/zen \
  --subset tools/terminal-bench/subset.json \
  --out /tmp/tb-results \
  --base-url https://api.experientiallabs.ai/v1 \
  --model deepseek-v4.1-flash-uncensored
```

(`--base-url` is seen from *inside* the task container; use a public URL,
not localhost, unless the endpoint runs on the Docker host with
`--add-host=host.docker.internal:host-gateway`, which the runner sets.)

## Limitations (read before citing)

1. **No score is claimed yet.** See above.
2. **Subset, not the full benchmark.** 8/89 tasks cannot be compared to
   published 89-task scores (e.g. Codex CLI 82% on TB 2.0 with GPT-5.5).
3. **Single run.** No retries, no error bars when scores land.
4. **Shared worker.** Durations are not clean performance numbers.

## Reproduce

```sh
git clone --depth 1 https://github.com/laude-institute/terminal-bench-2
python3 tools/terminal-bench/tb_runner.py \
  --repo ./terminal-bench-2 \
  --zen ./target/release/zen \
  --subset tools/terminal-bench/subset.json \
  --out /tmp/tb-results
```

See `tools/terminal-bench/README.md` for endpoint configuration.

## License note

Terminal-Bench 2.0 tasks are © Laude Institute (see the task repo for the
license). The canary string in each task is preserved; task data is never
used for training.
