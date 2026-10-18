# zen-code

A clean-room terminal coding agent written in Rust, built as a learning project. It mirrors the
*observable behavior* of tools like Claude Code (REPL, agent loop with tool calling, permissions,
hooks, ZEN.md memory, skills, MCP, sub-agents, headless JSON output, sessions, compaction) without
using any of their code, prompts or tool descriptions. It talks to any OpenAI-compatible
`/chat/completions` endpoint and is meant to be used with free models.

* Behavior spec: [`SPEC.md`](SPEC.md) (Appendix C is the parity checklist)
* How it works: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)

## Why ZenCode

**Free is structural, not promotional.** In 2026, every major "free" AI coding tier got
narrowed or killed: Gemini CLI shut down overnight (June 18), OpenCode locked free models
to its app (September), Qwen Code ended its 2,000 req/day free tier (April 15). ZenCode
can't rug-pull you — there is no central API to shut off. You bring your own endpoint:
free cloud models, your paid keys, or Ollama running fully offline on your machine.

**The Four Questions** (after the [Gemini CLI postmortem](https://www.jenariusganlary.com/blog/gemini-cli-shut-down-choosing-ai-coding-tools)):

| Question | ZenCode's answer |
|---|---|
| Is the backend as open as the code? | Yes — no backend. Any OpenAI-compatible endpoint, or local models. |
| Am I the customer, or am I the churn? | The customer: your keys, your models, your machine. |
| What breaks the day it dies? | Nothing. The repo and your models keep working. |
| What's my exit cost? | Near zero. Open formats, portable sessions. |

More: [`docs/positioning.md`](docs/positioning.md) · [`docs/faq.md`](docs/faq.md) ·
[`docs/messaging.md`](docs/messaging.md)

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
### `/btw` side questions

`/btw <question>` asks the model a quick side question without disturbing the main
conversation: only a short system prompt and the question are sent (never the session
history), no tools are offered, and nothing is written to the transcript.

```text
> /btw what does exit code 137 mean?
Out of memory — the process was killed (OOM).
```

Keep it cheap with a small model: `--btw-model <name>` or the `btw.model` setting
(`{"btw": {"model": "org/tiny-model"}}` in settings); otherwise the session model is used.
### Terse mode and output budget

Output tokens cost 3-5x input tokens on most price sheets, so output discipline has
outsized dollar impact:

- `--terse` (or `/terse` in the REPL to toggle mid-session) appends a maximal-concision
  instruction to the system prompt: no preamble, no restated context, no unasked summaries.
- `--max-output-tokens N` sets a hard per-request cap (provider-level `max_tokens`).
  Sub-agents inherit the cap from their parent.

```bash
zen -p "summarize this repo" --terse --max-output-tokens 500
```

The built-in system prompt is distilled (every behavioral rule kept, ~20% fewer tokens than
the original) and `/context` shows the system-prompt token cost per session.
### Ephemeral mode (`--ephemeral`)

For sensitive work: nothing session-related is written to disk. No transcript, no session
directory, nothing under `~/.zen` — everything stays in memory and is discarded on exit.
A startup banner (`EPHEMERAL MODE — nothing will be saved`) confirms the mode is active.

```bash
./target/release/zen --ephemeral                      # interactive REPL, zero trace
./target/release/zen -p "review this" --ephemeral     # headless, zero trace
```

Notes:

- `--ephemeral` implies `--no-session-persistence` and works in both REPL and `-p` modes.
- `/cost` and `/context` keep working (in-memory usage); `/export` still writes the file you
  explicitly ask for; `/resume` is refused (there is nothing to resume); tool scratch falls
  back to a temp directory instead of `~/.zen/projects/...`.
- Spend caps (`--max-budget-usd`) are still enforced from in-memory usage.
- Explicit user actions still write files on request: `/export <file>`, `#note` (project memory),
  `/config` (project settings). Ephemeral covers *session* persistence, not files you ask for.

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
### `@file` mentions and `!shell` passthrough (REPL)

Two Claude Code-style shortcuts keep you in flow without spending a model round-trip:

**`@<glob>` — attach file contents to your prompt.** Mentions are expanded before the
prompt is sent; each file is wrapped in a `<file path="..." tokens="~N">` block with a
path header and token count.

```
> review @src/main.rs
@src/main.rs (42 tokens)

> what changed in @src/**/*.test.ts since yesterday?
@src/a.test.ts (120 tokens)
@src/b.test.ts (88 tokens)
```

- Multiple `@` mentions per prompt; globs (`*`, `**`, `?`, `[...]`) supported.
- `.gitignore` is respected (ignored files never match).
- A pattern that matches nothing is an error naming the pattern (`@nope/*.rs matched
  no files`) — the turn is not sent.
- Binary files become a `[binary file — contents not included]` placeholder; files over
  512 KiB are truncated.
- Token budget: expansions over the budget only warn. Configure with
  `{"mentions": {"tokenBudget": 20000}}` in settings (default 10 000).

**`!cmd` — run a shell command immediately.** No model round-trip: the command runs at
once, output prints, and stdout/stderr is appended to the conversation as a
`shell_passthrough` record so the *next* turn sees it.

```
> !npm test
...output...
> why did that test fail?
```

- Runs under the current permission mode: deny rules and plan mode refuse up front,
  `Ask` prompts `[y]es / [a]lways / [n]o` (same as the `Bash` tool), `bypassPermissions`
  runs directly.
- Cheaper than asking the agent to run Bash: zero input/output tokens for the command
  itself.
### Plan mode

Plan mode is **mechanically** enforced read-only — the harness withholds `Write`/`Edit`/state-changing
`Bash`/… from the model, and rejects any mutating call that slips through with a tool error
(the tool never executes). It is not model politeness.

- REPL: `/plan` toggles plan mode; the prompt shows a `[PLAN]` badge while active.
- Research, then present a plan ending with `ExitPlanMode`. Reply `approve` (or `yes`) to accept it:
  the plan is recorded in the session transcript and plan mode exits into `acceptEdits`.
- Start the REPL in plan mode: `"permissions": { "defaultMode": "plan" }` in `.zen/settings.json`.
### Memory (ZEN.md)

Project memory loads hierarchically — later files override/extend earlier ones:

1. `/etc/zen/ZEN.md` (managed, organisation-wide)
2. `~/.zen/ZEN.md` (user), then `~/.zen/rules/**/*.md`
3. For each directory from the filesystem root down to cwd: `ZEN.md`, `.zen/ZEN.md`, then `ZEN.local.md`
4. Project rules `.zen/rules/**/*.md` (root down to cwd)

A directory with no ZEN.md-family file falls back to `AGENTS.md` (then `.zen/AGENTS.md`),
so repositories written for other agents work unchanged; `~/.zen/AGENTS.md` stands in for a
missing `~/.zen/ZEN.md`. Files support `@path` imports (max 4 hops, cycles safe).

- `/memories` — list every loaded memory file with its token count and the total,
  so you can see exactly what the model sees.
- `/memory` — list the loaded memory file paths.
- `/init` — scaffold (or improve) the project `ZEN.md`. If `CLAUDE.md`/`AGENTS.md` files
  written for other agents are found, zen offers to import their content into the new
  `ZEN.md` (migration path for Claude Code/Codex switchers).
## Prompt caching (paid models)

In a long agent loop the cost driver is rarely the model's answers — it is the
ever-growing input prefix (system prompt + tool schemas) replayed on every turn.
Zen ships prompt caching for paid models, on by default, with zero configuration:

- **Anthropic** (`--base-url https://api.anthropic.com`): explicit
  `cache_control: {"type": "ephemeral"}` breakpoints after the tool definitions
  and after the stable system block. Cache reads are billed at **10% of the
  input price** (90% off); the 5-minute TTL refreshes on every read, so an active
  session stays warm.
- **OpenAI**: automatic — any stable prompt prefix >= 1,024 tokens is served from
  cache at 90% off, no code changes. Zen keeps the prefix stable so hits happen.
- **Gemini / DeepSeek**: implicit/best-effort caching benefits from the same
  stable-prefix discipline, free.

Measured savings on real agent trajectories: **-49% to -80% token cost**
(LangChain Deep Agents eval), **25-35% off the total bill** in production.

How zen keeps the cache hot:

1. **Cache-aware prompt ordering** — the system prompt is assembled
   stable-first: role, project snapshot, memory, skills. The volatile
   environment block (date, git branch/status) goes *last*, after an internal
   marker, so it never poisons the cached prefix.
2. **Stable tool bundles** — tool definitions are sent in the same order every
   turn. Recomposing the tool list per request would silently invalidate the
   cache (Anthropic caches tools before everything else).
3. **No volatile values in the prefix** — a CI test fails the build if today's
   date or a request id ever leaks into the cacheable prefix.

Verify it is working: `/cost` (or `zen cost`) shows the session's cache hit
rate, e.g. `Prompt cache: 800 tokens served from cache (80% of input)`. If the
hit rate is unexpectedly 0 on a paid model, something broke the prefix
stability — that is the first thing to check.
### Tool-result pruning

In a long agent loop the cost driver is rarely the model's answers — it is the
ever-growing input prefix replayed on every step. A 5,000-token tool result added
on step 2 of a 15-step run is re-billed ~13 more times. Zen prunes it:

- **Pruning** (on by default): tool results older than `--prune-after` turns
  (default 5; never the last 2 turns) that are large (>=2k tokens) and whose
  identifier (file path, command, ...) is not referenced by any later message are
  replaced with an extractive summary: first line + token count + a
  `[zen: pruned ...]` marker. Summaries are extractive, never rewritten — exact
  identifiers survive byte-identical. Results whose identifier appears in the
  current plan or recent tool calls are never pruned.
- **Capping**: new tool results over `--max-result-tokens` tokens (default 8000)
  are truncated with a `[zen: truncated ...]` marker and a re-read hint instead of
  being sent whole. The session journal keeps the raw output.

```bash
./target/release/zen -p "refactor the auth module" --prune-after 3
./target/release/zen -p "refactor the auth module" --no-prune   # disable
```

Pruning emits a `system`/`prune` stream-json event (`pruned`, `tokens_saved`) and
`zen context` tells pruned summaries apart from live results by their marker.
### Tool bundles (token-saving)

Every tool's JSON schema is re-sent on every model request — a hidden per-turn
tax (~1.7k tokens for the full builtin set). A *tool bundle* sends only one
task phase's tools with full schemas; tools outside the phase are name-only
stubs (the model knows they exist, at ~50 tokens instead of ~150-300).

| Bundle | Full-schema tools |
|---|---|
| `all` (default) | everything (today's behavior) |
| `core` | TodoWrite |
| `read` | TodoWrite, Read, Glob, Grep, WebFetch |
| `write` | TodoWrite, Write, Edit, MultiEdit |
| `exec` | TodoWrite, Bash, JobOutput, JobKill |

`Task` and `mcp__*` tools are never classified: they keep full schemas in every
bundle. If the model calls a stubbed tool, the call is refused with guidance
instead of executing with hallucinated arguments.

The bundle is fixed for the whole session — it is never recomposed per
request, because changing the tool list between requests silently invalidates
provider prompt caches (which would cost more than the schemas save).

```bash
zen --tool-bundle read -p "where is the retry logic?"
ZEN_TOOL_BUNDLE=write zen -p "fix the bug"
```

Settings file: `{ "toolBundle": "exec" }`. Precedence: `--tool-bundle` >
`ZEN_TOOL_BUNDLE` > settings > `all`.

Audit the cost anytime: `zen context` (or `/context`) shows per-tool schema
costs, most expensive first. Full per-tool reference: `zen tool-help Read`
(parameters, behavior, examples); the source lives in `docs/tools/*.md`.

CI enforces per-bundle token budgets (`SCHEMA_TOKEN_BUDGETS` in
`crates/zen-agent/src/tool_bundles.rs`): a description edit that inflates a
bundle over budget fails the build. Bump a budget only with a measured
justification.
## Session export / import

No major coding agent ships session portability — transcripts are locked into
one tool's format. ZenCode exports any session to a documented, versioned
format (`format_version`) that other tools can read, and imports it back:

```bash
zen export --format json --session <id> -o session.json   # portable document
zen export --format markdown -o session.md               # human-readable
zen export --format html -o session.html                 # styled report
zen export --redact -o session.json                     # strip secrets first
zen import session.json                                 # restore as a NEW session
```

`--session` defaults to the most recent session for the current directory.
Only JSON exports can be imported; import always creates a new session id.
See [docs/session-format.md](docs/session-format.md) for the format spec.

### Code review

```bash
./target/release/zen review                  # review uncommitted changes (staged + unstaged + untracked)
./target/release/zen review --uncommitted     # same, explicit
./target/release/zen review --base main       # review the diff against a base ref
./target/release/zen review --commit abc123   # review a specific commit
./target/release/zen review --fail-on medium --output-format json
```

`zen review` runs the model with a **mechanically enforced read-only tool set**
(only `Read`, `Glob`, `Grep` are registered — write tools do not exist in the
session, and a permission checker denies anything not flagged read-only). Findings
are grouped by severity (`critical` / `high` / `medium` / `low` / `nit`) with
`file:line`, category (`bugs`, `security`, `perf`, `style`, `tests-missing`) and a
suggested fix.

Exit codes are CI-friendly: `0` = no findings at or above `--fail-on` (default
`high`), `1` = findings at or above the threshold, `2` = tool error (git failure,
missing API key, unparseable model output). Example pre-commit hook:

```bash
zen review --fail-on high || echo "review found blocking issues"
```
### `/deep-research` (REPL)

Fan out parallel research sub-agents over a topic, then synthesize a cited markdown report
(Summary / Findings / Sources / Confidence / Open questions, plus a raw-findings appendix).

```
/deep-research "prompt caching" --max-subagents 4 --timeout-secs 600 --max-budget-usd 2
```

- Four angles: official docs, codebase exploration, web research, alternatives & comparison.
- Researchers run through the `Task` tool with isolated context; progress prints as each finishes.
- Budget: `--max-subagents` (default 4, hard cap 8), per-researcher timeout, and a
  `--max-budget-usd` pre-flight check (or `ZEN_MAX_BUDGET_USD`). The dollar estimate is
  conservative until the cost ledger lands; the subagent count and timeout are the hard guard.
### Definition of done

The agent should not declare work complete without evidence, and should not silently
grow scope. The done-guard gives you three tools:

- `/done` — runs the definition-of-done checklist in the working directory: the
  project's tests and linter (auto-detected: `cargo test`/`clippy`, `npm test`/`eslint`,
  `pytest`, `go test`), the session todo list (every item must be `completed`), plus a
  report of changed files, scope violations, and speculative new public APIs with no
  callers (YAGNI). Exits non-zero in `-p` mode when a check fails, so it works in CI:
  `zen -p "/done"`.
- `/scope <glob>...` — declares the task scope (e.g. `/scope src/**`), seeded
  automatically from file mentions in your first prompt. The turn guard nudges the model
  to confirm with you before mutating files outside it, and the REPL warns you after
  each turn that changed out-of-scope files. `/scope` with no args shows the current
  scope; `/scope clear` removes it.
- The system prompt carries a short "Definition of done" section, and the finish gate
  reminds the model to run the checks after file changes (`verify.command` in settings
  makes the harness run them automatically).

### Curated plugin registry

The skill registry has two tiers: `curated` (maintainer-reviewed, badge `[curated ✓]`)
and `community` (unreviewed). Every curated entry carries public review metadata —
reviewer, date, and notes covering security, quality, documentation, and maintenance —
and the badge is version-scoped. See `docs/plugin-curation.md` for the full process.

```sh
zen skill search tdd              # community registry
zen skill search tdd --curated    # curated (maintainer-reviewed) only
```

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
## REPL polish commands

Small quality-of-life slash commands for the interactive REPL:

| Command | What it does |
|---|---|
| `/loop <n> <prompt>` | Run a prompt `n` times in a row (batch operations). Shows `[loop i/n]` per iteration, stops on the first failed turn, capped at 100 iterations. |
| `/goal <text>` | Set a session goal, shown on a dimmed line above the prompt and in `/status`. `/goal` shows it, `/goal clear` unsets it. Reset by `/clear`. |
| `/personality <name>` | Switch response style: `concise`, `normal` (default), `detailed`. Adjusts the system prompt; `normal` leaves it byte-identical. |
| `/vim` | Toggle the vim-keybinding preference. The REPL currently reads plain stdin lines, so vi editing motions are not available yet — the command records the preference and says so honestly. |
| `/theme <name>` | Switch color theme: `dark` (default), `light`, `none`. `NO_COLOR` (any value) always disables colors regardless of theme. |

```bash
/loop 3 run cargo test and fix any failures
/goal ship the v0.6 milestone
/personality concise
/theme light
## Sessions

Conversations persist as JSONL transcripts under `~/.zen/projects/<project>/`.
Manage them with `zen sessions`:

```bash
zen sessions list               # active sessions: id, date, messages, tokens, title
zen sessions list --all         # include archived sessions
zen sessions archive <id>       # hide from the default list (restorable)
zen sessions restore <id>       # bring an archived session back
zen sessions delete <id>        # permanently delete (asks for confirmation; --yes to skip)
```

Archived sessions live in `<project>/archive/` — they stay out of `zen sessions list`
and `--continue`, but `--resume <id>` and `restore` still work. Deleting removes the
transcript and its scratch files; there is no undo.

Auto-archive stale sessions with `sessions.autoArchiveDays` in `~/.zen/settings.json`
(`0` = off, the default):

```json
{ "sessions": { "autoArchiveDays": 90 } }
```
## LSP code intelligence

`zen` reads code like a compiler: three read-only tools — `goto_definition`, `hover` and
`find_references` — backed by real language servers spoken to over LSP (JSON-RPC 2.0 on
stdio with `Content-Length` framing). Servers start lazily per language the first time a
tool needs them and stay alive for the whole session, so repeated queries are fast.

Supported servers:

| Language | Extensions | Server command | Install |
|---|---|---|---|
| Rust | `.rs` | `rust-analyzer` | `rustup component add rust-analyzer` |
| Python | `.py` | `pyright-langserver --stdio` | `npm install -g pyright` |
| TypeScript / JavaScript | `.ts` `.tsx` `.js` `.jsx` `.mjs` `.cjs` | `typescript-language-server --stdio` | `npm install -g typescript-language-server` |

How it works:

- The project root is found by walking up from the file to the nearest `Cargo.toml`
  (Rust), `pyproject.toml`/`setup.py`/`setup.cfg` (Python) or `tsconfig.json`/`package.json`
  (TypeScript), and passed as the LSP `rootUri`.
- The full handshake runs (`initialize` with `processId`/`rootUri` and
  definition/hover/references capabilities → `initialized`); files are opened with
  `textDocument/didOpen` and re-sent with `didChange` when they change on disk.
- Positions are 1-based line/character columns; multibyte characters count once (LSP's
  UTF-16 offsets are converted both ways). Requests time out instead of hanging
  (30s for `initialize`, 15s per request).

Tool reference:

- `goto_definition(file, line, col)` — definition location(s) of the symbol. Parses all
  three LSP shapes: a single `Location`, `Location[]`, and `LocationLink[]`.
- `hover(file, line, col)` — type signature and documentation, rendered as markdown.
- `find_references(file, line, col, include_declaration=true)` — every use of the symbol.

Each result prints human-readable `path:line:col` lines followed by a fenced `json` block, so
both the model and scripts can consume it.

Graceful degradation: if a server binary is missing from `PATH`, the tool fails with a
clear error naming the server and the exact install command — the agent never hangs.
The tools honor `--tools` (allowlist and `+`/`-` delta forms) and `--disallowedTools`
like built-in tools, and they are skipped by `--tools ""`.
## Skills

ZenCode skills are installable, permission-declared, trust-tiered bundles of
instructions (methodology, checklists, habits). Format spec: [`docs/skills.md`](docs/skills.md).

```bash
zen skill list                        # installed skills with trust badges
zen skill install builtin:tdd         # tdd | token-diet | security-review
zen skill install https://github.com/you/skill.git --yes
zen skill test tdd                    # run the skill's evals (free, deterministic)
zen skill create my-skill             # scaffold a new skill
zen skill search tdd                  # search the community skill registry
zen skill info tdd                    # tier + permissions before installing
zen skill install registry:tdd --yes  # install from the registry
```

Discoverability: the community skill registry ([`docs/skill-registry.md`](docs/skill-registry.md))
ships curated with the binary (`zen skill search` works offline). The registry
is advisory — install re-verifies trust from the skill's own signature, so a
registry can never upgrade a skill's tier.

Trust tiers: `[✓ verified]` (ed25519-signed by ZenCode — nobody else in the
industry ships a real verified tier), `[community]` (you approved the install,
hash-pinned, never auto-updates), `[⚠ unverified]` (sandboxed: read-only, no
network). Install always shows the permission diff first and requires `--yes`;
there are no silent auto-updates.
