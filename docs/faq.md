# ZenCode FAQ

**Last updated:** 2026-10-17
**Scope:** The questions every developer asks before trusting a "free" AI coding agent.
For how we talk about ZenCode, see `docs/messaging.md`. For the full argument,
see `docs/positioning.md`.

---

## Is the free tier really free?

Yes - and more importantly, **there is no "tier" to take away.**

Most "free tiers" are a vendor's marketing budget: they pay for your tokens hoping
you'll convert to paid. When the math stops working, the tier gets narrowed
(Gemini CLI, June 2026), locked to the vendor's app (OpenCode, September 2026), or
killed outright (Qwen Code's 2,000 req/day tier, April 2026).

ZenCode has no central API and no token subsidy. "Free" means:

- **Free cloud models** - ZenCode routes to providers with genuine free tiers
  (Google AI Studio, Groq, OpenRouter `:free`, Mistral, NVIDIA NIM, Cerebras).
  You use *their* free tier with *your* key, through an open protocol.
- **Local models** - Ollama on your own machine. $0 marginal cost, no rate limits,
  no account, no data leaving your hardware. No company on Earth can rug-pull a
  model running on your laptop.
- **The receipt** - `zen cost` shows a per-session ledger. On free models it reads
  **$0.00**, computed from the same pricing table as the paid paths. Don't take our
  word for it; read the ledger.

Nobody pays for your tokens because nobody *needs* to - the tokens were already
free at the source.

## What if ZenCode shuts down?

**Nothing breaks.** This is the question the whole project is designed around
(see "The Four Questions" in `docs/positioning.md`).

- There is no ZenCode backend to turn off. The binary talks to *your* configured
  endpoint - a cloud provider, your company's gateway, or `localhost:11434`.
- The code is open source. Your config, sessions, skills, and memory (`ZEN.md`)
  live in open formats on your disk.
- Sessions are portable: `zen export` / `zen import` move a session to a new
  machine or a new tool.

Compare: when Gemini CLI's backend died on June 18, 2026, CI pipelines and cron
jobs calling `gemini` broke overnight - the open-source repo survived, but *"a repo
without a backend willing to serve it is source code, not a tool."* ZenCode never
has a backend, so there is nothing to die.

## How do you make money?

Honestly: **right now, we don't.** ZenCode is an open-source project, not a
funnel. There is no paid tier to upsell you to, no "Pro" plan behind the free
one, no investor demanding conversion metrics.

That is precisely why the free can't be rug-pulled - there is no business model
depending on converting you. (iFlow CLI shut down mid-2026 with the post-mortem
*"free multi-model aggregation had no visible business model"* - we agree, and
that's why we don't aggregate: you bring the model, we bring the harness.)

If monetization ever comes, the commitment is: it will be for something *additive*
(hosting, team features, support) - never for gating what is free today. The
architecture makes that promise enforceable: we couldn't take away your local
models if we wanted to.

## How is ZenCode different from OpenCode / Pi / Claude Code?

| | Claude Code | OpenCode | Pi | **ZenCode** |
|---|---|---|---|---|
| Cost | $20–200/mo, no free tier | Free agent; models BYOK or vendor PAYG | Free, BYOK | **$0 structural** (free models + local) |
| Free tier rug-pull risk | n/a (paid) | **Real** - Zen free tier locked to app Sep 2026 | Low (no central free tier to pull) | **None** - no central free tier exists |
| Works fully offline | No | No | With local models | **Yes - Ollama first-class** |
| Per-run cost visibility | No (#1 complaint) | Basic | `$0.0xx` footer | **`zen cost` ledger + prompt budget** |
| Harness for weak models | No (frontier-first) | Manual tuning | Minimal by design | **Verify loop + eval + compaction** |
| Auditability | Closed source | Partial | Partial | **Stream-json, eval, audit, export** |

The short version: Claude Code is the best harness money can buy; **ZenCode is the
best harness $0 can buy** - engineered specifically to make free and weak models
succeed, with the receipts to prove it.

## What models can I use?

Anything speaking OpenAI-compatible `/chat/completions` or the Anthropic Messages
API - 11+ providers configured out of the box, plus any custom endpoint:

- **Free cloud:** Google AI Studio, Groq, OpenRouter `:free`, Mistral, NVIDIA NIM,
  Cerebras (quotas and catalogs change - `zen models --refresh` probes what's
  actually working *right now*).
- **Local:** Ollama - Qwen3-Coder, Kimi K2, GLM-5.2, Devstral, gpt-oss. Fully
  offline, fully private.
- **Paid (your choice):** OpenAI, Anthropic, Google, DeepSeek, Z.ai, Moonshot -
  `zen auth login` stores keys in your OS keyring. Paid is supported, never required.

`zen models` shows what's available; fallback chains fail over automatically when
a free quota is exhausted.

## Do my prompts train someone else's models?

**Only if you point ZenCode at a provider whose terms say so** - and we'll tell
you which ones do.

- **Local models (Ollama):** your prompts never leave your machine. Nobody trains
  on them. This is the only setup with a mathematical guarantee.
- **Free cloud tiers:** many do train on prompts (OpenCode's own docs admit most
  of their free catalog does; only a few models are zero-retention). Assume yes
  unless the provider says otherwise.
- **Paid APIs:** generally zero-retention by contract.

ZenCode itself never sees your prompts - there is no telemetry phone-home. What
leaves your machine goes exactly one place: the endpoint *you* configured.

## Can I use ZenCode offline?

Yes - with a local model via Ollama. `zen --model ollama/qwen3-coder:30b` works
with the network cable unplugged: no auth, no quota, no telemetry, no kill
switch. This isn't a degraded mode; it's a supported configuration we demo
deliberately.

## Is this a Claude Code clone?

**No - and we never market it as one.** ZenCode is a clean-room implementation:
it mirrors *observable behavior* (REPL, agent loop, tools, hooks, skills) without
using anyone's code, prompts, or tool descriptions. But "clone" is the wrong frame
entirely: the product is *the free-first agent*, not *the free Claude Code*.
Positioning as a clone would be both a legal risk and weak positioning - it
concedes that the original defines the category. It doesn't; the category is
"agents that cost $0 by design," and we're defining it.

## What does a run actually cost?

Whatever your model costs - and ZenCode shows you, per session:

- Free model: **$0.00** (ledger-verified).
- Paid model: exact input/output token counts × the provider's price, plus cache
  hit rate when prompt caching applies.
- `--max-budget-usd` caps spend per session; the pre-flight check blocks the call
  *before* any request is sent, so a cap is never exceeded by surprise.

The #1 complaint about coding agents in 2026 is unpredictable per-run cost. Our
answer is radical transparency: every token attributed, every cent (or lack of
cent) on the record.

## Who is ZenCode for?

- Developers who want a capable coding agent at **$0/month** - students, hobbyists,
  anyone between jobs, anyone in a region where $20/mo is real money.
- Developers burned by a rug-pull (Gemini CLI, OpenCode Zen, Qwen Code) who now
  evaluate tools with the Four Questions.
- Privacy-conscious developers: local models, sandboxing, folder trust,
  `zen audit` before the agent touches anything.
- Tinkerers who want an **observable** harness: stream-json contracts, eval
  harness, cost ledger - the agent as infrastructure you can inspect.

## Who is ZenCode NOT for?

Honesty matters more than conversion:

- If you need the absolute frontier model on every task and never think about
  cost, Claude Code with Max is a fine product. ZenCode supports paid models
  too, but our harness is *optimized* for making weaker models succeed.
- If you need an IDE-native experience today, our VS Code extension is on the
  roadmap - the CLI is the product right now.
- If you need enterprise SSO, audit trails for compliance, or a vendor SLA -
  we're not there yet. The architecture (local-first, no backend) is arguably
  *better* for compliance, but we won't claim certifications we don't have.

---

*Something missing or wrong? Open an issue - this FAQ is a living document and
corrections are welcome. Facts about third parties are perishable; verify before
quoting.*
