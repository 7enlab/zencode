# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Added
- One-command install for Linux, macOS, and Windows (install.sh, install.ps1) with
  `zen update` and `zen update --check` (PR #4): clones the repo or fast-forwards an
  existing checkout, builds the CLI crate in release mode, copies `zen` (`zen.exe` on
  Windows) into the user bin directory, and adds that directory to the user PATH.
  No admin rights, no toolchain install.
- (P0-3) Per-turn checkpoints, no git needed: before Write/Edit/MultiEdit first change
  a file in a REPL turn, its old content is saved in the session directory. `/undo`
  restores the last turn that changed files (new files are deleted) and notes it in the
  history; repeat to walk back. It refuses when a file was changed outside zen since
  the turn (`/undo --force` overrides). Files over 10 MB are not saved (the tool result
  says so). Bash changes are not undone.
- `/diff --turn` shows the last file-changing turn as a unified diff (works outside git).
- stream-json: every record now carries a top-level `v` (schema version, currently `1`),
  so downstream consumers can detect schema drift and gate on breaking changes (issue #19).
- stream-json: `system`/`tool_progress` heartbeat records, emitted every 5 seconds while a
  tool call is still running (`tool_use_id`, `tool_name`, `elapsed_ms`), so UIs can
  distinguish "tool still working" from stalled (issue #21).
- docs: `--include-partial-messages` consumer contract in `docs/stream-json-v1.md` -
  deltas are provisional, the following full `assistant` record is authoritative; UIs must
  replace (not append) accumulated deltas (issue #20).
- RateGate (roadmap g3, atzenn/zencode#10): per-provider daily request quota for `:free`
  LLM tiers, implemented as an `LlmBackend` wrapper in `zen-agent` so it composes with the
  existing retry/fallback logic. Counts every `chat` call per provider host (including
  failures) in `~/.zen/rategate.json`, rolling over each UTC day; local gateways are never
  gated. When a provider's quota is exhausted the gate fails fast and the agent loop's
  model-fallback moves to the next configured model automatically. Configurable via
  settings `rateGate: {"enabled", "dailyLimit", "limits"}` (defaults: enabled, 50/day). Only
  `:free` model ids are gated, and only on openrouter.ai or hosts listed in `limits`; paid
  models and the default gateway are never throttled.
  Quota status is shown in `zen doctor` and via the new REPL `/quota` command.

### Changed
- (P0-4 / P0-5) `/quota` on an OpenRouter base URL shows the real free-model counter from
  `GET /api/v1/key` (used / limit / remaining, UTC day) and the key's credit limit, then the
  local RateGate counts. The endpoint is called only when you run `/quota`.
- 429: an `X-RateLimit-Reset` instant is honoured when there is no `Retry-After`, and a server
  wait is honoured in full; a wait longer than what is left of `retry.maxWaitSecs` stops at once
  with the reason instead of retrying early.
- 402: never retried, with OpenRouter's reason in the error (`weight_exceeds_budget`, key limit,
  credits); only the in-flight budget 402 with `Retry-After` is retried.
- `finish_reason: "content_filter"` ends the turn with a clear "blocked by the provider's content
  filter" error after one request (no empty-reply requery); a `message.refusal` is kept as the
  reply text.
- A provider error inside a 200 body (`finish_reason: "error"`, OpenRouter's mid-stream error) is
  retried like its status: 429 within the wait budget, other errors at most 3 times, then the
  turn ends with the code and `error_type` in the error.
- (P0-1 / P0-2) Write, Edit and MultiEdit write atomically: temp file in the same directory,
  fsync, rename. A kill or full disk mid-write no longer leaves a truncated file; file mode is
  kept; a symlink is written through to its target instead of being replaced.
- Model requests: timeout is configurable with `api.timeoutSecs` / `ZEN_REQUEST_TIMEOUT`
  (default 300 s, 900 s for local endpoints, `0` = off; was a fixed 120 s), plus an optional
  idle timeout `api.idleTimeoutSecs` / `ZEN_IDLE_TIMEOUT` and a 10 s connect timeout. Timeouts
  are retried and the time they take counts against `retry.maxWaitSecs`.
- Errors from the provider include its request id (`[request id: ...]`, from `x-request-id`,
  `request-id`, etc.), also stored as `request_id` on `response_error` session records.
- stream-json: records are now flushed to stdout one line at a time as they are emitted,
  instead of all at once when the run ends. Record shapes are unchanged (`v` stays `1`).

### Fixed
- `encode_cwd`: also replace `\` and `:` when encoding the project-dir name, so Windows
  paths like `C:\Users\...` become a single flat directory instead of nested dirs.
  Fixes session lookup/resume failures on Windows (issue #6).
- Folder trust: in an untrusted folder, project baseUrl and apiKeyEnv are ignored
  (user-level values apply) so a repo cannot redirect requests carrying your API key (PR #7).

## 0.1.0 - 2026-10-17

Note: no v0.1.0 tag exists on this repo; the entry below records the 0.1.0 state
as of 2026-10-17.

### Added
- `docs/stream-json-v1.md`: the `zen -p --output-format stream-json` contract, with sample
  streams in `docs/fixtures/stream-json/` produced by the real binary. A test regenerates the
  fixtures and fails on any difference, so the doc cannot drift from the output.
- Continue on truncation: a reply cut off by the output token limit is continued and stitched;
  a cut-off tool call is never run; `maxContinuations` (default 3) caps it with a clear note
  (PR #3).
- Durable tool results: every tool result is appended to the session file the moment it finishes, so a turn that dies
  mid-tool resumes with the results it already had (PR #2).
- MCP output cap: oversized MCP tool results are capped and spilled to a file, like Bash output (PR #1).
- Folder trust: in an untrusted folder, project hooks, MCP servers, allow rules,
  `additionalDirectories`, env and verify do not load. The REPL asks once and saves the answer in
  `~/.zen/trusted-folders.json`; `-p` is untrusted unless `--trust-folder` / `ZEN_TRUST_FOLDER=1`.
  stream-json `system/init` gains `folder_trusted` (bool, add-only) (PR #5).

### Changed
- **Breaking for exhaustive matches:** `zen_core::AgentEvent` is now `#[non_exhaustive]`.
  Downstream code that matches on it must add a wildcard arm (`_ => {}`). New event kinds can
  then be added in a minor release without breaking your build.

### Fixed
- Windows: the Bash tool, background jobs, hooks, the REPL `!cmd` and the shell syntax guard no longer start the WSL
  launcher (`C:\Windows\System32\bash.exe`, "Windows Subsystem for Linux has no installed
  distributions") instead of Git Bash. The shell is now resolved to the full path of the first
  `bash` on PATH outside `%SystemRoot%`, falling back to `%ProgramFiles%\Git\bin\bash.exe`.
