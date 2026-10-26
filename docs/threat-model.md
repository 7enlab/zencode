# Threat Model - ZenCode CLI

This document tells security scanners (human or automated) what to test, which
inputs are adversarial, what is out of scope, and how to classify severity.
It follows the threat-model pattern used by Anthropic's OSS Scanner program.

## What ZenCode is

A local AI coding CLI. It runs with the user's own privileges on their machine,
talks to model providers over the network (OpenAI-compatible and Anthropic
Messages APIs), reads and writes files in the working directory, and executes
shell commands proposed by a language model.

## Assets to protect

1. **User API keys and credentials** - provider keys, OAuth tokens. These must
   never be written to logs, plain-text config files outside the secret store,
   or sent to any endpoint other than the provider they belong to.
2. **The user's filesystem outside the working directory** - the agent must
   not read or write outside approved roots without explicit user approval.
3. **The user's shell** - commands are proposed by a model; execution must be
   gated by the permission system, never automatic for destructive actions.
4. **Integrity of the stream-json contract** - the Desktop app parses CLI
   output; malformed or injected records are a correctness and security issue.

## Adversarial inputs (treat as untrusted)

- **Model output** - tool calls and shell commands proposed by the model are
  untrusted until the permission layer approves them. This includes output
  from *any* provider, including local models.
- **Repository content** - files in a cloned repo (READMEs, comments, embedded
  instructions) may contain prompt-injection attempts. Content read from disk
  is data, not instructions.
- **MCP server responses** - external MCP servers are third-party code; their
  tool results are untrusted input.
- **Provider responses** - a compromised or malicious endpoint can return
  crafted responses; treat API payloads as untrusted data.
- **Environment variables and config files** - may be attacker-controlled in
  shared or CI environments; validate before use in command construction.

## In scope for testing

- Permission/approval gating for shell execution and file writes.
- Secret storage: OS keyring usage, file fallback permissions (0600), redaction
  in logs and error messages.
- Path handling: traversal outside the working directory, symlink following,
  Windows/macOS/Linux path differences (cross-platform parity is a hard rule).
- Command construction: shell injection via filenames, arguments, or config.
- The account-safety warnings (login risk prompts) - they must fire before
  risky patterns, not after.
- Dependency vulnerabilities (tracked in CI via cargo audit + OSV-Scanner).

## Out of scope

- Attacks that require already having local code execution as the same user
  (the CLI itself is that level of trust).
- Social engineering of the user to approve a malicious command (the approval
  UI is the mitigation; bypassing the UI is in scope, the user's choice is
  not).
- Vulnerabilities in model providers' servers.
- Denial of service by exhausting the user's own API quota (rate and cost
  controls are a product feature, tracked separately).

## Severity rubric

| Severity | Definition |
| --- | --- |
| Critical | Credential exfiltration, or command execution without user approval |
| High | Write outside approved roots, secret written to log/plain text, approval-gate bypass requiring user interaction |
| Medium | Prompt-injection path that reaches a gated action but still requires approval; information disclosure of non-secret local data |
| Low | Hardening gaps, missing redaction of non-sensitive data, unclear warning copy |

## How to report

See [SECURITY.md](../SECURITY.md). Do not open public issues for vulnerabilities.

Reports are most useful when they include: the trust boundary crossed, the
exact input, the code path (file/function), and a minimal reproduction.
Prefer a merge-ready minimal patch proposal over a proof-of-concept exploit.
