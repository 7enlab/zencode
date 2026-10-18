# ZenCode Positioning: Free Is Structural, Not Promotional

**Status:** Draft messaging foundation (2026-10-17) — pending review
**Audience:** Everyone who writes about ZenCode — README, docs, launch posts, issue replies.

---

## The one sentence

> **ZenCode is the AI coding agent that costs $0 — by design, not by promotion.**

---

## 1. The rug-pull economy (why this message exists)

In 2026, every major "free" AI coding tier got narrowed, locked, or killed. This is not
anecdote — it is the pattern that now defines the market:

| Event | Date | What happened |
|---|---|---|
| **Qwen Code** free OAuth tier | **April 15, 2026** | The famous ~2,000 requests/day free tier ended (quotas stepped 2,000 → 1,000 → 100 first). Today: bring your own key, metered at $0.11/M input. |
| **Gemini CLI** personal backend | **June 18, 2026** | Google shut off the free/Pro/Ultra backend with **zero grace period**. 100k+ stars, 6,000 community PRs — dead for personal users overnight. CI pipelines and cron jobs broke. The community called it a *"bait and switch."* |
| **OpenCode** Zen free tier | **September 2026** | Free models stopped answering external callers (`FreeTierError: "OpenCode's free tier can only be used from within OpenCode"`). The catalog rotates without notice (13 → 11 models in one week), and most free models train on your prompts. |
| **Trae** free tier | **2026** | Unmetered free access became a **one-time allowance that never resets** — a trial wearing a free tier's clothes. |
| **iFlow CLI** | **Mid-2026** | Shut down entirely. Post-mortem lesson: *"free multi-model aggregation had no visible business model."* |

The pattern: **every "free" tier in this market is a marketing funnel, and funnels get cut.**
Developers learned this lesson twice in four months. They are now — rationally — distrustful
of any free tier that depends on a vendor's continued generosity.

ZenCode's answer is not "our free tier is better." It is: **there is no tier to pull.**

---

## 2. Why ZenCode's free is structural

"Structural" means the $0 does not depend on anyone's business decision. Four properties
make it so:

### 2.1 No central API exists to shut off

ZenCode talks to **any OpenAI-compatible `/chat/completions` endpoint**. There is no
ZenCode cloud, no ZenCode gateway, no ZenCode API key. When a provider changes its
terms, you point ZenCode at a different endpoint — and when providers *in general*
can't be trusted, you point it at your own machine (see 2.3), whose terms nobody
can change. *A repo without a backend willing to serve it is source code, not a
tool* — so ZenCode was designed to never have a backend in the first place.

### 2.2 Bring your own model — including free ones

ZenCode routes to 11+ providers out of the box: free cloud tiers (Google AI Studio,
Groq, OpenRouter `:free`, Mistral, NVIDIA NIM, Cerebras), paid APIs when you choose
them, and local models. Fallback chains mean one provider's quota wall is a speed bump,
not a wall: OpenRouter `:free` hits its limit → ZenCode fails over to Groq → Mistral →
local. `zen models` probes and picks the best free model *today*, because free catalogs
rotate and static guides go stale on publish day.

### 2.3 Ollama: the escape hatch that is also the front door

Local models via Ollama are **first-class, not a fallback**. Qwen3-Coder, Kimi K2,
GLM-5.2 — all permissively licensed, all Ollama-runnable, all $0 marginal cost, no
rate limits, no data-retention question. `zen --model ollama/qwen3-coder:30b` keeps
working with the network cable unplugged. **No vendor on the planet can rug-pull a
model running on your own hardware.**

### 2.4 The $0.00 receipt

`zen cost` shows the session ledger with a big **$0.00** on free models. This is not a
marketing claim — it is an auditable number computed from the same pricing table the
paid paths use. When we say "$0," we show the receipt.

---

## 3. The Four Questions

After the Gemini CLI shutdown, developer Jenarius Ganlary published a framework now
widely cited for evaluating AI coding tools. Four questions, answerable *before*
adopting:

