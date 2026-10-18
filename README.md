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

# cost dashboard: per-session token usage and estimated cost
./target/release/zen cost                        # latest session
./target/release/zen cost --session <id>         # a past session
./target/release/zen cost --output-format json   # machine-readable

# spend cap: hard-stop the session before a model call would breach it
./target/release/zen -p "refactor everything" --max-budget-usd 2.00
ZEN_MAX_BUDGET_USD=5 ./target/release/zen        # same via env
# config file: { "maxBudgetUsd": 5, "budgetWarnAt": 0.8 } in ~/.zen/settings.json
# precedence: --max-budget-usd > ZEN_MAX_BUDGET_USD > maxBudgetUsd
# --budget-warn-at 0.5 warns once at 50% (default 0.8). Free models never trip the cap.
```

### Ollama (local models, $0)

Run fully offline with [Ollama](https://ollama.com) — no API key, no rate
limits, no data leaving your machine:

```bash
ollama pull qwen3-coder:30b
./target/release/zen --model ollama/qwen3-coder:30b
# or: ./target/release/zen --provider ollama --model llama3.1
```

The `ollama/` prefix (or `--provider ollama`) routes to the local instance
(`OLLAMA_HOST`, default `http://localhost:11434`). On startup zen checks the
model is pulled and prints `ollama pull <name>` when it isn't. `zen doctor`
shows `provider: ollama (local)`.

Configuration lives in `~/.zen/settings.json`, `.zen/settings.json`, `.zen/settings.local.json`
(see SPEC §12), project memory in `ZEN.md`, custom commands in `.zen/commands/`, skills in
`.zen/skills/<name>/SKILL.md`, sub-agents in `.zen/agents/*.md`, MCP servers in `.mcp.json`.

### Named providers

`--model provider/model` routes directly to a named provider's API when that
provider's key env var is set (the `provider/` prefix is stripped on the wire).
Without the key, the value is treated as a gateway model ID on the default
endpoint. An explicit `--base-url` / `ZEN_BASE_URL` / settings `baseUrl`
always wins over the provider default.

```bash
ZAI_API_KEY=<key> ./target/release/zen -p "hi" --model zai/glm-5.2
MOONSHOT_API_KEY=<key> ./target/release/zen -p "hi" --model moonshot/kimi-k2.7
DEEPSEEK_API_KEY=<key> ./target/release/zen -p "hi" --model deepseek/deepseek-v4-flash
```

| Provider | Key env var | Models (examples) | Pricing/1M in/out |
|---|---|---|---|
| `zai` — Z.ai (Zhipu) | `ZAI_API_KEY` | `glm-5.2`, `glm-4.6` | $1.40 / $4.40 (glm-5.2) |
| `moonshot` — Moonshot AI | `MOONSHOT_API_KEY` | `kimi-k2.7`, `kimi-k3` | $0.95 / $4.00 (k2.7) |
| `deepseek` — DeepSeek | `DEEPSEEK_API_KEY` | `deepseek-v4-flash`, `deepseek-reasoner` | **$0.14 / $0.28** (v4-flash) |
| `openrouter` | `OPENROUTER_API_KEY` | any | varies |
| `groq` | `GROQ_API_KEY` | any | varies |
| `anthropic` | `ANTHROPIC_API_KEY` | `claude-opus-4-6`, ... | varies |
| `openai` | `OPENAI_API_KEY` | any | varies |
| `ollama` | _(none — local)_ | any local model | $0.00 |
| `together` / `mistral` / `cerebras` / `fireworks` / `google` | `<PROVIDER>_API_KEY` | any | varies |

All three Chinese providers document Anthropic-compatible endpoints
(`https://api.z.ai/api/anthropic`, `https://api.moonshot.ai/anthropic`,
`https://api.deepseek.com/anthropic`); ZenCode uses their OpenAI-compatible
endpoints today and switches to the Anthropic endpoint automatically once an
Anthropic client is available. Pricing feeds the `zen cost` ledger; see
`crates/zen-agent/src/providers.rs` for the registry, featured models for
`zen models`, login order for `zen auth`, and cheap-first fallback order.
### `/diff` (REPL)

In the interactive REPL, `/diff` shows uncommitted changes as `git diff HEAD`, plus a list of
untracked files (parity with Claude Code and Codex CLI, which both ship `/diff`):

- `/diff` — working tree vs HEAD (staged + unstaged), then untracked files
- `/diff --staged` — staged changes only (`git diff --cached`)
- `/diff --stat` — one-line summary per file instead of the full diff

Output is colored when stdout is a terminal and `NO_COLOR` is unset. Outside a git repo it
prints a hint instead of failing.

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

## `zen models` — free-model auto-discovery

Free-model catalogs rotate constantly, so `zen models` probes the configured
provider and tells you which free model actually works *right now*:

```bash
./target/release/zen models              # list candidates (uses cache when fresh)
./target/release/zen models --refresh    # re-probe every candidate free model
./target/release/zen models --best       # print just the recommended model id
```

Probing runs 3 tiny micro-tasks per model (instruction following, arithmetic,
code completion), sequentially with short timeouts — a few hundred tokens per
model, respectful of free-tier rate limits. Results are cached in
`~/.zen/models-cache.json` (re-probed when the cache is older than 24h or the
provider changes). A model is only recommended when *every* probe succeeded.

```bash
# use the recommended model for a run
./target/release/zen -p "hi" --model $(./target/release/zen models --best)
```
### Context debugger

`zen context` shows where your tokens actually went — an observable, attributable
view of what the model saw. Token totals come from the API's reported usage when
available, otherwise from a `chars/4` estimate (the output says which).

```bash
./target/release/zen context                        # latest session for this directory
./target/release/zen context --session <id>         # a specific session
./target/release/zen context --output-format json   # machine-readable for UIs
```

Example output:

```
Context: 42,310 tokens (reported by API) — session 9f3a…
├── system prompt: 1,180 (2.8%)
├── tool schemas (12): 8,400 (19.9%)
│   ├── bash: 325
│   └── …
└── history (14 turns): 31,780 (75.1%)
    ├── turn 5 tool (tool_result (read src/main.rs)): 5,120 [PRUNABLE?]
    │     → 5120 tokens and 'src/main.rs' is not referenced by any later message
    └── …

Compactions (1):
  1. 2026-10-17T10:02:00Z — 38,200 tokens → 100 summary tokens (38,100 dropped)

Top token consumers — where did your tokens go?
  1. tool_result (read src/main.rs) — 5,120 tokens
     → Large result no longer referenced — a future prune pass could summarize or drop it.
```

Large tool results that no later message references are flagged `[PRUNABLE?]`
(heuristic). Compactions show what each summary kept versus dropped.

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
