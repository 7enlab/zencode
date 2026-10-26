# MultiEdit

Apply several replacements to one file in a single call.

## Parameters

- `file_path` (string, required): File to edit.
- `edits` (array, required): Ordered list of `{old_string, new_string,
  replace_all?}` objects with the same matching rules as `Edit`.

## Behavior

- Edits run in order, each on the result of the previous one.
- Atomic: if any edit fails, nothing is written.
- The file must have been read earlier in this session.
- Prefer over N sequential `Edit` calls: one round-trip, one permission
  check, one file write.

## Examples

```json
{
  "file_path": "src/main.rs",
  "edits": [
    { "old_string": "let x = 1;", "new_string": "let x = 2;" },
    { "old_string": "let y = 3;", "new_string": "let y = 4;" }
  ]
}
```

## Phase bundle

`write` (implementation).
