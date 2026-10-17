# PLAN p0b — spill, background jobs + tool timeouts, session event log

Branch `feat/p0b` from `origin/main` (ef3bca5). One commit + push per item, in order.
Design reference: dsh `packages/spill`, `ctx.jobs` + `timeout-policy`, "model-visible means logged"
(`/workspace/work/research/deepseek-harness.md`). No new dependencies.

Gate after every item: `cargo test --workspace --no-fail-fast` and
`cargo clippy --workspace --all-targets -- -D warnings`, raw output read, 0 failures.

## 1. Spill long tool output (Bash)
- `ToolContext.session_dir: Option<PathBuf>` (zen-core). CLI sets it to the transcript path minus
  `.jsonl` (`~/.zen/projects/<enc>/<session-id>/`); unset → `<tmp>/zen-<session-id>/`.
- `zen_tools::spill_output(text, max_chars, ctx)`: under the cap returns text unchanged; over it writes
  the full text to `<session_dir>/tool-output/<tool-use-id>.txt` and returns head + tail + path +
  total bytes/lines + a Read/Grep hint. Write failure falls back to `truncate_middle` and says so.
- Bash uses it instead of `truncate_middle`.
- Proof: unit tests (short output untouched; long output → file holds the exact full text, reply has
  head, tail, path, byte and line counts; fallback dir when no session dir) + Bash test with
  `seq 1 20000`.

## 2. Background Bash jobs + per-tool timeouts
- Bash `background: true` (alias of the old fire-and-forget `run_in_background`) starts a tracked job:
  output (stdout+stderr merged) goes to `<session_dir>/jobs/<job-id>.log`, returns the job id.
  Unix: own process group so a kill takes the whole tree; Windows: `Child::kill`.
- New tools `JobOutput {job_id, wait_ms?}` (new output since last read + status; long chunks spill
  per item 1) and `JobKill {job_id}`.
- Settings `toolTimeouts` (ms per tool name, `"default"` for the rest, 0 = none). Defaults:
  Bash 120000 (its default when the call gives no `timeout_ms`), other tools 300000, Task/JobOutput
  exempt. The agent wraps non-Bash calls in the timeout and returns a clear error.
- Proof: tests — job id returned, incremental reads see only new output, exit status reported,
  kill stops a `sleep` job (and its child on Unix), unknown id errors; agent test where a slow tool
  hits a 50 ms timeout; settings parse test. Windows: CI build job.

## 3. Session file = complete event log
- `RecordingBackend` (zen-cli) wraps the LLM client when a transcript exists and appends to the same
  session file: `tools` record (defs + FNV-1a hash, once per new hash), `request` record per model
  call (`v`, `seq`, model, max_tokens, tools_hash, `base` seq + `keep` prefix count + `append`
  messages, hash of the exact serialized body), `response`/`response_error` record after it.
  Field names avoid `message`, so `load_messages` and every old reader ignore them.
- `rebuild_request(path, seq)` / `rebuild_last_request(path)` reconstruct the request body.
- Proof: test runs a real `Agent` (scripted backend capturing the serialized body it receives, a real
  tool, two turns, compaction-free) through the recorder, then rebuilds the last request from the
  file and compares bytes; an old-format session (no request records) still loads the same history.

## Status
- [x] 1 (452 pass, clippy clean)  - [x] 2 (461 pass, clippy clean)  - [ ] 3  - [ ] CI green on feat/p0b

## Failures log
(none yet)
