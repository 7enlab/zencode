# Edit

Replace an exact string inside a file.

## Parameters

- `file_path` (string, required): File to edit.
- `old_string` (string, required): Exact text to replace. Must occur exactly
  once in the file, unless `replace_all` is true.
- `new_string` (string, required): Replacement text.
- `replace_all` (boolean, optional): Replace every occurrence. Defaults to false.

## Behavior

- Matching is exact and whitespace-sensitive - copy the target text verbatim,
  ideally from a fresh `Read`.
- `old_string` occurring 0 times is an error; occurring 2+ times without
  `replace_all` is an error (no silent partial edits).
- The file must have been read earlier in this session.
- For several edits to one file, prefer a single `MultiEdit` call (one
  permission check, one write).

## Examples

```json
{
  "file_path": "src/main.rs",
  "old_string": "let x = 1;",
  "new_string": "let x = 2;"
}
```

## Phase bundle

`write` (implementation).
