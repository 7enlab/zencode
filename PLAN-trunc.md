# PLAN-trunc — continue on truncation (branch `feat/continue-on-truncation`)

Problem: when a reply stops because it hit the output token limit (`finish_reason: "length"` on
OpenAI/OpenRouter; some gateways pass the native `max_tokens` / `MAX_TOKENS` /
`max_output_tokens`), the loop treats it like a normal reply: a cut-off text answer ends the turn,
and a half-written tool call goes to repair, whose JSON balancing can turn a truncated object into
a "valid" call and execute half of it.

Design (minimal, no new deps, all in `zen-agent/src/agent.rs` + one settings key):
- `is_truncated(finish_reason)`: case-insensitive `length | max_tokens | max_output_tokens`.
- Checked right after the model call, before text-call repair and the empty-reply requery.
- Truncated text reply (no tool calls): keep the partial assistant message in history (emitted +
  journaled like any reply), append a user message `TRUNCATION_CONTINUE_NUDGE` ("continue exactly
  where you stopped, don't repeat"), requery. The pieces are stitched: the turn's `result` is
  partial text + continuation(s) concatenated with no separator; plan-mode plan text glues across
  the nudge too.
- Truncated reply with a tool call whose arguments are not strict JSON: no tool runs. All calls of
  that reply are dropped (the model re-emits the whole set, so nothing is half-done or reordered),
  the reply's text (if any) stays in history without the calls, and a user message
  `TRUNCATED_CALL_NUDGE` asks to re-emit, splitting big content. Truncated reply whose calls all
  parse: they are complete, run normally. Text the cut-off replies carried keeps being stitched
  into the answer until a tool call actually runs.
- Cap: `AgentConfig::max_continuations` (default 3) consecutive truncations; a non-truncated
  reply resets it. Configurable via settings `maxContinuations` (same pattern as `toolTimeouts`
  etc.; 0 disables). Cap reached on text → turn ends `success` with the stitched text plus a
  `SystemMessage {event: "Truncation"}` saying it may be incomplete; cap reached on a broken call →
  `error_during_execution` with a clear message (nothing ran).
- Event log / replay: every continuation is an ordinary model request, so `RecordingBackend`
  logs its `request` (delta: partial assistant + nudge appended) and raw `response` (with
  `finish_reason`); `turn_assistant` journals what entered history. `rebuild_requests` stays
  byte-identical and `resume_history` works unchanged.
- stream-json: no new record types or renamed fields; the cut-off reply is a normal `assistant`
  record, the cap note reuses `system/hook_message` (`hook_event_name: "Truncation"`).

Steps (proof per step):
1. Agent logic + config field — `cargo build`; existing agent tests unchanged.
2. Tests in `zen-agent/src/tests_trunc.rs`: text truncation continues & stitches; truncated tool
   call (bad JSON) is not executed and is re-emitted; truncated but complete call runs; cap
   reached → clear message (text and tool cases); `max_tokens` native reason recognized.
3. Settings `maxContinuations` parse test; wired into headless/REPL configs and sub-agents inherit.
4. Replay test in `zen-cli/src/eventlog.rs`: a session with a truncation continuation rebuilds
   every request byte-identically and `resume_history == load_history`.
5. `cargo test --workspace` all green (baseline 471), `cargo clippy --all-targets -- -D warnings`.
6. PR, CI green, backup tag `backup/main-pre-trunc-20261017` at main 5a1242d, merge.

Status log:
- Steps 1-5 done: 13 new tests (9 agent unit, settings parse, eventlog replay, 2 binary
  stream-json e2e); `cargo test --workspace` 484 passed / 0 failed; clippy `-D warnings` clean.
  One test expectation changed during work (stitched text now survives a cut-off tool call).
- Caveat: a tool call written as text (repair path) that is split across a cut-off is not
  re-assembled into a call; it ends up as text. The empty-reply / unparsed nudges were already
  not journaled; continuation nudges live in the next `request` record, which resume uses.
