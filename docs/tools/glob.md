# Glob

Find files by glob pattern.

## Parameters

- `pattern` (string, required): Glob pattern such as `**/*.rs` or `*.toml`,
  relative to the working directory.

## Behavior

- Skips `.git`, `target`, `node_modules` and gitignored trees.
- Results are sorted newest-first and capped at 1000 paths.
- This is a read-only tool: no permission prompt in `default` mode.

## Examples

```json
{ "pattern": "crates/*/src/*.rs" }
```

## Phase bundle

`read` (exploration).
