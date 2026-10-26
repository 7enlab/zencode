# Top coding agents (Oct 2026) - what they do, and what ZenCode should adopt next

Scout report, 2026-10-15. Read-only pass over ZenCode (PLAN.md, SPEC.md, docs/ARCHITECTURE.md, crates/*) plus public docs/READMEs/blogs.
Clean-room rule holds: everything below describes **behaviors** in our own words. No source, prompts, tool texts or plugin text were copied, and none should be copied when implementing.

---

## 1. Key finding: the harness moves the score more than the model

* Terminal-Bench 2.0 (89 Docker tasks): the top entries cluster at ~80–85% with overlapping ±2–3% CIs (NexAU-AHE 84.7%, LemonHarness 84.5%, Capy 83.1%, Codex CLI/GPT-5.5 82.2%, ForgeCode 81.8%). The **same model swings 15–25 pp depending on harness**: Opus 4.6 got 58% in Claude Code but 79.8–81.8% in ForgeCode; Gemini 3.1 Pro went from 55% to 80.2% in TongAgents; Gemini 3 Flash went from 47.8% (Google's own run) to 65.2% in Dirac.
  Sources: https://www.tbench.ai/leaderboard/terminal-bench/2.0 · https://stackfutures.com/blog/terminal-bench-20-nexau-ahe-scaffold-race/ · https://codex.danielvaughan.com/2026/04/09/harness-performance-terminal-bench/ · https://www.computeleap.com/blog/harness-leaderboards-dirac-gemini-3-flash-terminalbench/
* **LangChain deepagents-cli, 52.8 → 66.5 with the model held fixed** (https://www.langchain.com/blog/improving-deep-agents-with-harness-engineering, fetched):
  1. a plan → build → **verify** → fix structure, plus a *pre-completion checklist* hook that stops the agent before exit and makes it run verification against the task spec (the most common failure was "wrote code, re-read it, said it looks fine, stopped");
  2. **environment context injected at start**: cwd tree, available toolchains (python etc.), the fact that grading is by tests, time budget warnings;
  3. **loop detection**: per-file edit counters, with a "reconsider your approach" note after N edits to the same file;
  4. a "reasoning sandwich": high effort for planning and verification, lower for execution.
* **ForgeCode, ~25% → 78.4% → 81.8%** (summary via https://devthrottle.com/coding-agents/forgecode and https://www.tensorlake.ai/blog/forgecode-terminal-bench; the forgecode.dev blog URLs now 404): a **tool-call correction layer** that repairs bad calls instead of failing the turn; **mandatory todo_write** for multi-step tasks; **flattened, consistently ordered tool schemas**; **explicit truncation markers** on partial reads; a **mandatory verification pass** before completion; semantic entry-point discovery; parallel execution of independent tool calls; tiered reasoning effort.
* **oh-my-pi hashline** (https://github.com/can1357/oh-my-pi README, fetched): edits point at line anchors (a line number plus a short content hash, or a file snapshot tag) instead of retyping old text, and stale anchors are rejected. Self-reported: +15 pp on average over 16 models; Grok Code Fast went from 6.7% to 68.3%; output tokens down 61% on Grok 4 Fast. An independent edit-bench found the effect **depends on the language** (worse on Python, neutral on TS, a wash on Rust) and concluded that "edit format is not the bottleneck" for strong models (https://pi.dev/packages/@d3ara1n/pi-hashline-edit). Takeaway: the **weaker the model, the more edit and tool robustness matter**. ZenCode runs only free models, so robustness work pays off more for us than for most agents.

## 2. Agents × features

Legend: ● built-in / first-class · ◐ partial, opt-in or plugin · – none known. Source per agent is listed under the table.

| Feature | Claude Code | Codex CLI | Gemini CLI | Aider | OpenCode | Cline/Roo | Cursor CLI | Amp | Goose | oh-my-pi | ForgeCode | **ZenCode now** |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Edit format | exact str-replace + multi-edit, read-before-edit | `apply_patch` freeform grammar (multi-file add/update/delete/move) | replace + write | search/replace, udiff, whole, per-model choice | edit/patch, modified-since-read check | multi search/replace, order-invariant | str-replace | `apply_patch` (rush) | developer ext. | **hashline** + fuzzy replace + ast_edit | replace | exact str-replace, read-before-write (no staleness check, no multi-edit) |
| Tool-call repair | ◐ | ◐ lenient patch parse | ◐ | n/a (text formats) | ◐ | ◐ | ? | ? | ◐ | ● normalization warnings | **●** correction layer | ◐ bad JSON → error result only |
| Repo map / context engine | – (agentic grep, Explore agent) | – | – | **●** tree-sitter + PageRank map | ◐ LSP symbols | ◐ Roo codebase index | ● semantic index | ● Search subagent | – | ● ast summaries, read summarizes | ● semantic search | – (Glob/Grep only) |
| Env snapshot at start | ◐ git status, env block | ◐ | ◐ | ◐ | ◐ | ◐ | ◐ | ◐ | ◐ | ● AGENTS/workspace scan | ◐ terminal history | ◐ cwd, git branch/status, OS |
| Todo / plan tool | ● TodoWrite/Tasks + plan mode | ● plan mode | ● task planning | ◐ architect mode | ● build/plan agents | ● **Focus Chain** (re-injected every N msgs) + Plan/Act | ● | ● | ◐ | ● todo w/ phases | ● **mandatory** todo | ● TodoWrite + plan mode (not enforced, not re-injected) |
| Sub-agents | ● (Explore on a cheaper model, custom, worktree isolation, background) | ● | ● | – | ● general/explore | ● | ● explore/browser | ● Search/Oracle/Librarian/Task | ● subrecipes, parallel | ● typed results, worktrees, advisor | ● forge/sage/muse | ● Task, custom agents, depth limit |
| Second-opinion model | ◐ advisor tool | ◐ auto-review approvals | – | ● architect/editor pair | – | – | – | **●** Oracle (different frontier model, read-only) | ● lead/worker | ● advisor role | ◐ | – |
| Sandbox | ◐ process-level + permission rules | **●** Seatbelt / Landlock+seccomp / bwrap / Windows DACL; read-only default | – (sandbox opt.) | – | ◐ permissions | ◐ approvals | ◐ | ● orbs (remote VMs) | ◐ sandbox mode | ◐ project trust | ◐ | ◐ permission rules only (sandboxed bash = SPEC non-goal) |
| Verification loop | ◐ hooks (Stop), /code-review | ◐ | ◐ | ● auto lint + test after edit (`--auto-test`) | ◐ LSP diagnostics | ● watches linter/compiler errors | ◐ | ● Task does verification | ◐ | ● LSP diagnostics on every write | **●** mandatory verify | ◐ Stop hook exists; nothing built in |
| Checkpoint / undo | ● /rewind (code, conversation or both; summarize from here) | ◐ backtrack (conversation only; code-rewind requested, issue #12558) | ● shadow-git checkpoints, /restore, /rewind | ● git auto-commit, /undo | ● git snapshots per step, /undo /redo | ● checkpoint per tool use, compare/restore | ◐ | ● | ◐ | ● checkpoint/rewind tools | ◐ | – |
| LSP / diagnostics | ◐ LSP plugin tool | – | – | ◐ lint via tree-sitter | ● (docs: CLI linters often better) | ● diagnostics | ● IDE | ◐ | – | **●** 14 LSP ops + DAP debugger | ◐ | – |
| Caching / cost | **●** cache-stable prefix, no mid-session model swap, deferred tool stubs | ● | ◐ | ● prompt caching opt. | ◐ | ◐ | ◐ | ● | ◐ | ● /fresh cache reset | ◐ | cost 0 (free models), prompt rebuilt each request |
| Model routing | ● opusplan, haiku Explore, fallback | ● profiles | ◐ | ● weak-model for commits/summaries, editor model | ● per-agent model | ● per-mode model | ● Auto | **●** Dial low/med/high/ultra, per-role pins | ● lead/worker auto-fallback | **●** 9 roles (default/smol/slow/plan/…) + fallback chains | ● per-agent | ◐ `fallback_models`, per-sub-agent model |
| Headless/CI | ● -p json/stream-json, json-schema | ● `codex exec --json`, resume | ● | ● scripting | ● run/serve | ● CLI `--json` | ● | ● | ● recipes in CI | ● -p, RPC, ACP | ● | ● -p text/json/stream-json (json-schema / stream input open) |
| Session UX | resume/fork, /context, /btw | resume | resume browser, named checkpoints | /drop /add files | share links, multi-session | task history | – | **handoff** to a new thread instead of compaction | sessions | collab, agent hub | – | resume/continue/fork, /compact, /context, /export, /rename |

Sources: Claude Code checkpointing https://code.claude.com/docs/en/checkpointing · prompt caching https://code.claude.com/docs/en/prompt-caching and https://claude.com/blog/lessons-from-building-claude-code-prompt-caching-is-everything · sub-agents https://code.claude.com/docs/en/sub-agents · Codex sandbox https://github.com/openai/codex/blob/main/docs/sandbox.md, exec https://developers.openai.com/codex/noninteractive.md, apply_patch grammar https://github.com/openai/codex/blob/main/codex-rs/core/assets/tools/apply_patch.lark · Gemini CLI checkpointing https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/checkpointing.md, rewind https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/rewind.md · Aider repo map https://aider.chat/docs/repomap.html, edit formats https://aider.chat/docs/more/edit-formats.html, modes https://aider.chat/docs/usage/modes.html · OpenCode LSP https://opencode.ai/docs/lsp, agents https://opencode.ai/docs/agents, snapshots https://opencode.ai/v2/docs/snapshots/ · Cline focus chain https://github.com/cline/cline/blob/main/docs/features/focus-chain.mdx, checkpoints https://docs.cline.bot/core-workflows/checkpoints · Roo features https://roocodeinc.github.io/Roo-Code/features/ · Cursor subagents https://cursor.com/docs/subagents · Amp modes https://ampcode.com/docs/models-and-subagents, oracle https://ampcode.com/docs/tools, handoff https://ampcode.app/news/handoff, rush+oracle https://ampcode.com/news/rush-2.0 · Goose lead/worker https://github.com/opensecuritycompliance/goose/blob/main/documentation/docs/tutorials/lead-worker.md, subagents https://goose-docs.ai/docs/guides/context-engineering/subagents/ · oh-my-pi https://github.com/can1357/oh-my-pi · ForgeCode https://github.com/tailcallhq/forgecode + the summaries above.
Cells marked ◐ or ? from search excerpts only are lower confidence. The well-sourced cells are edit format, checkpoints, sandbox, routing and verification.

### pstack (/workspace/tools/pstack): workflow ideas worth stealing as zen behaviors or skills
* `create-verification-skill`: generate a per-repo **verify recipe** (launch → doctor → drive → evidence → cleanup) that later agents reuse. Maps to improvement #3 (a `verify` config that `/init` can propose).
* `swarm`: fan out N workers whose reports are fixed to **PASS / ISSUES / BLOCKED + evidence**; drop and respawn workers that don't follow the format. Maps to a structured sub-agent result for Task.
* `interrogate` / `arena`: reviewer sub-agents with a rubric, and a multi-model race. Maps to the "second-opinion model" gap (Amp Oracle, omp advisor).
* poteto-mode playbooks `session-pickup` / `pause-safely`: leave a handoff file good enough for a cold restart. Maps to an Amp-style `/handoff` alternative to compaction.
* Principles `prove-it-works` and `guard-the-context-window`: same message as LangChain (verify, keep context lean).

## 3. Gap list vs ZenCode

What exists (P1 parity 55/55, 363 tests): settings layering, permissions (rules + 5 modes), hooks (full P1 event set), ZEN.md memory + imports, custom commands/skills, sub-agents (Task, custom, built-in Explore/Plan), MCP stdio/http, headless text/json/stream-json, resume/continue/fork, compaction, `--tools`.

Gaps that affect **task success** (ordered by evidence strength):
1. Edit robustness. Only exact unique match. No staleness check: `ToolContext::read_files` is a path set with no content hash or mtime. No multi-edit/atomic batch. No whitespace-tolerant fallback. No "closest match" hint on failure.
2. No tool-call repair beyond "bad JSON → error result". No argument coercion, name fuzzing, or fenced/stringified args handling.
3. No built-in verification gate. The Stop-hook mechanism exists, but nothing uses it by default.
4. Thin starting context: only cwd/git/OS in system_prompt.rs. No project-type, test/build-command or toolchain detection.
5. No doom-loop detection. The todo list is never re-injected and never required.
6. No checkpoints or `/rewind` (code or conversation).
7. No post-edit diagnostics (linter/typecheck/LSP).
8. Routing: only fallback models. No cheap-model role for Explore/compaction/WebFetch summaries, no plan-vs-execute model, no reasoning-effort passthrough. The system prompt is rebuilt per request, so the cached prefix is not guaranteed stable.

Lower priority / later: tool calls run sequentially (speed only); no repo map (tree-sitter); no OS sandbox; no second-opinion/oracle tool; `--json-schema`, stream-json input, real SSE streaming (already open P2); no `/handoff`; no eval harness for zen itself.

## 4. Top 8 improvements, ranked by (impact on task success ÷ effort)

Each item names its behavior, prior art, likely files, a verify criterion and a size. P2 = next round; P3 = later or experiment. All behaviors are written in our own words; implement without looking at others' code or prompts.

### #1 Robust Edit: staleness guard + tolerant matching + MultiEdit - **P2, S–M**
* **Behavior.** (a) Read records a content hash per file. Edit/Write refuse if the file changed on disk since the last Read ("file changed since you read it - re-read"). (b) When the exact match fails, try ordered fallbacks: trailing-whitespace-insensitive, indentation-normalized (re-indent `new_string` by the detected delta), CRLF/LF-normalized. Accept only a **unique** fallback hit, and say in the result which fallback was used. (c) On a miss, return the 1–3 most similar regions with line numbers so the model can retry once instead of looping. (d) A `MultiEdit` tool (or an `edits[]` array on Edit) applies several replacements to one file against the same pre-edit snapshot, all or nothing.
* **Who.** Claude Code (multi-edit, read-before-edit), OpenCode (modified-since-read check), Cline (order-invariant multi-block), oh-my-pi (fuzzy threshold, stale rejection), Aider (search/replace with fuzzy recovery).
* **Files.** `crates/zen-core/src/lib.rs` (ToolContext: read_files → path→hash map; additive), `crates/zen-tools/src/builtin.rs` (Read/Write/Edit, new MultiEdit), `crates/zen-tools/src/permissions.rs` (MultiEdit treated like Edit for rules), `crates/zen-cli/src/agent_run.rs` (registry).
* **Verify.** New unit tests: edit after an external modification → error; indentation-shifted `old_string` → applied with a fallback note; ambiguous fuzzy match → error; MultiEdit where the 2nd of 3 edits fails → file unchanged. `cargo test --workspace` green; e2e on a free model: an edit task with tab/space mismatch succeeds in ≤2 Edit calls.

### #2 Tool-call repair layer - **P2, S**
* **Behavior.** Before a call is rejected: strip code fences or a leading "json" from argument strings; parse once more after common fixes (trailing commas, single quotes, an unescaped newline in a string); unwrap args sent as a JSON string inside JSON; coerce scalar types to the schema (`"20"`→20, `"true"`→true); map near-miss tool names (case, `_`/`-`, known aliases such as `str_replace`→Edit, `bash`→Bash); drop unknown keys. A repaired call runs with a short "repaired: …" note in its result. Calls that can't be repaired return an error that lists the expected schema fields. Keep tool JSON schemas flat with required fields first.
* **Who.** ForgeCode (correction layer, flattened/ordered schemas), oh-my-pi (normalization warnings), Codex (lenient apply_patch parsing of heredoc-wrapped calls).
* **Files.** New `crates/zen-agent/src/repair.rs`; `crates/zen-agent/src/agent.rs` (call it before dispatch); `crates/zen-agent/src/client.rs` (provider quirks already handled there); schema review in `crates/zen-tools/src/builtin.rs`.
* **Verify.** Table-driven unit tests with ~15 malformed calls taken from real free-model transcripts (docs/e2e-runs, .pi-logs): each is either repaired to the expected args or rejected with a schema hint, and none panics. Mock-LLM test: a turn with `{"name":"edit","arguments":"```json{...}```"}` succeeds.

### #3 Built-in verification gate before finishing - **P2, S–M**
* **Behavior.** If the turn mutated files (Write/Edit/MultiEdit or a mutating Bash) and no verification command has run since the last mutation, the loop injects **one** reminder at the would-be stop: "you changed X files; run the project's checks and compare against the request". It does not stop the turn. With `verify.command` set (e.g. `cargo test -q`; `/init` can propose it from detected project files), the harness runs the command itself, feeds back the last N lines on failure, and continues, with a cap of 2 automatic rounds. Implemented as a built-in that uses the existing Stop-continuation path (loop guard max 8 already exists). Off with `verify.enabled=false`; in `-p` mode it is on only when configured.
* **Who.** LangChain PreCompletionChecklist (+13.7 pp together with the other changes), ForgeCode mandatory verification, Aider `--auto-test`, Claude Code Stop-hook pattern, pstack create-verification-skill.
* **Files.** `crates/zen-agent/src/agent.rs` (stop phase, mutation tracking), `crates/zen-ext/src/settings.rs` (`verify` key; additive), `crates/zen-cli/src/agent_run.rs` + `commands.rs` (`/init` proposes a verify command), SPEC.md new section.
* **Verify.** Mock-LLM e2e: the model edits and then answers → exactly one injected reminder → the model runs Bash → success. With `verify.command` failing twice, the run ends with the failure summary after 2 rounds (no infinite loop). No reminder when the turn made no mutations.

### #4 Environment snapshot at session start - **P2, S**
* **Behavior.** A compact block (≤1.5k tokens) added once to the system prompt. Kept stable for the session so it stays cache-friendly. It holds: a depth-2 tree of the cwd (gitignore-aware, capped); detected project types and their **build/test/lint commands** (Cargo.toml→`cargo test`, package.json scripts, pyproject/pytest, Makefile targets, go.mod); toolchain versions found on PATH (`rustc`, `node`, `python3`, `go`); whether tests exist; and a line saying the work is judged by running checks. Disable with `--bare`.
* **Who.** LangChain LocalContextMiddleware; Claude Code/Codex/Gemini env blocks; ForgeCode entry-point discovery; oh-my-pi workspace scan.
* **Files.** New `crates/zen-cli/src/envsnap.rs` (or extend `git.rs`), `crates/zen-cli/src/system_prompt.rs`; reuse `zen-tools` gitignore walker.
* **Verify.** Unit tests on fixture dirs (cargo, npm with scripts, python, empty): the expected commands are detected, the size cap is respected and gitignored dirs are excluded. e2e: the system prompt contains the "Project" block, and `--bare` omits it.

### #5 Loop guard + todo focus re-injection - **P2, S**
* **Behavior.** (a) Track per-file edit counts and identical (tool, args-hash, error) repeats within a turn. After N (default 4) edits to one file, or 3 identical failing calls, append a one-time note to the next tool result: stop repeating, re-read the file, state what is wrong, and consider another approach. (b) If the todo list is non-empty, re-inject a compact todo snapshot into a tool result every K (default 6) model calls, and after compaction. (c) Optional `todo.require` setting: if the first ≥3 mutating calls happen with an empty todo list, nudge once to create one.
* **Who.** LangChain LoopDetectionMiddleware, Cline Focus Chain (re-inject every 6 messages, survives compaction), ForgeCode mandatory todo.
* **Files.** New `crates/zen-agent/src/guard.rs`; `crates/zen-agent/src/agent.rs`; `crates/zen-agent/src/compaction.rs` (keep the todo after summary); `zen-core` ToolContext todos (exists).
* **Verify.** Mock-LLM tests: 5 edits to a.rs → exactly one guard note; 3 identical failing Bash calls → note; todo re-injection appears at call 6 and 12 and after `/compact`; no notes in a normal 3-call turn.

### #6 Checkpoints + `/rewind` (code, conversation or both) - **P2, M**
* **Behavior.** Before the first mutating tool of each user turn, snapshot the affected files into a session-local store: a shadow git repo under `~/.zen/history/<project-hash>`, or content-addressed copies when git is absent. Record the checkpoint id in the transcript. REPL `/rewind` lists turns; you choose code-only, conversation-only or both, and the restored prompt goes back into the input. `/undo` = rewind the last turn (both). Only zen's own Write/Edit changes are tracked; Bash side effects are listed as "not covered". Checkpoints survive `--resume`. `-p --rewind <turn>` is P3.
* **Who.** Claude Code /rewind (with summarize options), Gemini CLI shadow-git checkpoints + /restore + /rewind, OpenCode per-step git snapshots /undo /redo, Cline checkpoint per tool use, Aider auto-commit /undo.
* **Files.** New `crates/zen-tools/src/checkpoint.rs` (store) or zen-cli; `crates/zen-tools/src/builtin.rs` (call the snapshot before writes via ToolContext hook); `crates/zen-cli/src/repl.rs` (/rewind, /undo), `crates/zen-cli/src/session.rs` (checkpoint records), SPEC C5.1 addition.
* **Verify.** e2e: turn 1 edits a.txt, turn 2 edits b.txt + creates c.txt; `/rewind 2 code` restores b, deletes c and leaves a; `/rewind 1 both` also trims history; resume + `/rewind` still works; non-git dir works.

### #7 Post-edit diagnostics (configurable checker → later LSP) - **P2 (checker), P3 (LSP), M**
* **Behavior.** After a successful Edit/Write/MultiEdit, if `diagnostics.command` is configured for that file's extension (e.g. `rs`→`cargo check --message-format short -q`, `ts`→`tsc --noEmit -p .`, `py`→`ruff check {file}`), run it with a short timeout. Debounce: once per batch of edits in the same model step. Append ≤20 lines of new errors **for the touched files** to the tool result. Keep it off by default and let `/init` propose it. LSP (`lsp` tool + diagnostics via language servers) stays P3, because OpenCode's own docs say linters/typecheckers are often a better feedback signal than out-of-sync language servers.
* **Who.** OpenCode (LSP diagnostics, plus the advice to prefer CLI checks), oh-my-pi (LSP on every write), Cline (watches linter/compiler errors), Claude Code LSP plugin, Aider auto-lint.
* **Files.** `crates/zen-ext/src/settings.rs` (`diagnostics` map), `crates/zen-agent/src/agent.rs` (post-tool step, after PostToolUse hooks), maybe `crates/zen-tools/src/diagnostics.rs`.
* **Verify.** e2e on a tiny Rust fixture: an edit that introduces a type error → the Edit result contains the `cargo check` error line; a correct edit → no diagnostics block; the timeout kills the checker and the edit still succeeds.

### #8 Model roles + cache-stable prompt - **P2, S–M**
* **Behavior.** (a) Settings `models: { default, small, plan }`. `small` is used for built-in Explore sub-agents, compaction summaries and WebFetch summarization. `plan` is used while in plan mode (an opusplan-style switch, done once at the mode boundary). (b) A `reasoningEffort` passthrough (`reasoning_effort`/provider equivalent) per role, with an optional "high for first and verify turns, lower in between" policy (LangChain/ForgeCode "sandwich"). Ignore it silently for providers that reject it, as the relay already strips `thinking`. (c) Cache stability: build the system prompt once per session (memory, skills list, env snapshot). Send mid-session changes as user/tool-message reminders instead of rewriting the prefix. Keep tool order fixed. Don't switch the main model mid-session; delegate to a sub-agent instead. (d) Goose-style escalation (P3): after M consecutive failed tool calls, retry the turn on the stronger model, then drop back.
* **Who.** Claude Code (opusplan, Haiku Explore, prompt-cache rules), Amp Dial + Oracle, oh-my-pi 9 roles + fallback chains, Goose lead/worker, Aider weak/editor model.
* **Files.** `crates/zen-ext/src/settings.rs`, `crates/zen-agent/src/client.rs` (per-request model/effort), `crates/zen-agent/src/subagent.rs`, `crates/zen-agent/src/compaction.rs`, `crates/zen-cli/src/system_prompt.rs`, `crates/zen-mcp/src/agents.rs` (Explore default model = small).
* **Verify.** Mock-server tests record request bodies: Explore and compaction requests use `models.small`; plan-mode requests use `models.plan`; the system prompt bytes are identical across 3 requests in one session (even after a skill is added mid-session, which shows up as a reminder message instead); `reasoning_effort` is present only when configured.

## 5. Next candidates (beyond top 8)
| # | Item | P | Size | Note |
|---|---|---|---|---|
| 9 | **Hashline / line-anchored edit mode** (opt-in tool variant: Read shows `line:tag`, edits reference anchors, stale tag → reject) | P3 (experiment) | M | Big wins on weak models; language-dependent. Gate it behind a per-model setting and measure with #12. Must be written from the behavior description only. |
| 10 | `apply_patch`-style multi-file patch tool (add/update/delete/move in one call) | P3 | M | Some models (GPT/Codex family) are trained on it; offer it per model. |
| 11 | Parallel execution of read-only tool calls in one assistant message | P2 | S–M | Latency only; keep the order of results; mutating calls stay sequential. |
| 12 | **zen eval harness**: a local edit-bench (~30 mutation tasks) + optional Harbor Terminal-Bench run | P2 (enabler) | M | Needed to prove #1–#9 actually help on our free models; LangChain/ForgeCode both iterated on traces. |
| 13 | Oracle/second-opinion tool: read-only sub-agent on a different/stronger model for plan review | P3 | S | Amp Oracle, omp advisor, pstack interrogate. |
| 14 | `/handoff <goal>`: start a fresh session from an extracted brief + file list instead of compaction | P3 | S | Amp handoff, pstack pause-safely. |
| 15 | Structured sub-agent results (PASS/ISSUES/BLOCKED + evidence, or a JSON schema) | P3 | S | pstack swarm, omp typed task results; pairs with open C8.7 `--json-schema`. |
| 16 | OS sandbox for Bash (bubblewrap/Landlock on Linux, read-only default, network off) | P3 | L | Codex is the reference; currently a SPEC non-goal. |
| 17 | Repo map (tree-sitter symbol outline ranked by references) | P3 | L | Aider; heavy dependency. #4 covers most of the value cheaply. |

## 6. Notes, assumptions, open questions
* ZenCode was read-only for this pass. Another worker is editing the source, so line-level details may shift; the file paths above are the likely touch points, not a plan of exact edits.
* Leaderboard numbers are self-reported or leaderboard snapshots (May–Jun 2026). Use them as direction, not as precise ranks.
* ForgeCode's own blog posts returned 404 at fetch time. Its techniques are cited from third-party summaries (DevThrottle, Tensorlake) and the forgecode GitHub README.
* Order of work suggested: #2 → #1 → #5 → #4 → #3 (they compound and are mostly in zen-agent/zen-tools), then #12 to measure, then #6, #7 and #8.
