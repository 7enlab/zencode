# ZenCode architecture

ZenCode is a small, clean-room terminal coding agent written in Rust. It reproduces the *observable
behavior* of tools like Claude Code (see `SPEC.md`) without using any of their code, prompts or tool
texts. This document explains how the pieces fit together so you can study the code crate by crate.

```
                     ┌──────────────────────────── zen-cli (binary `zen`) ────────────────────────────┐
 argv/stdin ───────▶ │ cli.rs (clap) → settings.rs (precedence) → agent_run.rs (assemble everything)  │
                     │ repl.rs / commands.rs (slash + custom commands)   output.rs (text/json/stream) │
                     │ session.rs (JSONL transcripts, resume/continue/fork)  system_prompt.rs, git.rs │
                     └───────┬──────────────┬──────────────┬───────────────┬──────────────────────────┘
                             │              │              │               │
                      zen-agent        zen-tools        zen-ext         zen-mcp
                   (loop, client,   (built-in tools, (settings layers, (MCP client, agent
                    compaction,      permission rules) ZEN.md, skills,  definitions, Task
                    sub-agents)                         hooks)           tool)
                             └──────────────┴──────┬───────┴───────────────┘
                                                zen-core
                         (wire types, Tool/LlmBackend/PermissionChecker/HookRunner/EventSink traits)
```

**Why so many crates?** Each crate owns one concern and depends only on `zen-core`, so they can be
built, tested and understood in isolation (they were literally written in parallel by separate
workers). Only `zen-cli` depends on everything; it is the composition root. Cross-crate needs that
would create cycles are expressed as traits in `zen-core` - e.g. the `Task` tool (zen-mcp) needs to run
an agent (zen-agent), so it receives an `Arc<dyn SubAgentRunner>` that zen-cli builds from zen-agent.

## 1. Shared vocabulary: `zen-core`

| Type / trait | Role |
|---|---|
| `ChatMessage`, `ToolCall`, `ToolDefinition`, `ChatRequest/Response`, `Usage` | OpenAI-compatible chat wire format. History is a `Vec<ChatMessage>` (`system`/`user`/`assistant`/`tool`). |
| `LlmBackend` | `async fn chat(ChatRequest) -> ChatResponse`. The real HTTP client and test mocks both implement it. |
| `Tool`, `ToolRegistry`, `ToolContext`, `ToolOutput` | A tool = name + description + JSON-Schema + `is_read_only` + `async call(args, ctx)`. `ToolContext` is shared session state: persistent cwd, extra dirs, env, set of files already Read, todo list, current tool-call id. |
| `PermissionChecker`, `PermissionDecision`, `Prompter` | Policy (`Allow`/`Deny`/`Ask`) and the human who answers `Ask` in the REPL. |
| `HookRunner`, `HookEvent`, `HookOutcome` | User shell hooks around lifecycle events. |
| `EventSink`, `AgentEvent` | Everything the loop wants to *show* (assistant messages, tool results, retries, compaction, denials). The CLI renders events as text or NDJSON. |
| `RunResult` | The final `{"type":"result",...}` object of a headless run. |
| `Settings`, `SkillInfo`, `AgentDefinition`, `SubAgentRunner` | Data shared between the extension crates and the CLI. |

The loop never prints anything itself: it only emits `AgentEvent`s. That single rule is what lets the
same loop drive the REPL, `--output-format json`, `stream-json`, and sub-agents.

## 2. The agent loop (`zen-agent`)

`Agent::run_turn(prompt)` implements one user turn:

1. **UserPromptSubmit hook.** A blocking hook rejects the prompt (the run ends with `is_error`).
   Successful hooks may return extra context, appended to the user message.
2. **Model call.** The request is `[system prompt] + history`, plus the tool definitions from the
   registry. The system prompt is *not* stored in history; it is prepended on every request, so it
   can be rebuilt (memory, skills) without rewriting the transcript. The CLI builds it once per
   session - role, environment, the **project snapshot** (`zen-cli/src/envsnap.rs`, r7: shallow
   tree, detected build/test commands, toolchain versions, test dirs; ≤4000 chars; off with
   `--bare`), memory, skills - so every request carries the same bytes and the provider can cache
   the prefix. The role prompt ends with a short explore → reproduce → fix → rerun → edge-cases
   workflow (r7 bench C2, `system_prompt.rs`).
