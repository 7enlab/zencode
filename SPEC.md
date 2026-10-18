# zen-code — Behavioral Specification (clean-room)

Status: v1 (2026-10-14). Sources: public docs at code.claude.com/docs (headless, settings, hooks, cli-reference pages),
`claude --help` output (saved in `claude-help.txt`), and black-box observation via REA on Railway (see Appendix A).
No vendor source, system prompt, or tool description text was read or copied. All wording is original.
Naming: `CLAUDE.md` → `ZEN.md`, `.claude/` → `.zen/`, `~/.claude/` → `~/.zen/`, binary `claude` → `zen`.

Implementation target: Rust stable, single binary `zen`. Model access: OpenAI-compatible
`POST {baseUrl}/chat/completions` with `tools` (function calling) and optional SSE streaming.

---

## 1. CLI surface (clap)

```
zen [OPTIONS] [PROMPT]
```
| Flag | Behavior |
|---|---|
| `-p, --print` | Headless: run one prompt (from arg and/or stdin), print result, exit. Exit 0 on success, non-zero on failure. |
| `--output-format text\|json\|stream-json` | Only with `-p`. Default `text`. |
| `--input-format text\|stream-json` | Only with `-p`. `stream-json` reads NDJSON user messages from stdin. (MVP: `text` required, `stream-json` optional.) |
| `--verbose` | Extra diagnostics on stderr; in stream-json, emit every message. |
| `-c, --continue` | Continue most recent session for the current directory. |
| `-r, --resume [ID]` | Resume a session by id (or path to its `.jsonl`). No id in REPL → list sessions to pick. |
| `--session-id <uuid>` | Use a fixed session id. |
| `--fork-session` | With resume/continue: copy history into a new session id. |
| `--no-session-persistence` | Do not write transcript (only with `-p`). |
| `--model <name>` | Override model. |
| `--base-url <url>` / env `ZEN_BASE_URL` | OpenAI-compatible endpoint. |
| env `ZEN_API_KEY` (or `OPENAI_API_KEY`) | Bearer token; optional (some gateways inject auth). Never logged. |
| `--permission-mode default\|acceptEdits\|plan\|bypassPermissions\|dontAsk` | See §5. |
| `--allowedTools <rules...>` / `--disallowedTools <rules...>` | Comma/space separated permission rules. |
| `--tools <list>` | Restrict built-in tool set (`""` = none, `default` = all, `Read,Bash` = only those, `-Bash,-Write` / `+Name` = default set minus/plus; plain names and +/- deltas cannot be mixed). |
| `--system-prompt <text>` / `--append-system-prompt <text>` | Replace / append to the built-in system prompt. |
| `--add-dir <dirs...>` | Extra directories file tools may access. |
| `--mcp-config <file-or-json...>`, `--strict-mcp-config` | Load MCP servers; strict = only those. |
| `--settings <file-or-json>` | Session-only settings layer (§12). |
| `--setting-sources user,project,local` | Which file layers to load. |
| `--agents <json-or-file>` | Define sub-agents inline. |
| `--max-turns <n>` | Cap agent loop iterations (zen extension; default 50). |
| `--bare` | Skip ZEN.md, hooks, skills, custom commands, agents, MCP auto-discovery. |
| `-v, --version`, `-h, --help` | Standard. |

Subcommands (MVP subset): `zen mcp list|add|remove` (edit config), `zen doctor` (print loaded settings sources + model reachability), `zen export` / `zen import` (session portability, §10).

Stdin: when `-p` and stdin is not a TTY, read stdin (cap 10 MB, error if larger) and prepend to the prompt.

