# Contributing to ZenCode

Thank you for your interest in contributing to ZenCode! This document outlines
the process and expectations.

## Branch Policy

- **Never push directly to `main`.** All changes go through pull requests.
- Branch naming (English, kebab-case):
  - `dev/<name>` — developer tasks
  - `lead/<name>` — leader tasks
  - `hotfix/<name>` — urgent fixes
- One branch = one task = one PR.
- Create branches from the latest `main`.

## Pull Request Process

1. Before opening a PR, run `~/workspace/tools/gh-pace-check.sh` (anti-suspend:
   max 5 new PRs per day per repo).
2. PRs must be in **English** (title, description, comments, commits).
3. Link related issues. Keep the diff minimal — no unrelated refactors.
4. A leader reviews every PR. Developers do **not** self-merge.
5. CI must be green. Run before pushing:
   ```bash
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   ```
6. Merge strategy: squash. Delete the branch after merge.

## Code Standards

- Rust edition 2024. Follow `rustfmt` defaults.
- No `unwrap()` in production paths — use proper error types (`thiserror`/`anyhow`).
- No hardcoded secrets, API keys, or credentials. Ever.
- Cross-platform: code must work on Linux, macOS, and Windows.
  Use the `dirs` crate for paths, never hardcode `/home` or `C:\`.
- Stream-JSON contract (`docs/stream-json-v1.md`) is **additive only**.
  Changing it requires a separate PR and cross-team review.

## Clean-Room Rule

ZenCode is built clean-room. Never copy code, prompts, or tool descriptions
from Claude Code, Codex, or similar tools — even from memory of leaked sources.
Only public binaries, SDK types, official docs, and changelogs may be used
as references.

## Reporting Security Issues

See [SECURITY.md](SECURITY.md) and [docs/threat-model.md](docs/threat-model.md).
Do **not** open public issues for vulnerabilities.

## Release Checklist

No release ships until all five gates pass:

1. **context7** — dependencies and APIs checked against current docs; no deprecated usage.
2. **ocr** (OpenCodeReview) — full code review of the release diff.
3. **e2e** — full end-to-end test suite green on Linux, macOS, and Windows.
4. **rea** (REA research) — reverse-engineering/parity checks against reference behavior.
5. **bughunter** — security audit, including dependency scans:
   `cargo audit` and `osv-scanner scan source -r .` (OSV-Scanner, against OSV.dev)
   must both report no unresolved vulnerabilities.

## Benchmark Checklist

Before reporting any performance numbers publicly, answer these 7 questions
(adapted from pstack's benchmark-checklist):

1. **Why not double?** What is the limiter? If you doubled the resource, would the number double?
2. **Tuned?** Was the baseline tuned fairly, or set up to lose?
3. **Limits?** What are the error bars? What breaks at the edges?
4. **Errors?** What failed during measurement? Were failures excluded?
5. **Reproduce 5x?** Does the result hold across 5 independent runs?
6. **Matter?** Does this number change any decision? If not, don't report it.
7. **Did it even happen?** Can someone else reproduce this from your description?

Unverified numbers are never used in public materials (per project policy).
