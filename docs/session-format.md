# ZenCode Session Export Format

**Version:** 1 (`format_version: 1`)
**Status:** stable - v1 is frozen; breaking changes get a new version number.

No major coding agent ships session portability: transcripts are locked into
one tool's internal format. ZenCode's export format is documented and versioned
so that any tool - another agent, a script, or a future ZenCode web UI - can
read a session without knowing zen's internals.

## Producing / consuming

```bash
zen export --format json --session <id> -o session.json   # portable document
zen export --format markdown -o session.md               # human-readable
zen export --format html -o session.html                 # styled report
zen export --redact -o session.json                     # strip secrets first
zen import session.json                                 # restore as a NEW session
```

`--session` defaults to the most recent session for the current directory.
Only `--format json` documents can be imported. Import always creates a new
session id and prints it; the original session is never modified.

## Document shape (JSON)

```jsonc
{
  "format_version": 1,            // integer; check BEFORE parsing anything else
  "exported_by": "zen 0.1.0",    // producer identification
  "exported_at": "2026-10-17T13:56:38Z",  // RFC 3339
  "session_id": "0633e9d0-…",    // the original zen session id
  "cwd": "/home/user/project",   // working directory (may be absent)
  "model": "provider/model",     // from the first journal `request` record (may be absent)
  "usage": {"input_tokens": 150, "output_tokens": 75},  // summed over the session
  "cost_usd": 0.0,               // absent when no pricing data is available
  "redacted": false,             // true when exported with --redact
  "messages": [
    {
      "type": "user",            // "user" | "assistant" | "tool" | "summary" | "system"
      "timestamp": "2026-10-17T13:00:00Z",  // may be absent
      "uuid": "…",               // may be absent
      "payload": { "role": "user", "content": "…" }
    }
  ]
}
```

### Message kinds

| `type` | `payload` | Notes |
|---|---|---|
| `user` | OpenAI-style message (`role`, `content`) | content may be a string or a list of blocks |
| `assistant` | OpenAI-style message, may include `tool_calls` | |
| `tool` | `{"role":"tool", "name"?, "tool_use_id"?, "content", "is_error"?}` | journal `tool_result` records are normalized to this shape |
| `summary` | the compaction summary (string or `{"summary": "…"}`) | written when context was compacted |
| `system` | system note payload | usage records are aggregated into `usage`, not listed |

Journal metadata (`tools`, `request`, `response`, `response_error`) is not
exported as messages: the model name is harvested from `request`, usage from
the per-turn `system`/`usage` records (deltas - they are summed).

### `format_version` contract

- Consumers MUST check `format_version` before parsing and refuse versions
  newer than they understand.
- `zen import` refuses exports with `format_version` newer than its own
  (`EXPORT_FORMAT_VERSION`) with a clear "upgrade zen" error, and refuses
  older versions that are no longer supported.
- v1 will never change shape. A breaking change ships as v2.

## `--redact`

Strips potential secrets from every string in the export (mirrors the
account-safety redaction rules):

- Provider keys: `sk-ant-…` (Anthropic), `sk-…` (OpenAI), `AIza…` (Google),
  `xoxb-/xoxp-/xoxa-/xoxr-…` (Slack), `ghp_/gho_/ghu_/ghs_/ghr_…` (GitHub),
  `glpat-…` (GitLab), `hf_…` (Hugging Face), `AKIA…` (AWS)
- `Authorization: Bearer <token>` headers
- PEM private-key blocks (`-----BEGIN … PRIVATE KEY-----` … `-----END …-----`)
- `key = value` / `key: value` assignments for `api_key`, `token`, `secret`,
  `password`/`passwd`/`pwd`, `auth` (case-insensitive)
- JSON object keys naming a secret (`"api_key": "…"` → `"[REDACTED]"`)

Redacted values become `[REDACTED]` (or `[REDACTED PRIVATE KEY]`); the document
sets `"redacted": true`. Redaction is best-effort pattern matching - review
before publishing an export that touched real credentials.

## Import semantics

`zen import file.json`:

1. Parses and validates the document (`format_version` check).
2. Creates a NEW session id in the current directory's project dir.
3. Writes a leading `system` record noting the import origin and original
   session id, then replays every message as a transcript record
   (`summary` messages go back to the `summary` field, the rest to `message`).
4. Prints the new session id.

The imported session is a faithful copy of the conversation; it is not
linked to the original and resuming it does not affect the original.