3. **Tool calls.** If the assistant message contains `tool_calls`, each one is handled in order:
   * the call is **repaired** first (`repair.rs`, r7): near-miss tool names, fenced / malformed /
     double-encoded JSON and wrong value types are fixed against the tool's schema, the repaired call
     replaces the original in history and the result carries a `[zen: … repaired …]` note; what cannot
     be repaired becomes an error *result* listing the expected fields, never a crash. A message whose
     whole text is one JSON tool call is turned into a real call before this step. Tool names with
     leaked chat-template tokens (`Bash<|channel|>commentary`, `to=functions.Read`) are cut back to
     the tool name, and the note does not echo the tokens (r7 bench C4);
   * **PreToolUse hook** - may block (its stderr becomes the tool result) or force a permission decision;
   * **permission check** (§4 below) - `Ask` goes to the `Prompter` in the REPL; in headless mode it is
     denied and recorded in `permission_denials`;
   * the tool runs with a per-call clone of `ToolContext` whose `current_tool_use_id` is set;
   * **PostToolUse hook** - may add feedback for the model;
   * the per-turn **loop guard** (`guard.rs`, r7) may append one note (4th edit of a file, 3rd
     identical failure), and the finish gate records which files changed;
   * a `role:"tool"` message with the matching `tool_call_id` is appended, and events are emitted.
   Before each model call the main agent re-shows a non-empty todo list every 6th call (`guard.rs`).
