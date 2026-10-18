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

### Folder trust

Project config that can run commands, widen permissions or redirect provider requests only loads in a folder you trust:
project `hooks`, `permissions.allow`, `permissions.additionalDirectories`, `env` and `verify` from
`.zen/settings*.json`, the provider endpoint `baseUrl` and `apiKeyEnv` (a repo could otherwise send
your API key elsewhere; your `~/.zen` values apply instead), MCP servers from `.mcp.json` / `.zen/mcp.json`, and `allowed-tools` of
project commands/skills. Everything from `~/.zen/` always loads; project deny rules always apply.

- Interactive: when such config is present, zen asks once at startup `Trust this folder? [y/N]`.
  Yes is saved to `~/.zen/trusted-folders.json` (covers subfolders; never written to the repo).
- Headless `-p`: never asks; untrusted unless `--trust-folder` or `ZEN_TRUST_FOLDER=1` is given
  (or the folder is already saved). Skipped config is named in one line on stderr; stream-json
  `system/init` carries `folder_trusted: bool`. `zen doctor` shows the state.
- CI/scripts that rely on a repo's own hooks or MCP servers: add `--trust-folder` (or set
  `ZEN_TRUST_FOLDER=1`).

## Layout

| Crate | What it owns |
|---|---|
| `zen-core` | shared types and traits |
| `zen-agent` | HTTP client, agent loop, compaction, sub-agent runner |
| `zen-tools` | Read/Write/Edit/Bash/Glob/Grep/WebFetch/TodoWrite + permission rules |
| `zen-ext` | settings layering, ZEN.md memory, skills, hooks |
| `zen-mcp` | MCP client, sub-agent definitions, Task tool |
| `zen-cli` | the `zen` binary: CLI, REPL, slash commands, output formats, sessions |

## Account Safety

ZenCode is designed to be a good API citizen so your accounts stay in good standing.

**What ZenCode does automatically:**
- Exponential backoff with jitter on rate limits (never hammers through 429s)
- Never retries billing/quota errors
- Identifies itself with a versioned User-Agent on every request
- Caps concurrent requests per provider (default: 2)
- Uses Anthropic prompt caching to reduce rate-limit pressure

**What you should do:**
- One API key per machine; never share keys
- Don't run many parallel sessions on a single key
- Keep payment methods current — chargebacks trigger automatic suspensions
- If a key may be leaked, revoke it at the provider immediately

**What gets accounts banned (so you can avoid it):**
Providers suspend accounts for payment disputes, shared/resold keys, bot-like traffic, and ToS violations (e.g., model distillation, subscription-token relay) — not for normal coding-agent usage within rate limits. [Full details →](docs/account-safety.md)

> No tool can guarantee your account won't be flagged — providers use automated systems with false positives. ZenCode minimizes the risk factors within its control and warns you about the rest.
