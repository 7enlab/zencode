# Hooks

ZenCode hooks are **guarantees the model can't skip**: shell commands that run at
fixed points of the agent loop — before/after tool calls, when a prompt is
submitted, when a turn ends, before compaction. Use them for validation,
auto-linting, context injection, end-of-turn checks, and backups.

This mirrors [Claude Code's hooks](https://code.claude.com/docs/en/hooks.md):
command handlers, per-tool matchers, JSON on stdin, exit-code protocol.

## Configuration

Hooks live in settings under the `"hooks"` key (project `.zen/settings.json`,
user `~/.zen/settings.json` — see [settings layering](./ARCHITECTURE.md)):

```jsonc
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          { "type": "command", "command": "./scripts/no-rm-rf.sh", "timeout": 10 },
          { "type": "command", "command": "python3", "args": ["scripts/lint.py"], "timeout": 60 }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Edit|Write",
        "hooks": [
          { "type": "command", "command": "cargo fmt --check 2>&1 | head -20", "timeout": 30 }
        ]
      }
    ],
    "UserPromptSubmit": [
      { "hooks": [{ "type": "command", "command": "echo \"Repo: $(basename $PWD). Today is $(date +%A).\"", "timeout": 5 }] }
    ],
    "Stop": [
      { "hooks": [{ "type": "command", "command": "./scripts/check-tests.sh", "timeout": 120 }] }
    ],
    "PreCompact": [
      { "hooks": [{ "type": "command", "command": "./scripts/backup-transcript.sh", "timeout": 30 }] }
    ]
  }
}
```

Field reference per hook entry:

| Field | Required | Description |
|---|---|---|
| `type` | yes | Must be `"command"`. Other types are ignored. |
| `command` | yes | Shell command run via `bash -c` — **unless** `args` is set, in which case the program is exec'd directly with no shell. |
| `args` | no | Array of arguments; bypasses the shell (no expansion, no injection). |
| `timeout` | no | Seconds before the hook is killed (default `60`). Timed-out output is discarded and never blocks, unless `onFailure` is `"block"`. |
| `onFailure` | no | `"block"`: a spawn failure or timeout blocks the event (with the error as the reason) instead of being a non-blocking error. Default: ignore. |

Global switches:

- `"disableAllHooks": true` (top-level settings) — no hook runs at all.
- `--trust-folder` / folder trust: in an **untrusted** folder, project hooks do not
  load (same rule as MCP servers and `env`). The REPL asks once and records trust
  in `~/.zen/trusted-folders.json`.

Matching commands for one event run **concurrently**; identical command+args are
deduplicated (run once).

## Matchers

`matcher` filters which tool (or trigger) a hook entry applies to. Applies to
`PreToolUse` / `PostToolUse` (matched against the **tool name**) and a few other
events (see table below).

| Matcher value | Meaning |
|---|---|
| absent, empty, or `"*"` | matches everything |
| `"Bash"` | exact tool name |
| `"Edit\|Write"` or `"Edit,Write"` | list of exact names (split on `\|` or `,`) |
| `"mcp__.*"` | anything else is an **unanchored regex** (invalid regex → never matches) |

```jsonc
{ "matcher": "Bash", "hooks": [...] }          // only Bash calls
{ "matcher": "Edit|Write|NotebookEdit", "hooks": [...] }  // exact list
{ "matcher": "mcp__github__.*", "hooks": [...] } // regex over MCP tools
```

## Events

### `PreToolUse` — validate before a tool runs

Fires **before every tool call**, with the tool name as the matcher target.
This is the validation layer: lint the arguments, deny dangerous commands,
rewrite inputs.

Stdin payload:

```jsonc
{
  "tool_name": "Bash",
  "tool_input": { "command": "rm -rf /tmp/x" },
  "tool_use_id": "call_abc123",
  // ...plus the standard fields below
}
```

Hook powers:

- **Block**: exit `2` (stderr = reason shown to the model) **or** stdout JSON
  `{"decision": "block", "reason": "..."}`. The tool never runs; the model gets
  the reason as a failed tool result and can try something else.
- **Rewrite input**: stdout JSON
  `{"hookSpecificOutput": {"updatedInput": {"command": "ls -la"}}}` replaces the
  tool arguments (used for the permission check and the call).
- **Permission override**: stdout JSON
  `{"hookSpecificOutput": {"permissionDecision": "deny|ask|allow",
  "permissionDecisionReason": "..."}}`.

Example — refuse destructive shell commands:

```bash
#!/bin/bash
# scripts/no-rm-rf.sh — PreToolUse, matcher "Bash"
input=$(cat)
cmd=$(echo "$input" | python3 -c "import json,sys; print(json.load(sys.stdin)['tool_input']['command'])")
if echo "$cmd" | grep -Eq '\brm +-rf\b|:\(\)\{.*\}&'; then
  echo "Refusing dangerous command: $cmd" >&2
  exit 2
fi
exit 0
```

### `PostToolUse` — react after a tool runs

Fires **after every tool call** (and `PostToolUseFailure` when the tool errored),
with the tool name as the matcher target. Use for auto-lint, test feedback,
notifications.

Stdin payload:

```jsonc
{
  "tool_name": "Edit",
  "tool_input": { "file_path": "src/main.rs", "old_string": "...", "new_string": "..." },
  "tool_use_id": "call_abc123",
  "tool_response": "Edited src/main.rs (12 lines changed)",
  // on failure the event is PostToolUseFailure and "error" replaces "tool_response"
}
```

Hook powers:

- **Feedback to the model**: exit `2` (stderr = message) **or** any stdout text via
  JSON — the message is appended to the tool result the model sees. It does
  **not** block; the tool already ran.
- **Replace output**: stdout JSON
  `{"hookSpecificOutput": {"updatedToolOutput": "[redacted]"}}` swaps what the
  model sees (e.g. strip secrets from tool output).

Example — run the formatter check after every edit:

```jsonc
"PostToolUse": [{
  "matcher": "Edit|Write",
  "hooks": [{ "type": "command",
              "command": "cargo fmt --check 2>&1 | head -5; test ${PIPESTATUS[0]} -eq 0 || exit 2",
              "timeout": 30 }]
}]
```

(A non-empty diff makes the hook exit 2; the diff text is fed back to the model
as part of the tool result.)

### `UserPromptSubmit` — inject context with the prompt

Fires when the user submits a prompt, **before the model sees it**.

Stdin payload:

```jsonc
{ "prompt": "fix the login bug" }
```

Hook powers:

- **Inject context**: any stdout on exit `0` (plain text, or
  `{"hookSpecificOutput": {"additionalContext": "..."}}`) is appended to the
  user's message before it reaches the model.
- **Block**: exit `2` (stderr = reason) rejects the prompt entirely.

Example — always tell the model what day it is and which repo it's in:

```bash
#!/bin/bash
# UserPromptSubmit
echo "Today is $(date +%A). Working in $(basename $PWD). Main branch is 'main', never push to it."
```

### `Stop` — end-of-turn checks

Fires when the model finishes a turn with no more tool calls.

Stdin payload:

```jsonc
{
  "stop_hook_active": false,
  "last_assistant_message": "Done. All tests pass."
}
```

Hook powers:

- **Re-prompt (continue the loop)**: exit `2` (stderr = instructions) sends the
  message back to the model as feedback and the turn continues. Guarded by a
  max-consecutive-continues limit so a misbehaving hook can't loop forever.
- **End with a message**: stdout JSON `{"continue": false, "stopReason": "..."}`
  stops the run; `stopReason` (or stderr) is what the user sees.

Example — don't let the model stop before tests pass:

```bash
#!/bin/bash
# Stop — re-prompt until cargo test is green
if ! cargo test --quiet 2>&1 | tail -3; then
  echo "Tests are failing. Fix them before stopping." >&2
  exit 2
fi
exit 0
```

### `PreCompact` — before compaction

Fires right before conversation compaction (manual `/compact` or automatic),
with the trigger (`"manual"` or `"auto"`) as the matcher target.

Stdin payload:

```jsonc
{
  "trigger": "auto",
  "custom_instructions": "",
  "pre_tokens": 48210
}
```

Hook powers:

- **Veto**: exit `2` skips this compaction (the turn continues on the full history).
- **Backup**: the classic use — copy the transcript elsewhere before it's summarized.

Example — archive the transcript before every compaction:

```bash
#!/bin/bash
# PreCompact — matcher "auto" (or "*" for both)
input=$(cat)
ts=$(date +%Y%m%d-%H%M%S)
sid=$(echo "$input" | python3 -c "import json,sys; print(json.load(sys.stdin)['session_id'])")
cp "$(echo "$input" | python3 -c "import json,sys; print(json.load(sys.stdin)['transcript_path'])")" \
   ~/.zen/backups/transcript-$sid-$ts.jsonl
exit 0
```

### Other events

| Event | When it fires | Matcher target |
|---|---|---|
| `SessionStart` | session begins | `"startup"` / `"resume"` / `"clear"` |
| `SessionEnd` | session ends | — |
| `SubagentStart` / `SubagentStop` | a sub-agent starts / finishes | agent type |
| `PostCompact` | compaction finished | `"manual"` / `"auto"` |
| `PermissionRequest` | a permission prompt is about to be shown | tool name |
| `PermissionDenied` | a tool call was denied by permissions | tool name |
| `Notification` | a notification is sent | — |

## Payload schema (stdin)

Every hook receives a single JSON object on stdin: the event-specific fields
above **plus** these standard fields:

```jsonc
{
  "event": "PreToolUse",            // (in hook_event_name; see note)
  "session_id": "sess-9f3a…",
  "transcript_path": "/home/u/.zen/sessions/….jsonl",
  "cwd": "/home/u/project",
  "hook_event_name": "PreToolUse",
  "permission_mode": "default",     // default|acceptEdits|plan|bypassPermissions|dontAsk
  "tool_name": "Bash",              // tool events only
  "tool_input": { "command": "ls" },// PreToolUse / PostToolUse only
  "tool_result": "…",               // PostToolUse: "tool_response" (or "error" on failure)
  "prompt": "fix the login bug"     // UserPromptSubmit only
}
```

> The task spec names this `{event, session_id, cwd, tool_name?, tool_input?,
> tool_result?}`; the implementation sends `hook_event_name` (same value) and
> additionally `transcript_path` and `permission_mode`. `tool_result` arrives as
> `tool_response` on success and `error` on `PostToolUseFailure`.

## Exit codes

| Exit | Meaning |
|---|---|
| `0` | Success. Stdout: parsed as JSON if it's a JSON object (see below); otherwise plain text becomes extra context **only** for `UserPromptSubmit` / `SessionStart`, ignored for other events. |
| `2` | **Block / intervene.** For `PreToolUse`, `UserPromptSubmit`, `Stop`, `SubagentStop`, `PreCompact`, `PermissionRequest`: stderr is the reason; the event is blocked (tool doesn't run / prompt rejected / turn continues / compaction skipped). For `PostToolUse` / `PostToolUseFailure`: stderr is fed back to the model (the tool already ran — nothing to block). For other events: non-blocking error, stderr to the user only. |
| other non-zero | Non-blocking error: stderr goes to the user, the agent loop continues. |
| timeout | Process killed, output discarded, never blocks (unless the hook sets `"onFailure": "block"`). |

## Stdout JSON protocol

On exit `0`, a JSON object on stdout controls the outcome:

```jsonc
{
  "continue": false,          // stop the run (Stop, UserPromptSubmit); …
  "stopReason": "…",          // …message shown to the user
  "systemMessage": "…",        // shown to the user, not the model
  "decision": "block",        // block this event…
  "reason": "…",              // …with this reason (also "approve" for PreToolUse)
  "hookSpecificOutput": {
    "permissionDecision": "allow|ask|deny",
    "permissionDecisionReason": "…",
    "updatedInput": { "command": "ls -la" },   // PreToolUse: replace tool args
    "updatedToolOutput": "[redacted]",         // PostToolUse: replace what the model sees
    "additionalContext": "…"                   // extra context; for Stop/SubagentStop it re-prompts the model
  }
}
```

Combining multiple hooks: **any** block wins; **any** `continue: false` stops;
permission precedence is deny > ask > allow; the first `updatedInput` /
`updatedToolOutput` in config order wins; feedback and system messages are joined
with newlines. Hook output fed back is capped at 10,000 chars per hook.

## Security notes

- Hook commands run with the user's privileges — only install hooks you trust,
  and prefer the `args` form (no shell) for anything handling untrusted input.
- The LLM API key is **scrubbed** from hook processes (env vars holding it are removed).
- Settings `env` is exported to every hook plus `ZEN_PROJECT_DIR` (the session cwd).
- In untrusted folders, project hooks don't load at all (folder-trust rule).
- Timeouts kill runaway hooks; a hook that exits without reading stdin is fine
  (broken pipe on write is not an error).

## See also

- `/hooks` in the REPL prints the currently loaded hook config.
- `docs/stream-json-v1.md` — hook activity surfaces as `system_message` events.