4. **Repeat** until the model answers without tool calls, or `max_turns` is reached
   (`subtype: "error_max_turns"`). An answer with neither text nor tool calls is not an answer: it
   is dropped and the model is requeried with a one-line correction, at most twice per turn
   (r7 bench C5, after SWE-agent's format-error requery). When every client retry came back as an
   unparsed sample (`model returned no choices` / gateway error in a 200), the run does not end with
   `error_during_execution`: the model is requeried with a one-line correction instead, from the same
   2-per-turn budget (r7 bench C7, `agent.rs` `is_unparsed_sample`); other errors still end the run.
5. **Stop hook.** A blocking Stop hook pushes its feedback as a new user message and the loop
   continues (with a cap so a misbehaving hook cannot loop forever). Then the built-in
   **check-before-finish gate** (`finish.rs`, r7) runs on the same path and cap: with settings
   `verify.command` it runs the command after file changes (max 2 rounds per turn) and feeds a
   failure back; otherwise it sends one reminder when files changed and no Bash ran since.
6. **Accounting.** Token usage from each response is summed; durations are measured; cost is `0.0`
   because only free models are used.

### The HTTP client
`OpenAiClient` posts to `{base_url}/chat/completions`. It retries 429/5xx/network errors with
exponential backoff (raised to a server `Retry-After` / `retry_after_seconds` hint, capped at 60 s) and
emits `ApiRetry` events. A 200 whose body is an `error` object, has no choices, or holds an empty
assistant message is resampled on the same schedule (r7 bench C3: gpt-oss Harmony parse errors come
back this way from vLLM gateways); after the retries are exhausted the agent tries each
`fallback_models` entry. It tolerates quirks of free models (`content: null` next to tool calls, empty
argument strings). It honours `HTTPS_PROXY` and adds the PEM bundle named by `SSL_CERT_FILE` as extra
trusted roots (needed behind TLS-inspecting proxies). The API key is optional and never logged.

### Module map
`agent.rs` (loop), `client.rs` (HTTP), `compaction.rs` (§7), `plan.rs` (plan mode + history-surgery
helpers), `repair.rs` (tool-call repair), `guard.rs` (loop guard + todo focus), `finish.rs`
(check-before-finish gate), `subagent.rs` (`SubAgentExecutor`, the `SubAgentRunner` behind `Task`), `testutil.rs`
(scripted backends for tests).

### Plan mode
In `plan` permission mode mutating tools are hidden from the model and blocked if called anyway; the
model researches with read-only tools and its final answer is the plan.

## 3. Tools (`zen-tools`)

`builtin_tools()` returns `Read`, `Write`, `Edit`, `MultiEdit`, `Bash`, `Glob`, `Grep`, `WebFetch`,
`TodoWrite`. Design points worth reading in `builtin.rs`:

* **Read-before-write, and not stale.** `ToolContext::read_files` maps each canonical path to a hash
  of the content zen last saw (Read, or its own Write/Edit). `Write`/`Edit`/`MultiEdit` refuse a file
  the agent has not read, and one whose content changed since (a formatter, a Bash command, a person).
* **Edit: exact first, tolerant second** (`edit_match.rs`). `old_string` must occur exactly once unless
  `replace_all`. Only when it is absent do line-based fallbacks run (trailing whitespace/CRLF, then
  indentation with per-level re-indent, then stripping Read's line numbers), each accepted only for a
  unique hit. A miss lists the closest regions with line numbers. `MultiEdit` applies several edits in
  memory and writes once, all or nothing.
* **Syntax guard** (`syntax_guard.rs`, r7 bench C1). Before Write/Edit/MultiEdit write a `.py`, `.js`,
  `.json` or `.sh` file, the new text is parsed (`ast.parse`, `node --check`, serde_json, `bash -n`);
  a change that breaks a file that parsed before is refused with the error and nearby lines (SWE-agent's
  linter-gated edit). Missing checkers never block; `ZEN_NO_SYNTAX_GUARD=1` disables it.
* **Bash keeps a working directory.** The final `pwd` of each command is captured so `cd` persists
  across calls; long output is truncated in the middle; timeouts kill the process.
* **Glob/Grep** walk the tree skipping `.git`, build dirs and `.gitignore`d paths; Grep uses `rg` when
  present and a built-in regex walker otherwise.
* **`.gitignore` support** (`gitignore.rs`) is a small last-match-wins matcher, not a full git
  implementation.
* **`--tools`** (applied in zen-cli `agent_run.rs`) picks the built-in set: `""` none, `default` all,
  `Read,Bash` only those, or a delta `-Bash,-Write` / `+Name` on the default set (forms cannot mix).
* Other tools live elsewhere: `Skill` (zen-ext), `Task` and `mcp__*` (zen-mcp).

## 4. Permissions (`zen-tools::permissions`)

Evaluation order for every tool call:

```
deny rule matches?  → Deny            (always wins, even in bypassPermissions)
ask rule matches?   → Ask
allow rule matches? → Allow
otherwise           → mode default:
   default            read-only tools Allow, others Ask
   acceptEdits        + Write/Edit inside working dirs, simple fs commands (mkdir/touch/mv/cp)
   plan               non-read-only → Deny
   bypassPermissions  Allow
   dontAsk            anything that would Ask → Deny
```

Rules come from every settings layer (lists are merged) plus `--allowedTools/--disallowedTools`.
Syntax: `Tool`, `Bash(npm test)`, `Bash(git diff *)` (prefix; the space matters), path globs such as
`Read(./.env)` or `Edit(src/**)`, `WebFetch(domain:example.com)`, `mcp__server` / `mcp__server__tool`.
Answering "always" in the REPL appends a rule to `.zen/settings.local.json`.

## 5. Settings, memory, skills and hooks (`zen-ext`)

* **Settings layering** - `user (~/.zen/settings.json) < project (.zen/settings.json) <
  local (.zen/settings.local.json) < --settings < managed (/etc/zen/managed-settings.json)`.
  Objects merge deeply, arrays concatenate with de-duplication, scalars are overridden. Broken files
  produce warnings and are skipped. CLI flags and env vars (`--model`, `ZEN_MODEL`) are applied on top
  by zen-cli.
* **ZEN.md memory** (`memory.rs`) - `~/.zen/ZEN.md`, then every `ZEN.md` / `.zen/ZEN.md` from the filesystem root
  down to the cwd, then `ZEN.local.md`; `@path` lines import other files (depth-limited, cycle-safe).
  **AGENTS.md fallback (C4.4)** is decided per directory: a directory with no ZEN file loads its
  `AGENTS.md` then `.zen/AGENTS.md` instead (never both kinds), and `~/.zen/AGENTS.md` stands in for a
  missing `~/.zen/ZEN.md`. The result is a system-prompt section with path headers.
* **Skills** - folders with a `SKILL.md` (frontmatter `name`, `description`). Only names and
  descriptions go into the system prompt; the model loads a body on demand with the `Skill` tool.
  This "progressive disclosure" keeps the prompt small.
* **Custom slash commands** (`commands.rs`) - Markdown files in `~/.zen/commands/` and
  `.zen/commands/` (project overrides user, subdirectories become namespaces), with argument
  placeholders and permission-gated shell pre-execution; user-invocable skills appear here too.
* **Hooks** - `settings.hooks[Event] = [{matcher, hooks:[{type:"command", command, timeout}]}]`.
  Commands run with `bash -c`, receive a JSON payload on stdin, and communicate through their exit
  code (0 ok, 2 block with stderr as feedback, other = non-blocking error) or optional JSON stdout.

## 6. MCP and sub-agents (`zen-mcp`)

* Modules: `config.rs` (config files, `${VAR:-default}` expansion), `transport.rs` (stdio + HTTP
  JSON-RPC), `manager.rs` (connect, list, status), `agents.rs` (sub-agent definitions), `task.rs`
  (the `Task` tool).
* **MCP client.** Servers come from `.mcp.json`, `~/.zen/mcp.json` and `--mcp-config`. For each server
  the manager performs `initialize` → `notifications/initialized` → `tools/list` (stdio JSON-RPC over
  newline-delimited messages, or HTTP POST with JSON or SSE responses), concurrently and with a timeout.
  Each remote tool becomes a local `Tool` named `mcp__<server>__<tool>` whose call is `tools/call`.
  A server that fails is reported as `failed`, never fatal.
* **Sub-agents.** Definitions are Markdown files with frontmatter (`.zen/agents/*.md`,
  `~/.zen/agents/*.md`) or `--agents` JSON; a built-in `general-purpose` agent always exists. The
  `Task` tool picks a definition and calls `SubAgentRunner::run_subagent`. zen-agent's
  `SubAgentExecutor` builds a *fresh* `Agent` (empty history, filtered tools, depth + 1) and returns
  only its final text - the parent's context stays small. Sub-agent events carry
  `parent_tool_use_id` so stream-json consumers can rebuild the tree.

## 7. Context compaction

Long sessions outgrow the model's context window. When the last response's prompt tokens (or a
chars/4 estimate) exceed the threshold (`autoCompact` setting / `--autocompact`), or when the user runs
`/compact [focus]`, the agent:

1. runs the `PreCompact` hook;
2. flattens history into a plain-text transcript (`[User]`, `[Assistant]`, `[Assistant tool calls]`,
   `[Tool result]` lines, tool results capped) inside one user message and asks the model, with no
   tools and no system prompt, for a structured summary (goal, decisions, files, open tasks) - so the
   model summarizes instead of continuing the chat;
   * paths from `Read` and `Write`/`Edit` calls are appended as `<read-files>` / `<modified-files>`
     blocks, merged with the previous summary's lists, so they survive repeated compactions;
3. replaces history with the summary (as a user message) plus the last few messages, taking care not
   to keep a `tool` message whose originating assistant `tool_calls` message was summarized away
   (OpenAI-style APIs reject orphaned tool messages);
4. emits `CompactBoundary`, which the session writer records as a `summary` line.

## 8. Sessions (`zen-cli::session`)

Each conversation is a JSONL file at `~/.zen/projects/<cwd with / and . replaced by ->/<uuid>.jsonl`.
One line per message (plus summary records after compaction). `--continue` opens the newest file for
the current directory, `--resume <id|path>` searches all project directories, `--fork-session` copies
history into a new id, and `--no-session-persistence` writes nothing. On load, only messages after the
last summary are replayed, so a resumed session starts from the compacted state.

## 9. Output formats (`zen-cli::output`)

* `text` - the final answer only.
* `json` - one `RunResult` object (`type, subtype, is_error, duration_ms, duration_api_ms, num_turns,
  result, session_id, total_cost_usd, usage, model, permission_denials`).
* `stream-json` - NDJSON: a `system/init` line (model, tools, MCP status, permission mode, slash
  commands, cwd), then `assistant` and `user` (tool result) lines with `parent_tool_use_id`, optional
  `system/api_retry`, `system/compact_boundary`, `system/permission_denied`, and finally the `result`.

Errors inside a run still produce a result object with `is_error: true` and exit code 1.

## 10. Testing strategy

* Every crate has unit tests that use a scripted `LlmBackend`, fake tools, fake permission checkers and
  temp directories (with `home` overrides so the real `~` is never touched).
* zen-cli has end-to-end tests that run the real `zen` binary against a mock OpenAI-compatible HTTP
  server inside the test, and a fake stdio MCP server written in Python.
* `cargo clippy --workspace --all-targets -- -D warnings` must pass.
* Live smoke tests against free models are recorded in `docs/e2e-runs/` (latest: AnyModel `am/free`,
  2026-10-15, json + stream-json).
* `tools/bench.sh` (r7) is a 5-task mini-benchmark (fix a failing test, rename across files, add a
  CLI flag, edit a tab-indented file, implement from a test) run with `zen -p` in fresh temp repos
  and graded by hidden checks; `BENCH_SELFTEST=1` proves each grader fails on the fixture and passes
  on a reference fix. Results go to `docs/e2e-runs/<date>-bench.json`.
* `zen-cli/src/lib.rs` exposes the CLI surface, session helpers and output sinks so integration tests
  can reuse them; `git.rs` adds best-effort git context to the system prompt.
