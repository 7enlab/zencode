# Bash

Run a shell command with `bash -c` in the session working directory.

## Parameters

- `command` (string, required): Shell command to run.
- `timeout_ms` (integer, optional): Kill the command after this many
  milliseconds. Defaults vary by settings.
- `background` (boolean, optional): Run as a background job; the call returns
  a job id at once. Read output with `JobOutput`, stop with `JobKill`.

## Behavior

- The working directory is persistent: a `cd` inside one command is still in
  effect for the next one.
- Long output is cut to its head and tail; the full text is saved to a file
  whose path is reported, along with the exit code.
- This is a mutating tool: in `default` permission mode it may prompt; in
  `plan` mode it is hidden entirely.
- Prefer `Read`/`Glob`/`Grep` for inspecting files — spawning a shell costs a
  round-trip and its output is billed as context.

## Background jobs

```json
{ "command": "cargo test --workspace", "background": true }
```

returns `{ "job_id": "abc123" }`. Then:

- `JobOutput` with `{ "job_id": "abc123" }` polls new output.
- `JobKill` with `{ "job_id": "abc123" }` stops it.

## Phase bundle

`exec` (verification). Related: `JobOutput`, `JobKill`.
