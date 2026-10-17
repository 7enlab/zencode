# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-10-17

First tagged release, so downstream crates (zencode-desktop) can pin a tag instead of a commit.

### Added
- `docs/stream-json-v1.md`: the `zen -p --output-format stream-json` contract, with sample
  streams in `docs/fixtures/stream-json/` produced by the real binary. A test regenerates the
  fixtures and fails on any difference, so the doc cannot drift from the output.
- Continue on truncation: a reply cut off by the output token limit is continued and stitched;
  a cut-off tool call is never run; `maxContinuations` (default 3) caps it with a clear note
  (PR #3).
- Durable tool results: every tool result is appended to the session file the moment it finishes, so a turn that dies
  mid-tool resumes with the results it already had (PR #2).
- MCP output cap: oversized MCP tool results are capped and spilled to a file, like Bash output (PR #1).

### Changed
- **Breaking for exhaustive matches:** `zen_core::AgentEvent` is now `#[non_exhaustive]`.
  Downstream code that matches on it must add a wildcard arm (`_ => {}`). New event kinds can
  then be added in a minor release without breaking your build.

### Fixed
- Windows: the Bash tool, background jobs, hooks, the REPL `!cmd` and the shell syntax guard no longer start the WSL
  launcher (`C:\Windows\System32\bash.exe`, "Windows Subsystem for Linux has no installed
  distributions") instead of Git Bash. The shell is now resolved to the full path of the first
  `bash` on PATH outside `%SystemRoot%`, falling back to `%ProgramFiles%\Git\bin\bash.exe`.

[Unreleased]: https://github.com/atzenn/zencode/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/atzenn/zencode/releases/tag/v0.1.0
