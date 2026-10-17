# PLAN-sync — CLI side of the Desktop sync plan (items 11–14)

Branch `feat/desktop-sync` (worktree /workspace/work/zc-sync) from main 3a87c47.
Rules: minimal change, no new deps, no repo-wide `cargo fmt`, no secrets printed.
Same step fails twice → stop, note here, report.

## Steps (each with its proof)

1. **#11 `AgentEvent` non_exhaustive.** Add `#[non_exhaustive]` to `zen_core::AgentEvent` only
   (the one enum Desktop matches to build UI events; `ShellKind`/`PermissionDecision`/etc. are not
   part of the event API). zen-cli's own matches get wildcard arms. New `CHANGELOG.md`
   (Keep a Changelog) with the downstream note.
   Proof: workspace builds; clippy clean.
2. **#12 stream-json contract.** `docs/stream-json-v1.md` written from `output.rs`
   (`StreamJsonSink`, `result_json`) and `agent_run.rs` only. Fixtures in
   `docs/fixtures/stream-json/*.ndjson` produced by the real binary against the mock LLM in
   `crates/zen-cli/tests/common`: plain text, tool call + result, sub-agent
   (`parent_tool_use_id`), api retry, permission denied, truncation continuation note.
   Volatile values (uuid, cwd, durations, mock port) are normalised.
   Test `crates/zen-cli/tests/stream_json_contract.rs` regenerates each stream and fails on diff
   (`ZEN_UPDATE_FIXTURES=1` rewrites), and checks every record type/subtype/top-level key seen in
   the fixtures is named in the doc.
   Proof: test passes; flipping one fixture byte makes it fail.
3. **#13 Windows Bash.** Root cause from CI logs (see Findings). Fix if small.
   Proof: new unit tests for the path logic (host-independent); Windows CI job log after the PR.
4. **Verify.** `cargo test --workspace` (baseline 484, expect more) and
   `cargo clippy --all-targets -- -D warnings`.
5. **PR → CI green → backup tag `backup/main-pre-sync-20261017` at 3a87c47 → merge.**
6. **#14 release.** No version tags exist yet; workspace version 0.1.0 → annotated `v0.1.0` on the
   merge commit + GitHub release (notes: PR #1 MCP cap, PR #2 durable tool results, PR #3
   continue-on-truncation, this PR). Label `desktop` created if missing.

## Findings

### #13 Bash on Windows (CI run 39008328569, job 117325407525, main 3a87c47)
- Failing: `zen-tools builtin::tests::bash_*` (4), hooks e2e (6), and every e2e that runs a
  hook/bash; all print `Windows Subsystem for Linux has no installed distributions` (UTF-16).
- Root cause: `zen_core::pick_shell` returns the *bare name* `bash` after checking PATH, and the
  Bash tool / jobs / hooks / REPL `!cmd` spawn `bash` by name. Windows process creation (Rust std
  included) searches the system directory *before* PATH, so `bash` resolves to
  `C:\Windows\System32\bash.exe` — the WSL launcher — even when Git Bash is on PATH. With no WSL
  distro installed (CI runners, most Desktop users) every command fails with exit 1.
- Fix (small): resolve the shell to the **full path** found on PATH, skipping the WSL launcher in
  `%SystemRoot%`, with a fallback to Git for Windows' `%ProgramFiles%\Git\bin\bash.exe`; hooks and
  REPL `!cmd` use `resolve_shell` instead of a hard-coded `bash`. Unix unchanged.
- Verified on PR #4 CI (run 39009809675, Windows job 117329875688): WSL message gone, Windows
  native tests 416 passed/53 failed → 448/22. `syntax_guard` still spawned bare `bash` (same
  root cause) → now uses `resolve_shell` too.
- Remaining Windows failures are test-side, not the WSL root cause: hook unit tests interpolate
  `C:\...` paths into bash commands (backslashes eaten → file not found), `bash_cd_persists`
  compares an 8.3 short path with a `\\?\` canonical path, session lookup/resume, MCP e2e, ZEN.md
  loading. Tracked in a `desktop` issue.
- Other Windows failures in that job (session path parsing in `session.rs:535`, `lib.rs:242`,
  MCP e2e) are separate and not in scope; left for a follow-up issue if they persist.

## Status
- [x] 1 #11 non_exhaustive + CHANGELOG — builds, clippy clean.
- [x] 2 #12 doc + 6 fixtures + contract test — passes twice in a row; a one-byte fixture edit fails it.
- [x] 3 #13 Windows Bash fix + `windows_bash_skips_wsl_launcher` unit test (real proof = Windows CI job).
- [x] 4 `cargo test --workspace` 492 passed / 0 failed (baseline 484 + 7 contract + 1 shell);
      `cargo clippy --all-targets -- -D warnings` clean (local, aarch64 Linux).
- [ ] 5 PR, CI, backup tag, merge
- [ ] 6 tag v0.1.0 + release; label `desktop` created (done).
