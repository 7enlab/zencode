# Read

Read a text file from disk and return its lines with 1-based line numbers.

## Parameters

- `file_path` (string, required): File to read, absolute or relative to the
  working directory.
- `offset` (integer, optional): 1-based line to start from. Defaults to 1.
- `limit` (integer, optional): Maximum lines to return. Defaults to 2000.

## Behavior

- Use `offset`/`limit` to page through large files instead of reading them
  whole - a full 10k-line dump costs ~10k tokens on every later turn.
- Binary files are refused; the result tells you the file is not text.
- Reading a file records it as *read* for this session. `Write` and `Edit`
  require the target to have been read first (stale-content protection: if the
  file changed on disk since you read it, the write is rejected).
- Respects `.gitignore` only in the sense that ignored files can still be read
  explicitly by path.

## Examples

Read the first 50 lines of a file:

```json
{ "file_path": "crates/zen-cli/src/main.rs", "limit": 50 }
```

Read lines 100–150:

```json
{ "file_path": "crates/zen-cli/src/main.rs", "offset": 100, "limit": 51 }
```

## Phase bundle

`read` (exploration). Full schema is sent in the `all`, `core`+`read` bundles.
