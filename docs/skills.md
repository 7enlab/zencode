# ZenCode Skills

Skills are reusable bundles of instructions (methodology, checklists, habits)
that the agent loads on demand. ZenCode adopts the **open SKILL.md standard**
(Anthropic, Oct 2025 - same format as Claude Code / Codex / Cursor, so the
existing skill corpus works here) and adds three things nobody else ships
together: **declared permissions**, **trust tiers with signatures**, and
**built-in evals**.

## Format

A skill is a directory containing:

```
my-skill/
  SKILL.md          # required - instructions + YAML frontmatter
  zen-plugin.json   # required for install - identity + permissions + signature
  test.md           # recommended - CHECK-line evals run by `zen skill test`
```

### SKILL.md

```markdown
---
name: tdd
description: Test-driven development workflow
allowed-tools: read, write, edit, bash
---

# TDD
...instructions...
```

Frontmatter fields: `name`, `description` (required), `allowed-tools`
(comma list). The body is markdown the model reads when the skill is invoked.
Session start loads only frontmatter (~dozens of tokens/skill); the full body
loads on demand via the Skill tool.

### zen-plugin.json

```json
{
  "name": "tdd",
  "version": "1.0.0",
  "description": "Test-driven development workflow",
  "author": "ZenCode",
  "permissions": {
    "allowed_tools": ["read", "write", "edit", "bash"],
    "network": false,
    "file_write": true,
    "exec": true,
    "file_scope": "project"
  },
  "skills": ["."],
  "signature": "hex-encoded ed25519 signature (verified tier only)"
}
```

Permission fields are **declarative and enforced** (Codex-style), not advisory:

| Field | Meaning |
|---|---|
| `allowed_tools` | Tool names the skill may use. Empty = advisory only (no filtering). |
| `network` | May access the network (WebFetch etc.). |
| `file_write` | May write files (Write/Edit). |
| `exec` | May execute commands (Bash). Requires `bash` in `allowed_tools`. |
| `file_scope` | `"project"` (cwd only) or `"user"` (cwd + `~/.zen`). |

Validation rules: `exec: true` requires `bash` in `allowed_tools`;
`file_write: true` requires a write tool (`write`/`edit`/`multiedit`).

### test.md - skill evals

Deterministic checks, no model calls (free and reproducible):

```markdown
# Tests for tdd
- CHECK frontmatter.name == "tdd"
- CHECK manifest.permissions.network == false
- CHECK manifest.permissions.allowed_tools contains bash
- CHECK instructions contains "RED-GREEN"
- CHECK instructions not contains "rm -rf"
```

Grammar: `CHECK <target> <op> <value>` where op is `==`, `!=`, `contains`,
`not contains`. Targets: `frontmatter.<field>`, `manifest.<field>`,
`manifest.permissions.<field>`, `instructions`. Lists support
`contains`/`not contains` only.

## Trust tiers

| Tier | Badge | Meaning | Updates |
|---|---|---|---|
| `verified` | `[✓ verified]` | ed25519-signed by ZenCode (or ships with the release) | Allowed, diff shown first |
| `community` | `[community]` | Installed from git/local with your explicit `--yes` | Never automatic |
| `unverified` | `[⚠ unverified]` | No manifest | Never; sandboxed |

**Sandbox for unverified:** read-only tools (`read`, `glob`, `grep`), no
network, project scope - regardless of what the skill claims.

**The Plugin4Shell lesson:** there are no silent auto-updates in ZenCode.
`zen skill update` (planned) will show a diff before applying anything.
SHA-pinning alone was bypassed in Sept 2026; signatures + human-reviewed
diffs are the control that matters.

## Signing (verified tier)

The signature covers the **canonical manifest**: JSON with sorted keys,
sorted/deduped tool lists, compact separators, UTF-8 - the `signature` field
itself excluded. Verification uses the ed25519 public key baked into the
binary (`ZENCODE_SIGNING_PUBKEY_HEX` in `crates/zen-cli/src/skills.rs`).

Key management: the private half must live in offline/CI signing
infrastructure. Rotation = publish a new baked-in key + re-sign all verified
skills. The key currently baked in is the **initial development key**; it must
be replaced before v1.0.

## Commands

```bash
zen skill list                              # installed skills + tier badges
zen skill install builtin:tdd               # install a built-in skill
zen skill install https://github.com/you/skill.git --yes
zen skill install ./path/to/skill --yes     # shows permission diff first
zen skill test tdd                          # run the skill's evals
zen skill remove tdd
zen skill create my-skill                   # scaffold SKILL.md + manifest + test.md
```

Install always shows the permission diff **before** installing and requires
`--yes`. Without a manifest, the skill installs as `unverified` (sandboxed)
with a loud warning.

## Built-in skills

| Skill | What | Permissions |
|---|---|---|
| `tdd` | RED-GREEN-REFACTOR workflow | read/write/edit/bash, exec, no network |
| `token-diet` | Token-efficiency habits | read only (advisory) |
| `security-review` | Pre-commit security checklist | read/grep/glob, no write, no network |

All three are signed and ship `[✓ verified]`.

## Runtime

- Installed skills appear in the system prompt with tier badges (frontmatter only).
- Full bodies load via the Skill tool on demand.
- **Runtime enforcement (M1 security fix):** a session-scoped
  `SkillPermissionGate` wraps the session permission checker. Every time the
  model loads a registry-installed skill through the Skill tool, the skill is
  recorded and its declared `allowed_tools` are enforced on all subsequent
  tool calls - deny-by-default for undeclared tools (e.g. a skill declaring
  only `read` cannot drive `Bash`). Multiple loaded skills combine by
  **union**; advisory skills (empty `allowed_tools`) add no filtering.
  Sub-agents spawned from the session share the same gate state.
- `SkillPolicyChecker` (public API in `zen-cli::skills`) is the underlying
  allowlist wrapper; `SkillPermissionGate` applies it session-wide.
- Boundary: enforcement covers **registry-installed** skills (permissions the
  user approved at install time). Plain `.zen/skills/` directories without a
  registry entry are advisory - loading them adds no filtering.
