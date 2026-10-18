# Terminal-Bench runner for ZenCode

Runs a subset of [Terminal-Bench 2.0](https://github.com/laude-institute/terminal-bench-2)
tasks against the `zen` CLI using a **free model** ($0), producing citable
benchmark numbers for ZenCode's "harness optimized for weak models" story.

## Prerequisites

- Docker (task containers)
- Python 3.11+ with `tomli` (only needed on Python < 3.11; 3.11+ has `tomllib`)
- A `zen` binary
- An OpenAI-compatible model endpoint. Default is local Ollama (`$0`):
  ```sh
  ollama serve          # bind 0.0.0.0 if tasks run in Docker
  ollama pull qwen3:4b
  ```

## Quick start

```sh
# 1. Get the tasks
git clone --depth 1 https://github.com/laude-institute/terminal-bench-2

# 2. Run the subset (8 tasks, ~1h with a local 4B model)
python3 tb_runner.py \
  --repo ./terminal-bench-2 \
  --zen ./target/release/zen \
  --subset subset.json \
  --out results/

# 3. Results
cat results/report.md        # human-readable table
cat results/results.json     # machine-readable
```

## How it works

For each task in the subset:

1. `docker pull` the task's prebuilt Terminal-Bench 2.0 image.
2. Start a container with `--add-host=host.docker.internal:host-gateway`
   so the agent can reach the model endpoint on the host.
3. `docker cp` the `zen` binary into the container.
4. Run `zen --base-url <url> --model <model> -p "$(cat instruction.md)"`
   headless, with a per-task timeout (capped at 20 min).
5. Copy the task's `tests/` into the container and run `tests/test.sh`;
   pass = `/logs/verifier/reward.txt` contains `1`.
6. Remove the container. Write per-task agent + verifier logs to `out/logs/`.

## Options

| Flag | Default | Meaning |
|---|---|---|
| `--model` | `qwen3:4b` | Model name for the endpoint |
| `--base-url` | `http://host.docker.internal:11434/v1` | OpenAI-compatible base URL (as seen *from inside* the container) |
| `--agent-timeout` | per-task `task.toml` | Override agent timeout, seconds (capped at 1200) |

## Tests

```sh
python3 test_tb_runner.py -v   # no Docker needed
```

## Notes

- The subset (`subset.json`, 8 tasks: 4 easy + 4 medium) is a **pipeline
  validation**, not a leaderboard score. See `docs/benchmark-terminal-bench.md`.
- Terminal-Bench 2.0 is © Laude Institute; tasks are used unmodified.
  The canary string in each task is preserved; do not train on task data.