## 2. REPL (interactive)
- Started when no `-p`. Shows a prompt line; reads a line (multi-line via trailing `\` or paste).
- Input starting with `/` → slash command (§6). Input starting with `!` → run as shell command directly, output shown and added to context. `#text` → append a note to project ZEN.md (quick memory).
- Assistant text streams to stdout as it arrives; tool calls are shown as one-line summaries (`● Read(src/main.rs)`) followed by a truncated result preview.
- Permission prompts (§5) ask `Allow? [y]es / [a]lways (save rule) / [n]o`.
- Ctrl-C once: cancel the current turn (in-flight request/tool aborted, turn marked interrupted). Ctrl-C twice on empty line or Ctrl-D: exit.
- Every turn is persisted to the session transcript (§10).

## 3. Agent loop
1. Build request: system prompt (built-in + env info: cwd, OS, date, git status summary, + project snapshot + ZEN.md memory + skills list + `--append-system-prompt`), message history, tool schemas (built-in + MCP + Task). The system prompt is built once per session and sent byte-identical on every request (cache-friendly). **Project snapshot (r7, skipped with `--bare`):** a 2-level file tree (hidden dirs, `node_modules`/`target`/`dist`/`build`/`out`/`__pycache__`/`venv`/`vendor`/`coverage` and plain names from the top-level `.gitignore` omitted; ≤40 lines, ≤8 children per directory), build/test/lint commands detected from `Cargo.toml`, `package.json` scripts (npm/pnpm/yarn/bun by lockfile), `pyproject.toml`/`setup.py`/`requirements.txt`, `go.mod`, `pom.xml`, Gradle and Makefile targets, `--version` of the matching toolchains, test directories, and a line saying the work is judged by those checks; capped at 4000 characters. **Change workflow (r7 bench C2):** the built-in role prompt ends with a 5-step workflow for code changes — explore the relevant files, reproduce the problem (failing test or a tiny script), fix the root cause with the smallest change without editing/deleting/weakening tests unless asked, rerun the reproduction and the project's tests, check the edge cases the task implies — after https://www.anthropic.com/engineering/swe-bench-sonnet. `--system-prompt` replaces it with the rest of the role prompt.
2. Send to model. If response has `tool_calls`: for each call (in order) → run PreToolUse hooks → permission check → execute → PostToolUse hooks → append `role:"tool"` message with `tool_call_id` and textual result (errors returned as text with `is_error`-style prefix, never thrown away).
3. Loop until the model returns a message without tool calls, or `--max-turns` reached (then result subtype `error_max_turns`).
4. Run Stop hooks; a Stop hook exit 2 injects its stderr as a user message and continues the loop.
5. Token usage from `usage` is accumulated per turn. Cost reported as 0.0 for free models (configurable price table optional).
6. Retries: HTTP 429/5xx/network → exponential backoff (max 4 attempts). A **200 response that is really a failed sample** is retried the same way (r7 bench C3): a body with an `error` object instead of choices (vLLM returns gpt-oss `HarmonyError: unexpected tokens remaining in message header` like this; upstream advice is to catch and retry, https://github.com/vllm-project/vllm/issues/23567), no choices, or an assistant message with neither text nor tool calls; on the last attempt an empty message is returned as is and an error body becomes `model returned no choices (body: …)`; in stream-json emit `{"type":"system","subtype":"api_retry","attempt","max_retries","retry_delay_ms","error_status","error"}`.
7. Optional `fallbackModels` list: after retries are exhausted, try next model. A server wait hint (`Retry-After: <seconds>` header or `retry_after_seconds` in the JSON error body) raises the backoff delay for that retry, capped at 60 s (r7).
8. **Tool-call repair (r7).** Before a call is dispatched: an unknown tool name is mapped to a registered one when it differs only in case/punctuation, carries a `functions.`/`tools.` prefix, or is a common alias (`str_replace`→Edit, `bash`/`run_command`→Bash, `read_file`→Read, …); arguments wrapped in a code fence, with trailing commas, single quotes, Python literals, raw newlines in strings, missing closing brackets or double-encoded as a JSON string are repaired; values are coerced to the schema type (`"20"`→20, `"true"`→true, number→string, object→pretty JSON string, stringified array/object→value, also inside `edits[]`); near-miss keys are renamed (`filePath`/`path`→`file_path`, `cmd`→`command`); unknown keys are dropped when the schema has `additionalProperties:false`. A whole assistant message that is exactly one JSON tool call (`{"name","arguments"|"parameters"|"input"}`, optionally fenced or in `<tool_call>` tags) for a known tool is run as a tool call. A repaired call is stored repaired in history and its result ends with `[zen: your tool call was repaired before running: …]`. Unrepairable arguments → error result `Invalid JSON arguments for tool 'X': … Expected a JSON object with fields: name (type, required), …`; an unknown tool's error lists the available tools. **Leaked chat-template tokens (r7 bench C4):** before name matching, zero-width characters are removed, anything from `<|` on is cut and a `to=` prefix is dropped, so `Bash<\u200b|channel|>commentary`, `Read<|channel|>` and `assistant to=functions.Read` resolve to the real tool (https://github.com/vllm-project/vllm/issues/32587); the repair note then says `tool name had chat-template tokens; mapped to 'X'` without echoing the tokens, so the next request's history is clean.
9. **Loop guard (r7).** Within one user turn, the result of the 4th successful Edit/MultiEdit/Write to the same file, and of the 3rd identical failing call (same tool, arguments and error), gets one appended `[zen: …]` note asking the model to stop, re-read and reconsider. One note per file / per repeated failure per turn.
10. **Todo focus (r7).** When the session todo list is non-empty, the main agent appends it (`[zen: current todo list …]` + `[x]/[~]/[ ]` lines) to the newest tool/user message on every 6th model call since the list was last seen (TodoWrite resets the count), and the compaction summary always ends with it.
11. **Check-before-finish (r7).** At the point where the main agent would end the turn (after Stop hooks, not in plan mode): if settings `verify.command` is set and files were changed (successful Edit/MultiEdit/Write) since it last ran, the harness runs it via `sh -c` in the cwd with settings `env` (timeout `verify.timeoutSecs`, default 300); on failure the exit status and the last 40 output lines go back as a user message and the loop continues; at most 2 automatic runs per turn, after which a `Verify` system message notes the checks were not re-run. Without a command, one reminder per turn is sent when files changed and no Bash command ran since. The reminder is on by default in the REPL; in `-p` runs only when a `verify` object is configured; `verify.enabled:false` turns both off. Both use the Stop-hook continue path and its loop cap.
12. **Syntax guard (r7 bench C1).** Write/Edit/MultiEdit check the new content of `.py` (`python3` `ast.parse`), `.js/.mjs/.cjs` (`node --check`), `.json` (parser) and `.sh/.bash` (`bash -n`) files before writing (10 s timeout). If the new content fails and the old content passed (or the file is new), the write is refused with the checker's message and the numbered lines around the error; a file that was already broken is written as asked, and a missing checker never blocks. `ZEN_NO_SYNTAX_GUARD=1` turns it off. Source: SWE-agent ACI linter-gated edits (Yang et al., arXiv:2405.15793 §3).
13. **Empty-reply requery (r7 bench C5).** An assistant message with no text and no tool calls (after the client's own retries) is not taken as the answer: it is dropped from history and the user message `[zen: your last reply was empty. Continue the task: call a tool, or give your final answer.]` is sent; at most 2 times per turn, after which an empty answer ends the turn as before. After SWE-agent's format-error requery (https://swe-agent.com/latest/config/templates/).
14. **Unparsed-reply requery (r7 bench C7).** When the client's retries (item 6) are exhausted and the last failure is an unparsed sample (`model returned no choices …` or `gateway error in 200 response`, e.g. a gpt-oss Harmony header error on every attempt), the run does not end with `error_during_execution`: the user message `[zen: the server could not parse your last reply. Continue the task: make one tool call with plain JSON arguments, or give your final answer.]` is sent and the model is called again. It shares the 2-per-turn budget of item 13; once that is used up, or for any other error (network, auth, quota, 4xx), the run ends as before. Same sources as items 6 and 13.

## 4. Built-in tools (JSON schema params; names are zen's own)
| Tool | Params | Behavior |
|---|---|---|
| `Read` | `file_path` (abs or cwd-relative), `offset?` (1-based line), `limit?` (default 2000) | Returns lines prefixed with line numbers (`   12\t...`). Lines >2000 chars truncated. Errors on missing file / directory. Records the file as "read" for Edit/Write safety. |
| `Write` | `file_path`, `content` | Create/overwrite. If the file exists and was not Read in this session → error asking to read first; if it changed on disk since that Read (or zen's last write) → error asking to read it again. Creates parent dirs. |
| `Edit` | `file_path`, `old_string`, `new_string`, `replace_all?` | Exact string replace first. Error if found >1 times without `replace_all`, or identical to `new_string`. Requires prior Read; refuses a file whose content changed since zen last read/wrote it (r7). When the exact text is absent (r7), fallbacks are tried in order and used only if they match exactly one place: trailing whitespace / CRLF ignored; indentation ignored (`new_string` re-indented per level to the file's indentation, e.g. spaces→tabs); Read line-number prefixes stripped from `old_string`/`new_string`. The success text names the fallback used. On a miss the error lists up to 3 most similar regions with line numbers. Returns a short snippet around the change. |
| `MultiEdit` (r7) | `file_path`, `edits: [{old_string, new_string, replace_all?}]` | Applies the edits in order, each on the result of the previous one, with Edit's matching rules, then writes once. If any edit fails the file is untouched and the error names the failing edit (`edit 2 of 3`). Same read/staleness rules as Edit; treated as Edit by permission rules, `acceptEdits` and plan mode. |
| `Bash` | `command`, `timeout_ms?` (default 120000, max 600000), `description?`, `run_in_background?` | Runs via `bash -c` in a persistent cwd (a `cd` persists between calls). Returns combined stdout+stderr and exit code; output truncated to 30000 chars (middle elided). Timeout kills the process group. |
| `Glob` | `pattern`, `path?` | Glob match (`**/*.rs`), respects .gitignore, results sorted by mtime desc, capped at 1000. |
| `Grep` | `pattern` (regex), `path?`, `glob?`, `type?`, `output_mode` (`files_with_matches` default \| `content` \| `count`), `-i?`, `-n?`, `-A/-B/-C?`, `head_limit?` | Uses `rg` if on PATH else built-in regex walker (respect .gitignore). |
| `WebFetch` | `url`, `prompt?` | HTTP GET (follow redirects, 10 s timeout, 5 MB cap), HTML → readable text/markdown; optionally summarize with the model against `prompt`. Upgrades http→https. |
| `TodoWrite` | `todos: [{content, status: pending\|in_progress\|completed, activeForm?}]` | Replaces the session todo list; rendered in REPL; stored in session. At most one `in_progress` recommended. |
| `Task` | `description`, `prompt`, `subagent_type?` | Runs a sub-agent (§9) with its own fresh context and returns its final text only. |
| (MCP) `mcp__<server>__<tool>` | per server schema | §8. |
| `Skill` (optional) | `name` | Loads the named skill body into context (§7). |

Read-only tools (no permission needed in `default`): Read, Glob, Grep, TodoWrite, Task(launch), Skill. Mutating: Write, Edit, MultiEdit, Bash, WebFetch, MCP tools.

## 5. Permissions
**Modes**
- `default`: read-only tools run; Write/Edit/Bash/WebFetch/MCP ask (REPL) — in `-p` with no way to ask → denied, reason returned to the model.
- `acceptEdits`: Write/Edit auto-allowed (within working dirs); simple fs commands (`mkdir`, `touch`, `mv`, `cp`) auto-allowed; other Bash still asks.
- `plan`: no mutating tool runs; the model is told to research and present a plan; an `ExitPlanMode`-style tool (`ExitPlan{plan}`) ends plan mode after user approval.
- `bypassPermissions`: everything allowed (still subject to `deny` rules? → No: bypass skips checks, but `deny` rules still win — zen choice for safety).
- `dontAsk`: anything that would prompt is denied automatically; allow rules still apply.

**Rule syntax** (in settings `permissions.allow|ask|deny` arrays and CLI flags): `Tool` or `Tool(specifier)`.
- `Bash(npm run test)` exact; `Bash(git diff *)` prefix (space before `*` matters; `Bash(git diff*)` also matches `git diff-index`).
- `Read(./.env)`, `Edit(src/**)` gitignore-style path globs, relative to cwd; `//abs/path` absolute; `~/` home.
- `WebFetch(domain:example.com)`.
- `mcp__server` (all tools of server) / `mcp__server__tool`.
- Evaluation order: deny > ask > allow > mode default. Lists from all settings layers are merged.
- "Always allow" answer in REPL appends rule to `.zen/settings.local.json`.
- In `-p` json result, denied calls are listed in `permission_denials: [{tool_name, tool_use_id, tool_input}]`.

## 6. Slash commands
Built-in (REPL; `-p` supports those marked *): `/help`, `/clear` (new session, empty context), `/compact [instructions]`* (§11), `/model [name]`*, `/resume [id]`, `/continue`, `/cost`* (tokens used), `/status` (model, baseUrl, settings sources, mcp servers), `/permissions` (list rules), `/config key=value`*, `/memory` (open/print ZEN.md paths), `/init` (ask model to analyze repo and write ZEN.md), `/todos`, `/agents` (list sub-agents), `/mcp`* (server status), `/hooks` (list), `/skills` (list), `/diff` (git diff HEAD + untracked; `--staged`, `--stat`; `--turn` = last turn from its checkpoint), `/undo [--force]` (restore the files of the last file-changing turn, P0-3), `/exit`.

Custom commands: Markdown files in `.zen/commands/*.md` (project) and `~/.zen/commands/*.md` (user). Name = file stem; subfolders namespace as `dir:name`. Optional YAML frontmatter: `description`, `argument-hint`, `allowed-tools`, `model`. Body is the prompt; `$ARGUMENTS` replaced by all args, `$0..$9` / `$ARGUMENTS[N]` positional, 0-based (`$0` = first argument; see C5.2). Lines starting `!` followed by a backticked command are executed and output inlined; `@path` inlines file contents. In `-p`, a prompt starting with `/name` is expanded before running.

## 7. Memory (ZEN.md) and skills
**ZEN.md loading** (at session start, concatenated into system prompt with path headers):
1. `~/.zen/ZEN.md` (user)
2. Every `ZEN.md` and `.zen/ZEN.md` from filesystem root down to cwd (parents first)
3. `ZEN.local.md` in cwd (personal, gitignored)
- `@relative/path` lines inside ZEN.md import that file (max depth 5).
- Subdirectory ZEN.md files below cwd are loaded lazily when a tool reads a file in that subtree (optional).
- `/init` writes a starter ZEN.md; `#note` appends.

**Skills**: directories `~/.zen/skills/<name>/SKILL.md` and `.zen/skills/<name>/SKILL.md`. SKILL.md has YAML frontmatter `name`, `description` (required), optional `allowed-tools`, `disable-model-invocation`. Only name+description are put in the system prompt; the model loads the full body via the `Skill` tool (or user types `/<skill-name>` which inlines it). Supporting files in the skill dir are readable by path.

**Skill registry** (discoverability; `docs/skill-registry.md`): `zen skill search <query>` (ranked: exact name > prefix > substring > tag > description), `zen skill info <name>` (tier, permissions, source — shown before install), `zen skill install registry:<name> --yes` (resolves to `builtin:<name>` or the entry's git URL, then the standard install flow). Curated registry `skills/community-registry.json` is embedded in the binary (works offline); `--registry <path>` overrides with a local file. The registry is advisory: install re-computes the trust tier from the fetched manifest signature — a registry can never upgrade trust.

## 8. MCP client
- Config sources: `.mcp.json` in project root (`{"mcpServers": {name: cfg}}`), `~/.zen.json`/`~/.zen/mcp.json` user, `--mcp-config` files/JSON strings. `--strict-mcp-config` = only CLI ones.
- Server config: stdio `{ "command", "args"?, "env"? }` (type `stdio` default) or HTTP `{ "type": "http", "url", "headers"? }` (streamable HTTP; MVP may stub SSE).
- Protocol: JSON-RPC 2.0. `initialize` (protocolVersion, capabilities, clientInfo{name:"zen-code"}) → `notifications/initialized` → `tools/list` → `tools/call {name, arguments}`. Results' `content[]` text parts joined as tool result; `isError` respected.
- Tools exposed to the model as `mcp__<server>__<tool>` with the server's inputSchema.
- Startup timeout `MCP_TIMEOUT` (default 30 s); failed servers reported with status `failed` in `/mcp` and in stream-json init `mcp_servers: [{name, status}]`.
- `zen mcp add <name> -- <cmd> [args]`, `zen mcp add --transport http <name> <url>`, `zen mcp list`, `zen mcp remove <name>`; `-s local|project|user` scope.

## 9. Sub-agents
- Definitions: `.zen/agents/*.md` (project), `~/.zen/agents/*.md` (user), `--agents` JSON. Frontmatter: `name`, `description` (when to use), optional `tools` (comma list; omitted = inherit all), `model` (or `inherit`). Body = the sub-agent's system prompt.
- Invoked via `Task` tool (`subagent_type` = name; default `general-purpose` built-in). Sub-agent runs its own agent loop with fresh history, restricted tool set, same permission settings, cannot spawn further Task by default (depth limit 1, configurable).
- Only the final assistant text returns to the parent as the tool result. In stream-json, sub-agent messages are emitted with `parent_tool_use_id` set to the Task call id (main conversation messages have `null`).
- SubagentStop hook fires when it ends.

## 10. Sessions: persistence, continue, resume
- Transcripts: `~/.zen/projects/<cwd-encoded>/<session-uuid>.jsonl` where `<cwd-encoded>` = absolute cwd with `/` and `.` replaced by `-`.
- One JSON object per line: `{ "type": "user"|"assistant"|"tool"|"system"|"summary", "uuid", "parentUuid", "sessionId", "timestamp", "cwd", "message": {...OpenAI-style message...} }`.
- `--continue`: most recently modified transcript for the current cwd. `--resume <id>`: search all project dirs for `<id>.jsonl`, or accept a path. Resume reloads messages (after the last compaction summary) and continues appending to the same file unless `--fork-session`.
- Todo list and permission "always" rules persist with the session/settings respectively.
- **Portability** (`zen export` / `zen import`, format spec in `docs/session-format.md`): `zen export [--format json|markdown|html] [--session <id>] [-o file] [--redact]` renders any stored session — JSON (versioned portable document with `format_version`, the only importable format), Markdown (human-readable), or HTML (styled report). `--redact` strips potential secrets (provider keys, tokens, PEM private keys, `key=value` assignments) mirroring the account-safety redaction rules. `zen import <file.json>` validates `format_version` (refuses newer-than-zen and unsupported older versions) and restores the conversation as a NEW session id, leaving the original untouched.

## 11. Context compaction
- Track estimated tokens (usage from API, else chars/4). When history exceeds `autoCompactThreshold` (default 80% of model context window, configurable via settings `autoCompact` / `--autocompact`), auto-compact before the next request.
- Compaction: run PreCompact hooks; ask the model (no tools) to write a structured summary of the conversation so far (goal, decisions, files touched, open tasks, todo state); replace history with `[system prompt] + summary message (role user, marked as compaction summary) + last N (default 4) messages`. Write a `{"type":"summary"}` record to the transcript.
- `/compact [focus]` triggers manually; `focus` steers the summary.
- In stream-json emit `{"type":"system","subtype":"compact_boundary","trigger":"auto"|"manual","pre_tokens"}`.

## 12. Settings (settings.json layering)
Files (strict JSON):
1. User `~/.zen/settings.json`
2. Project shared `.zen/settings.json`
3. Project local `.zen/settings.local.json`
4. CLI `--settings <file-or-json>`
5. Managed `/etc/zen/managed-settings.json` (highest)
Precedence: managed > CLI > local > project > user. Scalars: highest layer wins. Arrays (e.g. `permissions.allow`) are concatenated/deduped across layers. Objects merged deeply. Specific flags (`--model`) and env (`ZEN_MODEL`) override the `model` key.
Keys (MVP): `model`, `baseUrl`, `apiKeyEnv` (name of env var holding key), `fallbackModels[]`, `permissions{allow[],ask[],deny[],defaultMode,additionalDirectories[]}`, `hooks{}`, `env{}` (exported into tool processes), `autoCompact` (bool or token count), `maxTurns`, `includeCoAuthoredBy`, `cleanupPeriodDays`.
Invalid file → warning on stderr, file skipped (in `-p` silently skipped, visible in `zen doctor`).

## 13. Hooks
- Configured in settings: `"hooks": { "<Event>": [ { "matcher": "<regex or tool name, optional>", "hooks": [ { "type": "command", "command": "...", "timeout": 60 } ] } ] }`.
- Events: `PreToolUse`, `PostToolUse` (matcher = tool name regex, e.g. `Edit|Write`, `mcp__.*`), `UserPromptSubmit`, `Stop`, `SubagentStop`, `SessionStart` (matcher: `startup|resume|clear|compact`), `SessionEnd`, `PreCompact` (matcher `manual|auto`), `Notification`.
- Command runs with `bash -c`, cwd = project dir, env `ZEN_PROJECT_DIR`. Stdin = JSON: `{session_id, transcript_path, cwd, hook_event_name, permission_mode, ...event fields}`; tool events add `tool_name`, `tool_input` (+ `tool_response` for Post); UserPromptSubmit adds `prompt`.
- Exit codes: `0` success (stdout shown in verbose; for UserPromptSubmit/SessionStart stdout is added to context); `2` blocking — PreToolUse: tool call blocked and stderr returned to the model; PostToolUse: stderr fed to the model; UserPromptSubmit: prompt rejected, stderr shown to user; Stop/SubagentStop: prevents stopping, stderr given to model as next instruction; other non-zero: non-blocking error, stderr shown to user, execution continues.
- JSON stdout (optional, exit 0): `{"continue": false, "stopReason": "..."}`, `{"decision": "block", "reason": "..."}`, PreToolUse `{"hookSpecificOutput": {"hookEventName":"PreToolUse","permissionDecision":"allow"|"deny"|"ask","permissionDecisionReason":"..."}}`.
- Hooks with the same event run in parallel; timeout default 60 s.

## 14. Headless output formats
**text**: final assistant text only, to stdout.

**json**: single object on stdout:
```json
{"type":"result","subtype":"success","is_error":false,"duration_ms":1234,"duration_api_ms":1100,
 "num_turns":3,"result":"<final text>","session_id":"<uuid>","total_cost_usd":0.0,
 "usage":{"input_tokens":0,"output_tokens":0},"model":"<model>","permission_denials":[]}
```
`subtype` ∈ `success`, `error_max_turns`, `error_during_execution`. Errors inside a run still print a result object (is_error true) and exit non-zero.

**stream-json**: NDJSON, one event per line:
1. `{"type":"system","subtype":"init","session_id","cwd","model","tools":[...],"mcp_servers":[{"name","status"}],"permissionMode","slash_commands":[...]}`
2. `{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text"},{"type":"tool_use","id","name","input"}]},"session_id","parent_tool_use_id":null}`
3. `{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id","content","is_error"}]},"session_id","parent_tool_use_id":null}`
4. optional `system/api_retry`, `system/compact_boundary`
5. last line: the same `result` object as json mode.
With `--include-partial-messages`: `{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text"}}}`.

## 15. Non-goals for MVP (explicitly out of scope / stub)
IDE integration, Chrome, cloud/teleport/remote-control, background sessions (`--bg`, `attach`), plugins/marketplaces, worktrees, auto-mode classifier, OAuth MCP, image input, voice, status line, output styles, `--json-schema` validation (optional), sandboxed bash.

## Appendix A — Evidence sources
- `claude --help` (v2.1.296) captured on Railway box → `claude-help.txt`.
- https://code.claude.com/docs/en/headless (output formats, result/init fields, retries, continue/resume)
- https://code.claude.com/docs/en/settings (layers, precedence, list merging)
- https://code.claude.com/docs/en/hooks (events, exit codes, JSON IO)
- REA black-box run (Claude Code on google/gemini-4-argon:free via relay) → `SPEC-rea.md` (merged notes in Appendix B).

## Appendix B — REA black-box findings (merged 2026-10-14; raw notes in SPEC-rea.md)
REA run: Claude Code 2.1.296 on Railway, 34 turns, black-box only (help output, docs pages, `-p` experiments in a scratch dir).
Model: started on google/gemini-4-argon:free; argon's free allowance ran out mid-run, so the final run was on nvidia/nemotron-3-super-120b-a12b:free (OpenRouter) via the relay chain.
Changes/additions adopted into zen (override the sections above where they differ):
- B1 (§1) `--allowed-tools` alias of `--allowedTools`; `--agent <name>` picks a sub-agent definition as the main agent; `zen mcp get <name>` and `zen mcp add-json <name> <json>`; `mcp add -e KEY=VAL` and `--header`.
- B2 (§6/§7) Custom commands and skills share the `/name` namespace; on a name clash the skill wins. Templates support `$ARGUMENTS`, 0-based positional `$0..$9` / `$ARGUMENTS[N]` (per C5.2), `${ZEN_SKILL_DIR}`, `${ZEN_PROJECT_DIR}`, `${ZEN_SESSION_ID}`, inline `` !`cmd` `` (run with permission checks) and `@path`.
- B3 (§13) Hook matcher: exact name, `A|B`, `*`/empty = all, otherwise regex. Tool-event stdin also carries `tool_use_id`. PreToolUse `hookSpecificOutput` may add `additionalContext` and `updatedInput` (replace tool input) besides `permissionDecision`.
- B4 (§5) Plan mode observed: mutating actions are not attempted; the agent writes a plan and the result is `subtype:"success"` with the plan text; `permission_denials` stays empty. zen: in plan mode, mutating tools are hidden from the tool list (not merely denied) and the result is the plan; plans may be saved to `~/.zen/plans/<session>.md`.
- B5 (§14) stream-json events carry `uuid` and `session_id` on every line; tool permission denials appear as `{"type":"system","subtype":"permission_denied","tool_name","tool_use_id","decision_reason"}`. The json result has many extra telemetry fields; zen keeps the documented core set (type, subtype, is_error, duration_ms, duration_api_ms, num_turns, result, session_id, total_cost_usd, usage, permission_denials) plus `model` and `uuid`, and adds `stop_reason`.
- B6 (§10) Transcript lines have top-level `type`, `sessionId`, `timestamp` (+ message payload); zen's record format in §10 is compatible in spirit (own field set).

## Appendix C — Behavior parity checklist (2026-10-14)
Sources (formats/behaviors only; nothing copied verbatim, no code or prompt text taken):
public repo github.com/anthropics/claude-code (README, examples/settings/*.json, examples/hooks/bash_command_validator_example.py,
plugins/*/.claude-plugin/plugin.json + hooks/hooks.json layout), and docs pages code.claude.com/docs/en/{hooks, settings, permissions,
memory, headless, commands, sub-agents, skills}. Names translated: CLAUDE.md→ZEN.md, .claude→.zen, CLAUDE_PROJECT_DIR→ZEN_PROJECT_DIR.
Priority: P1 = must have for parity of the core loop; P2 = should have; P3 = out of scope for this learning project (documented only).
Status column is maintained in PLAN.md ("Parity status").

### C1 Settings
- C1.1 P1 Layers & precedence: managed > --settings > local > project > user; scalars override, arrays (e.g. permissions.allow) merge across layers, objects deep-merge.
- C1.2 P1 Strict JSON; invalid file → warning, file skipped; individually invalid entries skipped, rest kept.
- C1.3 P1 Env/flag pairs: `ZEN_MODEL` env overrides settings `model`; `--model` overrides both.
- C1.4 P2 Keys honoured: model, env (exported to tools/hooks), permissions{allow,ask,deny,defaultMode,additionalDirectories,disableBypassPermissionsMode}, hooks, disableAllHooks, autoCompact, cleanupPeriodDays, includeCoAuthoredBy, zenMdExcludes (glob list of memory files to skip), fallbackModel(s).
- C1.5 P2 `permissions.defaultMode` of bypassPermissions is ignored when it comes from project/local settings (only user/managed/--settings/flag may enable it).
- C1.6 P2 `disableBypassPermissionsMode: "disable"` in any layer forbids bypassPermissions (flag rejected with an error).
- C1.7 P3 managed-only keys (allowManagedPermissionRulesOnly, allowManagedHooksOnly, strictKnownMarketplaces), sandbox block. Workspace trust: done as folder trust (README "Folder trust").

### C2 Permissions
- C2.1 P1 Order deny → ask → allow → mode default; first match wins; specificity does not matter.
- C2.2 P1 Bare tool deny (`Bash`, `Bash(*)`, `WebFetch`) removes the tool from the tool list sent to the model; scoped deny keeps the tool and blocks matching calls.
- C2.3 P1 Bash wildcard semantics: `*` matches any text incl. spaces, anywhere in the pattern; trailing ` *` also matches the bare command (`Bash(ls *)` matches `ls`, not `lsof`); `Bash(ls*)` matches `lsof`; `:*` suffix ≡ trailing ` *`.
- C2.4 P1 Compound commands split on `&&`, `||`, `;`, `|`, `|&`, `&`, newline: allow must match every part; deny/ask apply if any part matches; dangling `&&` → not split, allow cannot match.
- C2.5 P2 Wrapper stripping before matching: timeout, time, nice, nohup, stdbuf, command, builtin, bare xargs; deny/ask match past leading `VAR=value` assignments.
- C2.6 P2 Read-only Bash commands run without asking in default mode (ls, cat, echo, pwd, head, tail, grep, find (no -exec/-delete), wc, which, diff, stat, du, cd, read-only git: status/log/diff/show/branch).
- C2.7 P1 Path rules (gitignore syntax): `//abs`, `~/home`, `/relative-to-settings-root(cwd)`, `rel` or `./rel` relative to cwd; bare filename matches at any depth; `Read` deny also blocks Edit/Write and Grep/Glob on that path; `Edit` rules cover Write.
- C2.8 P1 `WebFetch(domain:x)` hostname match, case-insensitive; `*.x` = subdomains only; `domain:*` = all.
- C2.9 P1 MCP: `mcp__srv`, `mcp__srv__*`, `mcp__srv__tool`; deny/ask accept tool-name globs (`*`, `mcp__*`).
- C2.10 P2 `Agent(name)` / `Task(name)` rules gate sub-agent types.
- C2.11 P1 Modes: default (alias `manual`), acceptEdits (edits + mkdir/touch/mv/cp inside working dirs), plan (no mutations), dontAsk (would-ask → deny), bypassPermissions (deny rules still win); `auto` is P3 (classifier) — accepted as alias of default with a warning.
- C2.12 P1 "Always allow" from the REPL writes to `.zen/settings.local.json`; for a compound Bash command one rule per sub-command (max 5).
- C2.13 P1 PreToolUse hook decisions cannot override deny/ask rules; exit-2 block beats allow rules.

### C3 Hooks
- C3.1 P1 Config shape `hooks.<Event>[] = {matcher?, hooks:[{type:"command", command, timeout?(s, default 60), args?}]}`; `disableAllHooks: true` disables all.
- C3.2 P1 Events implemented: PreToolUse, PostToolUse, PostToolUseFailure, UserPromptSubmit, Stop, SubagentStart, SubagentStop, SessionStart (source startup|resume|clear|compact), SessionEnd (reason), PreCompact / PostCompact (trigger manual|auto), Notification, PermissionRequest (P2), PermissionDenied (P2). Others (Setup, ConfigChange, CwdChanged, FileChanged, Worktree*, Task*, Teammate*, Elicitation*, InstructionsLoaded, StopFailure, PostToolBatch, UserPromptExpansion) P3.
- C3.3 P1 Matcher: `*`/empty/omitted = all; only `[A-Za-z0-9_\- ,|]` → exact names separated by `|` or `,`; anything else → unanchored regex.
- C3.4 P1 Common stdin fields: session_id, transcript_path, cwd, permission_mode, hook_event_name; tool events add tool_name, tool_input, tool_use_id (+ tool_response on Post); UserPromptSubmit adds prompt; Stop/SubagentStop add stop_hook_active and last_assistant_message; SessionStart adds source; PreCompact adds trigger + custom_instructions.
- C3.5 P1 Exit codes: 0 ok (stdout parsed as JSON if it is a JSON object; for UserPromptSubmit/SessionStart plain stdout becomes context); 2 = block for blockable events (PreToolUse, UserPromptSubmit, Stop, SubagentStop) with stderr as the reason; PostToolUse exit 2 → stderr shown to model; other codes → non-blocking error (stderr to user only).
- C3.6 P1 Universal JSON fields: continue (false → stop run; stopReason shown), suppressOutput (accepted, no-op), systemMessage (warning to user; stream-json system line).
- C3.7 P1 PreToolUse `hookSpecificOutput`: permissionDecision allow|deny|ask (+ permissionDecisionReason; deny reason goes to the model), updatedInput replaces tool input; `defer` P3.
- C3.8 P1 PostToolUse: decision "block" + reason (added next to the result), additionalContext, updatedToolOutput replaces output.
- C3.9 P1 Stop/SubagentStop: decision "block" + reason (required) → continue; additionalContext → continue as feedback; loop guard: stop_hook_active=true on the re-run and at most 8 consecutive continuations.
- C3.10 P1 UserPromptSubmit: decision "block" + reason (shown to user, prompt not processed); additionalContext added.
- C3.11 P1 SessionStart: additionalContext added before the first prompt; initialUserMessage (P2) used as first turn in -p.
- C3.12 P1 Timeout → hook cancelled, output discarded, never blocks (unless `onFailure: "block"`, P2).
- C3.13 P1 Hook stdout/additionalContext capped at 10 000 chars (truncate + note).
- C3.14 P1 Env for hooks: ZEN_PROJECT_DIR (project root at session start), plus settings `env`.
- C3.15 P3 http / prompt / agent / mcp_tool hook types, async hooks.

### C4 Memory (ZEN.md)
- C4.1 P1 Load order: managed (/etc/zen/ZEN.md) → user (~/.zen/ZEN.md, ~/.zen/rules/*.md) → for each dir from filesystem root down to cwd: ZEN.md, .zen/ZEN.md, then ZEN.local.md last in that dir → project .zen/rules/*.md without `paths`.
- C4.2 P1 `@path` imports: relative to the importing file, absolute and `~/` allowed, max depth 4 hops, backslash-escaped spaces, imports inside code spans/fenced blocks ignored, cycle-safe.
- C4.3 P1 Block-level HTML comments `<!-- ... -->` stripped (kept inside code blocks).
- C4.4 P2 AGENTS.md fallback, decided per directory: for each directory from the filesystem root down to cwd that has none of ZEN.md / .zen/ZEN.md / ZEN.local.md, load AGENTS.md then .zen/AGENTS.md from that directory instead; a directory that has any ZEN file never also loads its AGENTS.md. At user level, ~/.zen/AGENTS.md is loaded only when ~/.zen/ZEN.md is absent.
- C4.5 P2 Subdirectory ZEN.md below cwd and path-scoped rules (`paths:` frontmatter) load lazily when Read/Write/Edit touches a matching file (injected into the next tool result).
- C4.6 P2 `zenMdExcludes` globs (absolute paths) skip files; managed file can't be excluded. Files > 4 MiB skipped.
- C4.7 P2 After compaction the project-root ZEN.md is re-injected (it lives in the system prompt in zen, so it survives automatically).
- C4.8 P3 auto memory (MEMORY.md), /memory editor integration.

### C5 Slash commands, custom commands, skills
- C5.1 P1 Built-ins: /help /clear /compact [focus] /config [k=v] /cost /status /model [name] /permissions /memory /init /agents /mcp /hooks /skills /resume [id] /exit, plus /context (token breakdown), /add-dir, /export (write transcript as markdown), /review (prompt template), /rename, /diff (git diff HEAD + untracked; --staged, --stat). Others P3.
- C5.1 P1 Built-ins: /help /clear /compact [focus] /config [k=v] /cost /status /model [name] /permissions /memory /init /agents /mcp /hooks /skills /resume [id] /exit, plus /context (token breakdown), /add-dir, /export (write transcript as markdown), /review (prompt template), /rename, /btw (side question, no history). Others P3.
- C5.1 P1 Built-ins: /help /clear /compact [focus] /config [k=v] /cost /status /model [name] /permissions /memory /init /agents /mcp /hooks /skills /resume [id] /exit, plus /context (token breakdown), /add-dir, /export (write transcript as markdown), /review (prompt template), /rename, /terse (toggle terse mode). Others P3.
- C5.1 P1 Built-ins: /help /clear /compact [focus] /config [k=v] /cost /status /model [name] /permissions /memory /init /agents /mcp /hooks /skills /resume [id] /exit, plus /context (token breakdown), /add-dir, /export (write transcript as markdown), /review (prompt template), /rename, /done (definition-of-done checklist: tests, lint, todos, scope, YAGNI), /scope (declared task scope). Others P3.
- C5.2 P1 Custom commands `.zen/commands/**.md`, `~/.zen/commands/**.md` (subdir → `dir:name`), frontmatter description, argument-hint, allowed-tools, model, disable-model-invocation; `$ARGUMENTS`, `$0..$9`/`$ARGUMENTS[N]` positional, `${ZEN_SESSION_ID}`, `${ZEN_PROJECT_DIR}`, `` !`cmd` `` pre-execution, `@file` inlining.
- C5.3 P1 Skills `.zen/skills/<name>/SKILL.md`, `~/.zen/skills/...`; frontmatter name, description, allowed-tools, disable-model-invocation (not listed for the model), user-invocable (false → hidden from `/`), argument-hint, model; skills and commands share `/name`, skill wins; `${ZEN_SKILL_DIR}`.
- C5.4 P1 In -p, `/name args` as the prompt is expanded; terminal-only built-ins unavailable; `/model x`, `/compact`, `/cost`, `/mcp` work.
- C5.5 P3 plugins (`.zen-plugin/plugin.json` with commands/, agents/, skills/, hooks/hooks.json and `${ZEN_PLUGIN_ROOT}`) — documented layout only; `--plugin-dir` P3.

### C6 Sub-agents
- C6.1 P1 Definition frontmatter: name, description (required), tools (allowlist; omitted = inherit), disallowedTools, model (alias/full/`inherit`), permissionMode, maxTurns, skills (preloaded), initialPrompt P3, memory P3, isolation P3, mcpServers P3, hooks P3.
- C6.2 P1 Scopes and priority: --agents JSON > project .zen/agents > user ~/.zen/agents > built-ins (general-purpose; optional Explore read-only, Plan read-only).
- C6.3 P1 Fresh context; returns only final text; cannot spawn nested sub-agents by default (depth limit); SubagentStart/SubagentStop hooks with agent_type as matcher target.
- C6.4 P1 stream-json: sub-agent tool_use/tool_result messages carry parent_tool_use_id = Task call id.
- C6.5 P2 `--agent <name>` runs the main session as that agent.

### C7 MCP
- C7.1 P1 Config files: project `.mcp.json`, user `~/.zen/mcp.json` (and `~/.zen.json` mcpServers), `--mcp-config` files/strings, `--strict-mcp-config`.
- C7.2 P1 Transports stdio (command,args,env) and http (url, headers); `sse` P2; `${VAR}` / `${VAR:-default}` expansion in command/args/env/url/headers.
- C7.3 P1 Validation: entry with url but no type → skipped (`url_missing_type`), unknown type → skipped (`unknown_type`); reported in init `mcp_server_errors` (key omitted when empty) and stderr warning when stderr is a TTY.
- C7.4 P1 init `mcp_servers: [{name, status}]` (connected|failed|pending); MCP_TIMEOUT startup timeout (default 30 s); -p waits for pending servers before the first turn.
- C7.5 P1 Tool naming `mcp__<server>__<tool>`; `zen mcp add|add-json|get|list|remove` with `-s local|project|user`, `-e KEY=VAL`, `--header`, `--transport stdio|http`.
- C7.6 P3 OAuth, resources/prompts (`@server:resource`, `/mcp__server__prompt`), elicitation, requiresUserInteraction.

### C8 Headless / output formats
- C8.1 P1 `-p` reads stdin when not a TTY (cap 10 MB → error, non-zero); exit 0 success, non-zero failure; invalid flag → error on stderr before the run; failures inside the run still print a result.
- C8.2 P1 json result fields: type "result", subtype (success | error_max_turns | error_during_execution), is_error, duration_ms, duration_api_ms, num_turns, result, session_id, total_cost_usd, usage, permission_denials[{tool_name, tool_use_id, tool_input}], plus zen's model, uuid, stop_reason.
- C8.3 P1 stream-json: first line `system/init` (session_id, cwd, model, tools, mcp_servers, permissionMode, slash_commands, apiKeySource?); then `assistant` / `user` lines (`message` + `parent_tool_use_id` + `session_id` + `uuid`); `system/api_retry` {attempt, max_retries, retry_delay_ms, error_status, error}; `system/compact_boundary`; `system/permission_denied`; last line the `result`.
- C8.4 P1 `--verbose` required semantics: zen accepts stream-json without --verbose (documented difference) .
- C8.5 P2 `--include-partial-messages`: `stream_event` lines with `content_block_delta`/`text_delta` (needs SSE streaming from the model; else no-op, documented).
- C8.6 P2 `--input-format stream-json` (NDJSON user messages on stdin), `--replay-user-messages` P3.
- C8.7 P2 `--json-schema` → `structured_output` field (validate JSON the model returns; on failure is_error).
- C8.8 P1 `--max-turns`, `--append-system-prompt`, `--system-prompt`, `--allowedTools`, `--disallowedTools`, `--tools`, `--permission-mode`, `--permission-prompts none` (P2), `--bare`, `--add-dir`, `--settings`, `--setting-sources`, `--mcp-config`, `--strict-mcp-config`, `--agents`, `--model`, `--fallback-model` (P2), `--session-id`, `--no-session-persistence`.
- C8.9 P1 SIGTERM → exit 143, running Bash child killed, SessionEnd hook run; SIGINT ends the current turn.

### C9 Sessions
- C9.1 P1 Transcript JSONL per session under `~/.zen/projects/<encoded cwd>/<uuid>.jsonl` (records carry type, uuid, parentUuid, sessionId, timestamp, cwd, message).
- C9.2 P1 `-c/--continue` newest session for the cwd; `-r/--resume <id|path>` (id searched across projects, or absolute path to a .jsonl); `--fork-session` new id with copied history; `--session-id <uuid>` (must be a valid UUID); `--no-session-persistence` writes nothing.
- C9.3 P1 Resume replays messages after the last compaction summary; cost/usage in json result accumulate over the whole conversation.
- C9.4 P2 `cleanupPeriodDays` (default 30) deletes older transcripts at startup.

### C10 Compaction
- C10.1 P1 Auto-compact threshold (`autoCompact` setting / `--autocompact auto|<tokens>`), manual `/compact [instructions]`.
- C10.2 P1 PreCompact hook (trigger, custom_instructions) before; PostCompact (P2) after; `compact_boundary` event with trigger + pre_tokens.
- C10.3 P1 Never leave orphaned tool messages; keep the last turn(s) verbatim; summary marked as such in the transcript.
