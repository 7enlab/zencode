# ZenCode Skill Registry

The community skill registry is the **discoverability layer** for ZenCode
skills. The skills system ([`docs/skills.md`](skills.md)) handles install trust;
the registry answers "what skills exist?".

```bash
zen skill search tdd                # ranked search: name > tag > description
zen skill search "commit"           # finds commit-msg, security-review, ...
zen skill info security-review      # tier, permissions, source — BEFORE installing
zen skill install registry:tdd --yes   # resolves + reuses the standard install flow
```

## How it works

- A **curated registry** (`skills/community-registry.json`) ships with the
  binary and is embedded at compile time, so `zen skill search` works offline.
  It lists the 3 built-in verified skills plus reviewed community examples.
- `zen skill info <name>` shows the full entry — trust tier, declared
  permissions, source — so you can decide **before** installing.
- `zen skill install registry:<name> --yes` resolves the entry to
  `builtin:<name>` or its git URL and runs the **unchanged** install flow:
  permission diff preview, explicit `--yes`, hash pinning.
- `--registry <path>` overrides the bundled registry with a local file in the
  same schema (e.g. your team's private registry).

## Trust model

The registry is **advisory**. The tier displayed comes from the registry entry,
but the tier actually assigned at install is recomputed by `assign_tier()` from
the fetched `zen-plugin.json` manifest and its ed25519 signature. A registry
claiming `verified` for an unsigned skill installs as `community` or
`unverified` — **a registry can never upgrade trust**. This is what makes a
future open HTTP registry safe to browse.

## Registry schema

`skills/community-registry.json`:

```json
{
  "format_version": 1,
  "updated": "2026-10-17",
  "skills": [
    {
      "name": "tdd",
      "description": "Test-driven development workflow — write the failing test first",
      "version": "1.0.0",
      "author": "ZenCode",
      "tier": "verified",
      "tags": ["testing", "workflow", "quality"],
      "permissions": {
        "allowed_tools": ["bash", "edit", "read", "write"],
        "network": false,
        "file_write": true,
        "exec": true,
        "file_scope": "project"
      },
      "source": { "kind": "builtin" }
    },
    {
      "name": "commit-msg",
      "description": "Conventional-commit message generator from staged diffs",
      "version": "0.3.0",
      "author": "zencode-community",
      "tier": "community",
      "tags": ["git", "commits", "workflow"],
      "permissions": {
        "allowed_tools": ["bash", "read"],
        "network": false,
        "file_write": false,
        "exec": true,
        "file_scope": "project"
      },
      "source": { "kind": "git", "url": "https://github.com/zencode-community/skill-commit-msg" }
    }
  ]
}
```

Field rules (validated on load — invalid registries are rejected with the
reasons listed):

| field | rule |
|---|---|
| `format_version` | must equal `1` (this zen's supported version) |
| `name` | `[a-zA-Z0-9_-]+`, unique (case-insensitive) |
| `description`, `version` | non-empty |
| `tier` | `verified` \| `community` \| `unverified` (advisory — see Trust model) |
| `permissions` | same shape as `zen-plugin.json`; `file_scope` is `project` or `user` |
| `source` | `{"kind": "builtin"}` or `{"kind": "git", "url": "https://…"}` / `git@…` |

Search scoring: exact name (100) > name prefix (75) > name substring (50) >
exact tag (40) > description substring (20); ties break alphabetically.

## Future: HTTP registry

The schema is designed so a hosted registry is a drop-in replacement:

1. **Transport**: `GET https://registry.zencodego.com/v1/registry.json`
   returning the same document. Client sends `If-None-Match` with the cached
   ETag; the server answers `304 Not Modified` when nothing changed.
2. **Cache**: `~/.zen/skill-registry-cache.json` + ETag, refreshed at most once
   per `--registry-refresh-hours` (default 24). Offline → stale cache is used
   with a warning; never a hard failure.
3. **Registry signature**: the registry document itself gets an ed25519
   signature over its canonical bytes (same scheme as skill manifests), so a
   mirror can't silently alter tiers or URLs. Unsigned registries are treated
   as `unverified` sources: entries install, but `verified` claims are ignored.
4. **Config**: `~/.zen/config` (or env `ZEN_SKILL_REGISTRY_URL`) selects the
   registry URL; the bundled curated registry remains the offline fallback.

Until then: download the registry JSON and pass `--registry <local-path>`.
HTTP URLs passed to `--registry` today fail with a message pointing here.
