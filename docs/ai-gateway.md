# Cloudflare AI Gateway with ZenCode (optional)

> **Optional integration.** ZenCode works fully without Cloudflare. AI Gateway
> is a convenience layer: free fallback chains, edge caching, and analytics
> that complement ZenCode's built-in features. If you prefer self-hosted or
> air-gapped setups, skip this document - nothing in ZenCode depends on it.

## What AI Gateway gives you

[Cloudflare AI Gateway](https://developers.cloudflare.com/ai-gateway/) is a
reverse proxy that sits in front of your LLM providers. You change the base
URL; everything else keeps working.

| Gateway feature | ZenCode built-in equivalent | How they combine |
|---|---|---|
| Universal fallback (dashboard-defined chain) | `--fallback` chains (client-side) | Use the Gateway when you want zero-config server-side fallback; use ZenCode's built-in chains when you want explicit per-request control. They can be layered: ZenCode retries through the Gateway, the Gateway retries across providers. |
| Edge caching of identical completions | Prompt caching (Anthropic) | Gateway cache hits return in <15 ms with **zero downstream cost** - best for repeated identical prompts (CI, evals). |
| Rate limiting & per-key budgets | `--max-budget-usd` | Gateway enforces limits at the edge before requests reach the provider - an extra guardrail under your ZenCode budget. |
| Request/token/cost analytics | `zen cost` ledger | Gateway analytics are provider-side aggregates; `zen cost` is your per-session ledger. Use both. |

Core Gateway features (dashboard, caching, rate limiting) are **free on all
Cloudflare plans**.

## Setup

1. Create a free Cloudflare account and an AI Gateway at
   <https://dash.cloudflare.com> → AI → AI Gateway. Name it e.g. `zencode`.
2. In the Gateway, add your providers (OpenAI, Anthropic, …) with their API keys.
   Keys live in Cloudflare - never in your shell history or agent sandbox.
3. Point ZenCode at the Gateway instead of the provider directly. The Gateway
   URL format is:

   ```
   https://gateway.ai.cloudflare.com/v1/{ACCOUNT_ID}/{GATEWAY_NAME}/{PROVIDER}
   ```

   For example, to route OpenAI traffic through the Gateway:

   ```bash
   export ZENCODE_BASE_URL="https://gateway.ai.cloudflare.com/v1/abc123/zencode/openai"
   # or per-provider env override, e.g. OPENAI_BASE_URL
   ```

4. (Recommended) Enable **caching** in the Gateway dashboard for deterministic
   workloads, and **rate limiting** as a backstop under `--max-budget-usd`.

## Fallback chains via the Gateway

Instead of configuring client-side fallback chains, define an ordered provider
list in the Gateway dashboard (**AI Gateway → your gateway → Fallbacks**).
When the primary provider returns an error or rate limit, the Gateway
automatically retries the next provider in the list. ZenCode sees a single
endpoint and a single bill of tokens in `zen cost`.

Trade-off to know: server-side fallback is simpler to operate, but the retry
decisions and logs live in Cloudflare's dashboard, not in ZenCode's
`--stream-json` event stream. If you need per-attempt observability, prefer
ZenCode's built-in fallback chains.

## Workers AI as a free-tier provider

Cloudflare **Workers AI** offers **10,000 Neurons/day free** (resets 00:00 UTC)
- real free inference for small tasks (classification, routing, summarisation).
It is registered in ZenCode's provider registry as `cloudflare`:

```bash
export CLOUDFLARE_API_TOKEN="..."        # API token with Workers AI permission
export CLOUDFLARE_ACCOUNT_ID="..."       # your Cloudflare account ID
zen --model cloudflare/@cf/meta/llama-3.1-8b-instruct -p "summarise this diff"
```

Notable free/cheap models on Workers AI (pricing per 1M tokens, 08/2026):

| Model | Input | Output | Notes |
|---|---|---|---|
| `@cf/cloudflare/clef-flash` | $0.09 | **free** | Cloudflare's own 9B decision model (Apache 2.0) |
| `@cf/meta/llama-3.1-8b-instruct` | $0.045 | - | Cheapest general chat |
| `@cf/zai-org/glm-5.3` | $1.40 | - | Frontier-class open weight |

Under the free tier, usage within 10K Neurons/day costs **$0** regardless of the
table above. Workers AI fits "right model for the right job": small tasks go to
the free tier, coding stays on frontier models.

> Workers AI's OpenAI-compatible endpoint embeds your account ID in the URL
> path (`.../accounts/{ACCOUNT_ID}/ai/v1`). ZenCode interpolates
> `{ACCOUNT_ID}` from `CLOUDFLARE_ACCOUNT_ID` automatically - you never have to
> construct the URL by hand. If the variable is missing, ZenCode reports a clear
> error instead of sending a malformed request.

## Privacy note

AI Gateway and Workers AI route your prompts through Cloudflare's network.
Do not use them for data that must stay inside your VPC - point ZenCode at a
self-hosted endpoint (e.g. Ollama) instead. See also `docs/account-safety.md`
for API-key hygiene.

## Troubleshooting

- **401 from the Gateway**: the provider key stored *in the Gateway dashboard*
  is wrong or expired - fix it there, not in ZenCode.
- **`CLOUDFLARE_ACCOUNT_ID is not set`**: export it; find it in the Cloudflare
  dashboard URL or Workers → Overview.
- **Cache not hitting**: the Gateway caches on exact request match - streaming
  requests and temperature > 0 reduce hit rates.
