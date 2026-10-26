# PLAN-dtr - durable tool results (branch `feat/durable-tool-results`)

Problem: the transcript gets the turn's messages only when the turn ends (`persist_turn`). A turn
that dies mid-tool (crash, SIGKILL, timeout, Ctrl-C) loses every tool result it already had, so
`--resume` / `/resume` start from the previous turn. The p0b event log (`request`/`response`
records) already holds what the model saw, but not the tool results.

Design (minimal, no new deps):
- `zen_core::ToolJournal` (2 methods) + `Agent.journal: Option<Arc<dyn ToolJournal>>` (None by
  default, so sub-agents and tests are unchanged). The main agent journals:
  - `turn_assistant`: the assistant message as pushed into history (after text-call repair);
  - `tool_result` stage `raw`: the moment `run_tool` returns, inside the spawned task for
    parallel read-only batches, right after the call for sequential tools;
  - `tool_result` stage `final`: the content the model will see, only when hooks/verify/guard
    changed it, and for calls that never ran (denied, skipped, stopped).
- `eventlog::Recorder` implements it; each record is one `write_all` of a full line under the
  recorder lock (no interleaving between parallel tools), then flushed/closed. The records have
  no top-level `message`, so `load_messages`, `rebuild_requests` and older binaries skip them:
  completed sessions replay byte-identically and old session files load unchanged.
- `eventlog::resume_history(path)`: committed history (`load_history`) plus, when the records after
  the last committed message hold an interrupted turn, that turn rebuilt from the log: the last
  main-chain request (its messages, system prompt dropped) + the journaled assistant message +
  its tool results (final, else raw, else an explicit "interrupted" tool message so the history
  stays valid). If a later main-chain request exists it already contains them and is used as is.
  Recovered messages are written to the transcript at resume as normal message records.
- Wire: headless (`agent_run`) and REPL set `agent.journal`; resume/fork and REPL `/resume` use
  `resume_history`. Background job output is already durable (job log file in the session dir);
  the Bash start result and JobOutput reads are tool results like any other.

Steps (proof per step):
1. core trait + agent field + journal calls - `cargo build`; agent unit tests unchanged.
2. Recorder impl + `resume_history` - unit test: 2 Read calls, journal 1 result, kill before the
   2nd → `resume_history` has the 1st result + interrupted marker for the 2nd; parallel batch test
   records raw results; old-format file → `resume_history == load_history`, nothing recovered.
3. Wiring headless/REPL - integration test with the binary: tool sleeps, process SIGKILLed after
   the first tool finished, `--resume` → the request the server receives contains tool result 1.
4. Existing `p0b_3_*` replay tests stay byte-identical; `cargo test` all green (baseline 465),
   `cargo clippy --all-targets -- -D warnings` clean.
5. PR (template), CI green, backup tag `backup/main-pre-dtr-20261017`, merge.

Status / failures:
- Steps 1-4 done. `cargo test`: 468 passed, 0 failed (465 + 3 new: `dtr_killed_after_one_of_two_sequential_tools_*`,
  `dtr_killed_mid_parallel_batch_*` in eventlog.rs, `dtr_killed_mid_turn_resume_keeps_finished_tool_result`
  in tests/sessions.rs, SIGKILL against the real binary). Existing `p0b_3_*` replay/old-format tests extended
  and green. The parallel test fails if the in-task raw journal write is removed (checked).
  `cargo clippy --all-targets -- -D warnings`: clean. `cargo fmt --check` already differs on main (CI step is
  continue-on-error); not reformatted to keep the diff minimal.
- Toolchain note: the sandbox lost the `ziglang` python module used by zc-p0/zcc; reinstalled to /tmp/zigpy
  and used wrappers /workspace/work/zc-dtr-{zcc,zar} + env /workspace/work/zc-dtr-env.sh.
