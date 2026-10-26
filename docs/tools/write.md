# Write

Create a file or overwrite it completely.

## Parameters

- `file_path` (string, required): File to write, absolute or relative to the
  working directory.
- `content` (string, required): Full new content of the file.

## Behavior

- Parent directories are created automatically.
- If the file already exists, it must have been read earlier in this session
  (via `Read`); otherwise the call is rejected. This prevents clobbering files
  you have not seen.
- The write is atomic: a temp file is written and renamed over the target.
- Large content is fine, but prefer `Edit` for small changes to large files -
  rewriting a whole file costs tokens for the full content.

## Examples

```json
{ "file_path": "notes/todo.md", "content": "# Todo\n\n- [ ] write docs\n" }
```

## Phase bundle

`write` (implementation). Needs `Read` first for existing files.
