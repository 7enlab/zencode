# zen-code

A clean-room terminal coding agent written in Rust, built as a learning project. It mirrors the
*observable behavior* of tools like Claude Code (REPL, agent loop with tool calling, permissions,
hooks, ZEN.md memory, skills, MCP, sub-agents, headless JSON output, sessions, compaction) without
using any of their code, prompts or tool descriptions. It talks to any OpenAI-compatible
`/chat/completions` endpoint and is meant to be used with free models.

* Behavior spec: [`SPEC.md`](SPEC.md) (Appendix C is the parity checklist)
* How it works: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)

## Build

```bash
cargo build --release          # binary: ./target/release/zen
cargo test --workspace         # unit + end-to-end tests (no network needed)
cargo clippy --workspace --all-targets -- -D warnings
```

## Run

```bash
# defaults: base URL https://api.experientiallabs.ai/api/v1, model google/gemini-4-argon:free
./target/release/zen                                   # interactive REPL
./target/release/zen -p "list files and summarize README" --output-format json
echo "explain this log" | ./target/release/zen -p --output-format text
./target/release/zen -p "continue" --continue          # resume the latest session here

# any OpenAI-compatible endpoint + free model
ZEN_BASE_URL=https://openrouter.ai/api/v1 ZEN_API_KEY=... \
  ./target/release/zen -p "hi" --model nvidia/nemotron-3-super-120b-a12b:free
```

Configuration lives in `~/.zen/settings.json`, `.zen/settings.json`, `.zen/settings.local.json`
(see SPEC §12), project memory in `ZEN.md`, custom commands in `.zen/commands/`, skills in
`.zen/skills/<name>/SKILL.md`, sub-agents in `.zen/agents/*.md`, MCP servers in `.mcp.json`.

## Layout

| Crate | What it owns |
|---|---|
| `zen-core` | shared types and traits |
| `zen-agent` | HTTP client, agent loop, compaction, sub-agent runner |
| `zen-tools` | Read/Write/Edit/Bash/Glob/Grep/WebFetch/TodoWrite + permission rules |
| `zen-ext` | settings layering, ZEN.md memory, skills, hooks |
| `zen-mcp` | MCP client, sub-agent definitions, Task tool |
| `zen-cli` | the `zen` binary: CLI, REPL, slash commands, output formats, sessions |
