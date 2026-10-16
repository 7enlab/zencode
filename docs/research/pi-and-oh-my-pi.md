# Pi and oh-my-pi: what zen took, what it left

Notes from reading the public docs of the Pi coding agent and the oh-my-pi fork (2026-10). Only
ideas were taken; zen's code, prompts and wording are its own (clean-room rule).

## Adopted

| Idea | How it looks in zen | Why |
|---|---|---|
| Context files written for other agents should just work | SPEC C4.4: a directory with no `ZEN.md`, `.zen/ZEN.md` or `ZEN.local.md` loads its `AGENTS.md` (then `.zen/AGENTS.md`) instead. Decided per directory, so a repo can mix both; a directory never loads both. `~/.zen/AGENTS.md` stands in for a missing `~/.zen/ZEN.md`. (`zen-ext/src/memory.rs`) | Many repos already ship `AGENTS.md`; zero setup for users, no duplicated instructions. |
| Summarize a transcript, not a conversation | Compaction flattens history into one user message of `[User]` / `[Assistant]` / `[Tool result]` lines (tool results capped) with no tools and no system prompt. (`zen-agent/src/compaction.rs`) | Free models tended to "continue the chat" or emit tool calls instead of a summary; the request is also much smaller. |
| Track file lists outside the model | Paths from `Read` and `Write`/`Edit` calls are appended to the summary as `<read-files>` / `<modified-files>` and merged with the previous summary's lists. | Deterministic; survives repeated compactions without trusting the model's memory. |
| Tool-set deltas on the command line | `--tools -Bash,-Write` or `+Name` adjusts the default set; plain names still mean "only these"; mixing the two forms is an error. (`zen-cli/src/agent_run.rs`) | Removing one risky tool is the common case; listing every other tool is error-prone. |

## Skipped (for now)

| Idea | Why not |
|---|---|
| Session tree (branching sessions inside one file) | zen's linear JSONL plus `--fork-session` already covers the observable behavior in SPEC; a tree changes the transcript format and resume logic for little gain. |
| RPC mode (driving the agent over stdin/stdout JSON-RPC) | `-p --output-format stream-json` already serves scripted use; C8.6 (stream-json input) is the planned path. |
| TypeScript extensions | Would pull a JS runtime into a Rust binary; hooks, skills, custom commands and MCP already cover extension needs. |
| Hash-anchored ("hashline") edits | Interesting for weak models, but zen's `Edit` is exact unique-string replacement by spec and tests depend on it; would need its own spec item and evaluation. |
| LSP / DAP integration | Large surface (process management, per-language servers); out of scope for a learning project. Reachable later through MCP. |
| Advisor / second "reviewer" model | Doubles API calls; zen targets free models with tight rate limits. `fallbackModels` and sub-agents with a custom `model` cover the nearby need. |
