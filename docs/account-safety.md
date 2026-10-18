# Account Safety

ZenCode is designed to be a **good API citizen** so your paid accounts
(OpenAI, Anthropic, Google) stay in good standing.

> **Honest disclaimer:** No tool can guarantee your account won't be flagged —
> providers use automated systems with false positives. ZenCode minimizes the
> risk factors within its control and warns you about the rest.

## What ZenCode does automatically

- **Exponential backoff with jitter** on rate limits (base 1s, doubling, ±25%
  jitter, capped at 60s), max 5 retries per request. It never hammers through 429s.
- **Always respects `Retry-After`** headers when the server sends one.
- **Never retries billing/quota errors.** `insufficient_quota` (and similar) is a
  hard stop with a clear message — retrying a billing error only creates
  failed-request noise on your account.
- **Classifies errors before retrying:** 429 rate-limit → backoff + retry;
  529 (Anthropic overloaded) → backoff + suggest a fallback provider;
  other 4xx → fail fast, no retry.
- **Identifies itself** with a versioned `User-Agent: ZenCode/<version>
  (+https://zencodego.com)` on every API request, so providers can reach out
  about issues instead of banning first.
- **Request pacing:** a per-provider token bucket enforces a 100 ms minimum
  interval between requests and slows down proactively at ~80% of the detected
  rate-limit ceiling (parsed live from response headers).
- **Concurrency cap:** at most 2 concurrent requests per provider by default.
- **Anthropic prompt caching** is used to reduce rate-limit pressure (cached
  input tokens don't count toward per-minute token limits on current models).

## What you should do

- **One API key per machine.** Never share keys across computers or people —
  the same key from multiple IPs/geographies looks like account sharing/resale
  to automated abuse detection.
- **Don't run many parallel sessions on a single key.** Agentic loops fire many
  requests per minute; ZenCode warns you when more than 2 sessions share one key.
- **Keep payment methods current.** Chargebacks and payment disputes trigger
  automatic suspensions at several providers.
- **If a key may be compromised** (committed to a repo, pasted somewhere),
  revoke it at the provider immediately and run `zen auth logout`. Billing can
  continue after a ban if revocation lags.
- **Slow down on repeated rate limits** instead of pushing through — pushing
  through is the fastest way to get flagged.

## What gets accounts banned (so you can avoid it)

Based on documented cases and community reports (October 2026):

| Trigger | Notes |
|---|---|
| Payment disputes / chargebacks | Automatic suspension at several providers, even for unrelated disputes |
| Shared or resold keys | Same key from many IPs looks like account farming |
| Bot-like traffic patterns | Metronome-regular requests 24/7, no backoff on 429s |
| ToS violations | Model distillation (training on outputs), OAuth subscription-token relay through third-party tools, policy-violating content |
| Compromised keys | Leaked keys get deactivated; revoke immediately |

**Not a ban trigger:** normal coding-agent usage within published rate limits,
with backoff. No evidence was found of providers banning accounts *solely* for
high-volume legitimate usage.

## Warnings you'll see

| Situation | What happens |
|---|---|
| `zen auth login` (paid provider) | One-time account-safety summary |
| >2 sessions on one key | Warning at session start |
| Repeated 429s (>3 in 5 min) | Warning with suggestions |
| `insufficient_quota` | Hard stop, no retry, recovery steps |
| `zen doctor` | Permanent safety checklist |

### `--i-understand-the-risks`

Some safeguards (concurrency caps, parallel-session warnings) can be overridden
with `--i-understand-the-risks`. This is deliberately verbose — it forces a
conscious decision. The acknowledgement is logged with a timestamp.

It **never** disables backoff, never enables retry of quota errors, and never
bypasses the User-Agent. It only acknowledges *volume* risk you choose to take.

## Provider-specific notes

- **OpenAI:** 429s come in two flavors — `rate_limit_exceeded` (slow down, retry)
  vs `insufficient_quota` (out of money — ZenCode stops immediately). Keep
  `max_tokens` tight: the token limit counts what you *request*, not just what
  is used.
- **Anthropic:** 429 means *you* hit a limit (respect `retry-after`); 529 means
  *Anthropic* is overloaded (back off and consider another provider). Prompt
  caching reduces per-minute token pressure.
- **Google (AI Studio):** Quotas are per-project and checked in AI Studio.
  Free-tier content may be used by Google to improve their products — a privacy
  consideration for code, not a ban trigger.

## FAQ

**Will ZenCode get my account banned?**
No evidence suggests normal usage within rate limits gets accounts banned.
ZenCode stays well inside limits, backs off automatically, and warns you about
the patterns that do cause problems (shared keys, parallel-session storms,
ignoring rate limits).

**What if I get banned anyway?**
Automated enforcement has false positives. Appeal through the provider's
support channel; include that you were using a client with backoff and a
versioned User-Agent. If you used `--i-understand-the-risks`, note that the
override is logged.

**Does the free tier need any of this?**
Free tiers are the most abuse-sensitive (they're where bot traffic concentrates),
so pacing and backoff matter *more* there, not less. Local models (Ollama)
skip pacing entirely — there's no provider to anger.
