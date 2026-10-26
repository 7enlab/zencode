# Plugin Curation

ZenCode's skill registry has two tiers. This document describes how the
**curated** tier works: how to submit, how reviews are done, and when entries
are removed.

## The two tiers

| | `curated` | `community` |
|---|---|---|
| Reviewed by maintainers | ✅ yes | ❌ no |
| Review notes public | ✅ in `registry-curated.json` | - |
| Badge version-scoped | ✅ (`reviewed_version`) | - |
| Install warning | standard permission diff | stronger unreviewed warning |
| Search | `zen skill search` (badge `[curated ✓]`) | `zen skill search` (badge `[community]`) |

`zen skill search --curated` shows only the curated tier.

## Review criteria

Every curation review must cover all four criteria and record a verdict for
each in the entry's `review_notes`:

### 1. Security (blocking)

- No data exfiltration: the skill must not send user data anywhere the task
  didn't name. Network access is allowed only if declared in `zen-plugin.json`
  and justified in the review notes.
- No credential access beyond declared permissions.
- Declared permissions match actual tool use - no `bash` in the manifest while
  the SKILL.md only reads files (over-broad), and no undeclared tools in use
  (under-declared).
- No obfuscated payloads, no base64-encoded blobs without a clear reason.
- No prompt-injection vectors: the skill must not instruct the agent to ignore
  safety boundaries, skip permission checks, or hide actions from the user.

### 2. Quality

- Ships a deterministic eval (`test.md` with CHECK lines) that passes.
- Instructions are concrete and falsifiable - a reader can tell whether the
  agent followed them.
- No conflicts with built-in skills; idempotent where it matters.

### 3. Documentation

- `SKILL.md` complete: name, description, usage, and definition of done.
- `zen-plugin.json` manifest accurate (name, version, author, permissions).
- Changelog for updates.

### 4. Maintenance

- Responsive author: acknowledges critical issues within 2 weeks.
- Versioned releases; no pinned-to-`main` git URLs.
- No abandoned transitive dependencies.

## How to submit

1. Publish your skill at a public git URL with `SKILL.md` and `zen-plugin.json`.
2. Run the self-assessment: `zen skill test <name>` must pass, and you must be
   able to answer all four criteria above for your own skill.
3. Open a **Curation request** issue on `atzenn/zencode` with:
   - skill name, repo URL, version submitted
   - self-assessment against the four criteria (one paragraph each)
   - any permission broader than `read`/`grep`/`glob`, with justification
4. A maintainer runs automated checks (manifest valid, `test.md` passes,
   permission diff sane) and then a human review.
5. Decision:
   - **Approve** → entry added to `registry-curated.json` with reviewer name,
     date, and review notes; the `[curated ✓]` badge is live.
   - **Request changes** → specific, actionable feedback; resubmit when done.
   - **Reject** → reasons given; may resubmit after addressing them.

The curated badge is **version-scoped**: it applies to `reviewed_version` only.
A minor or major version bump requires re-review (automated checks + a
lighter human pass focused on the diff).

## Removal policy

| Trigger | Action |
|---|---|
| Security vulnerability or malicious behavior | Immediate delist + security advisory |
| Author unresponsive to a critical issue for 6+ months | Demote to `community` |
| Quality regression (evals fail on new version) | Request changes; 30-day window, then demote |
| Author requests removal | Remove |
| Proven malicious intent | Delist + block by content hash |

Delisted entries are recorded in the registry's changelog with the reason and
date. `zen skill search` never shows delisted entries.

## Registry file format

`registry-curated.json` extends the community `registry.json` entry with a
`curation` block:

```json
{
  "format_version": 1,
  "updated": "2026-10-17",
  "entries": [
    {
      "name": "tdd",
      "description": "...",
      "version": "1.0.0",
      "author": "ZenCode",
      "trust_tier": "curated",
      "permissions": ["read", "write", "edit", "bash"],
      "source": "builtin:tdd",
      "curation": {
        "reviewer": "ZC Leader (ZenCode maintainer)",
        "review_date": "2026-10-17",
        "review_notes": "Security: PASS. ... Quality: PASS. ... Documentation: PASS. ... Maintenance: PASS. ...",
        "reviewed_version": "1.0.0"
      }
    }
  ]
}
```

Rules enforced by `zen`:

- `trust_tier` must be `"curated"` inside this file.
- `reviewer` must be non-empty; `review_date` must be a valid `YYYY-MM-DD`.
- `review_notes` must cover all four criteria and be at least 100 characters -
  a real review, not a rubber stamp.

Users can extend the registry at `~/.zen/registry-curated.json` (same format;
entries override the embedded ones by name). This is intended for teams
running their own internal curation, not for self-awarding the badge.