1. **Is the backend as open as the code?**
   *"If the vendor disappeared tomorrow, could a competent stranger run this end-to-end
   from the public repo?"*
2. **Am I the customer, or am I the churn?**
   *Individual tiers are marketing funnels; funnels get cut.*
3. **What breaks the day it dies?**
   *Inventory the blast radius before adopting.*
4. **What's my exit cost, in hours?**

ZenCode's answers: **yes / customer / nothing / near-zero** — because with local models
there is no backend at all, and with cloud models the backend is yours, not ours.

| Question | ZenCode | Typical vendor-hosted agent |
|---|---|---|
| Is the backend as open as the code? | **Yes.** No backend; any endpoint or local model. | No — the API is the product. |
| Am I the customer or the churn? | **Customer.** Your keys, your models, your machine. | Churn — free tiers are acquisition funnels. |
| What breaks the day it dies? | **Nothing.** The repo + your models keep working. | Everything — CI, cron, scripts, muscle memory. |
| Exit cost? | **Near zero.** Open formats, portable sessions (`zen export`). | Hours to weeks of migration. |

This is ZenCode's single strongest positioning vehicle: it is *structural*, not a
feature, and it cannot be copied without rebuilding the competitor's business model.

---

## 4. "What happens if the company dies?" — the comparison

| | Claude Code | OpenCode | Qwen Code | **ZenCode** |
|---|---|---|---|---|
| If the vendor dies | **Tool dies.** Proprietary, API-only. | **Free tier dies.** Still needs vendor APIs. | **Free tier already died** (Apr 2026). BYOK survives. | **Nothing breaks.** No vendor backend exists. |
| Who pays for "free"? | Nobody — no free tier. | The vendor (funnel). | The vendor (funnel, then cut). | **Nobody needs to.** Your keys, your models, or your hardware. |
| Can it work fully offline? | No. | No. | Yes, with local models. | **Yes — Ollama is first-class.** |
| Who sees your prompts? | The vendor. | The vendor (most free models train on them). | The vendor. | **Whoever you point it at.** Local = nobody. |

---

## 5. The supporting differentiators

The structural-$0 story opens the door; these are why developers stay:

- **D2 — The harness that makes free models work, with receipts.** Verify loop, eval
  harness, prompt budget, compaction, repo-map budget — each exists because free models
  need them. An October 2026 experiment: a free local model matched paid models 15/15
  on coding tasks *with* test-and-retry feedback vs 4/15 without. The harness is the
  product; competitors optimize harnesses for frontier models, ZenCode optimizes for
  weak ones. Different objective function, different product.
- **D3 — Auditability as a feature.** Stream-json contracts, prompt-budget regression
  tests, eval harness, cost ledger, `zen audit`, `zen export`. Nobody else treats the
  harness as observable infrastructure.

---

## 6. Positioning one-liners (approved)

Use these verbatim where they fit; do not invent new ones without review
(see `docs/messaging.md`).

1. *"The AI coding agent that costs $0 — by design, not by promotion."*
2. *"When your Claude Code quota runs out at 2 PM, ZenCode keeps going."*
3. *"Open source where it matters: no backend to shut off. Ask anyone who used Gemini CLI."*
4. *"The harness that makes free models actually work."*
5. *"Four questions. One answer: yes."*

---

## 7. What this positioning is NOT

- **Not "a free Claude Code."** Never market ZenCode as a clone of anything
  (see `docs/messaging.md` — legal risk and weak positioning). The product is
  *the free-first agent*, not *the free Claude Code*.
- **Not "free models are as good as paid."** Free-model quality variance is real.
  We publish honest benchmarks with methodology and let the numbers speak.
  The community fact-checks within hours — never oversell.
- **Not anti-paid.** ZenCode fully supports paid models and paid API keys
  (`zen auth`). "Free-first" means free works great, not that paid is unwelcome.
  Many users will mix: free for exploration, paid for the hard parts.

---

*Sources: `zencode-chinese-competitors.md` (2026-10-17), `zencode-roadmap-v3-global.md` §1.2–§1.3
(2026-10-17). Prices and events are perishable — verify before citing in dated materials.*
