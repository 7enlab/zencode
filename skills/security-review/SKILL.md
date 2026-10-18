---
name: security-review
description: Pre-commit security checklist — secrets, injection, auth, and dependency risks before code ships
allowed-tools: read, grep, glob
---

# Security Review — Pre-Commit Checklist

Run through this list before any commit or PR. One FAIL blocks the ship.

## 1. Secrets & credentials

- [ ] No API keys, tokens, passwords, or private keys in the diff. Search for `sk-`, `AKIA`, `-----BEGIN`, `password\s*=`, `token\s*=`.
- [ ] No `.env` files, credential files, or `~/.ssh` paths added or modified.
- [ ] New config examples use placeholders (`<redacted>`, `YOUR_KEY_HERE`), never real values.

## 2. Injection

- [ ] No shell commands built by string-concatenating user input — use argument arrays.
- [ ] No SQL/HTML/JS built from untrusted input without parameterization or escaping.
- [ ] `eval`, `exec(`, `Function(`, and template injection sinks: none on user-controlled data.

## 3. Auth & access control

- [ ] New endpoints/handlers check authentication AND authorization (not just auth).
- [ ] No hardcoded bypasses (`if debug`, `// TODO: remove`, backdoor flags).
- [ ] Sensitive operations log an audit trail; failed auth attempts don't leak which half was wrong.

## 4. Dependencies & supply chain

- [ ] No new dependency without a reason — check what it pulls in transitively.
- [ ] Pinned versions for new deps; no `*` ranges, no git URLs without a pinned hash.

## 5. Data handling

- [ ] No PII (emails, names, IDs) in logs, error messages, or telemetry.
- [ ] User data that leaves the machine goes only to the destination the task named.

## Severity guide

- **BLOCKER:** secrets in diff, injection sink, auth bypass. Stop and fix now.
- **WARN:** new dependency, broadened file scope, elevated permission. Flag in the PR description.
- **OK:** everything else. Ship it.
