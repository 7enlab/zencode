# Grep

Search file contents with a regular expression.

## Parameters

- `pattern` (string, required): Regular expression to search for.
- `path` (string, optional): File or directory to search; defaults to the
  working directory.
- `mode` (string, optional): `files_with_matches` (default), `content`, or
  `count`.
- `glob` (string, optional): Restrict to files matching a glob, e.g. `*.rs`.

## Behavior

- Uses ripgrep when installed, otherwise a built-in walker; both respect
  `.gitignore`.
- `files_with_matches` returns just paths (cheapest); `content` returns
  matching lines with line numbers; `count` returns per-file hit counts.
- Read-only: no permission prompt in `default` mode.

## Examples

Find where a function is defined:

```json
{ "pattern": "fn tools_for_request", "mode": "content", "glob": "*.rs" }
```

## Phase bundle

`read` (exploration).
