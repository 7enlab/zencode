# ZenCode Messaging Guidelines

**Status:** Required reading before writing any public ZenCode content (2026-10-17)
**Applies to:** README, docs, launch posts, social media, issue replies, talks.
**Foundation:** `docs/positioning.md` (the argument), `docs/faq.md` (the answers).

---

## The core message (say this, in your own words)

> ZenCode is the AI coding agent that costs $0 - **by design, not by promotion**.
> No backend to shut off, no free tier to rug-pull. Your keys, your models, your
> machine - with a harness engineered to make free models actually work.

Every public statement should be traceable to one of:

1. **Structural $0** - free is architecture, not marketing.
2. **The Four Questions** - yes / customer / nothing / near-zero.
3. **The harness** - verify loop, eval, prompt budget, compaction: built for weak models.
4. **Auditability** - stream-json, cost ledger, `zen audit`, `zen export`.

---

## DO

- **Lead with "$0."** Every viral dev-tool post in 2026 leads with free. Ours is
  the only $0 claim backed by a ledger (`zen cost`) and an architecture no vendor
  can revoke.
- **Name the rug-pulls.** Gemini CLI (June 18, 2026), OpenCode Zen (September
  2026), Qwen Code's free tier (April 15, 2026). These are facts, cited in
  `docs/positioning.md`. Developers who lived through them are our audience -
  speak directly to that experience.
- **Show receipts.** Screenshots of `$0.00` ledgers, benchmark cards with
  methodology, terminal recordings. The community fact-checks within hours;
  publish methodology *with* the claim, not after being challenged.
- **Be honest about weaknesses.** Free-model quality variance is real. The VS Code
  extension is roadmap, not shipped. Enterprise features aren't here. Honesty
  amplifies (Pi's MCP U-turn proved it); overselling gets debunked in the comments.
- **Say "free-first," not "free-only."** Paid models are fully supported
  (`zen auth`, fallback chains, `--max-budget-usd`). Free-first means free works
  great - not that paid is unwelcome.
- **Use the Four Questions as a structure** for launch posts and talks. It's a
  framework the community already cites; answering yes/customer/nothing/near-zero
  is a mic drop that needs no hype.
- **Credit sources.** The Four Questions are Jenarius Ganlary's framework. The
  15/15 free-model experiment is someone else's data. Cite generously - it
  signals confidence, not weakness.
- **Keep it technical.** Our audience reads HN, r/LocalLLaMA, r/selfhosted.
  Architecture diagrams beat adjectives. "No central API exists to shut off"
  beats "revolutionary."

## DO NOT

- **NEVER market ZenCode as a "Claude Code clone"** (or Codex/Gemini/OpenCode
  clone). Three reasons:
  1. *Legal risk* - "clone" invites scrutiny of exactly what was copied, even
     when the implementation is clean-room.
  2. *Weak positioning* - it concedes the original defines the category. Our
     category is "agents that cost $0 by design." We're defining it, not
     copying into it.
  3. *Wrong audience filter* - it attracts users who want Claude Code for free
     (and will complain it's not identical) instead of users who want a
     free-first agent (who will judge it on its own merits).

  What to say instead: *"a terminal coding agent"* / *"the free-first agent"* /
  *"clean-room implementation."* Behavioral comparison ("mirrors observable
  behavior of…") is fine in technical docs; "clone" is banned in marketing.
- **Don't claim free models equal paid models.** Say: *"a free local model
  matched paid models 15/15 on coding tasks with test-and-retry harnessing
  (4/15 without it)"* - with the citation. The harness is the claim, not the
  model.
- **Don't promise what the roadmap hasn't shipped.** The VS Code extension,
  community leaderboard, and plugin registry are roadmap items. Talk about them
  as roadmap items.
- **Don't trash competitors.** State facts ("OpenCode locked its free tier to
  its app in September 2026") - don't editorialize ("OpenCode betrayed its
  users"). Let the reader draw the conclusion; it's stronger that way. The one
  exception: direct, factual answers when asked "why not just use X?"
- **Don't use startup adjectives.** No "revolutionary," "game-changing,"
  "blazingly fast," "seamless," "delightful." If it's fast, give the number.
  If it's simple, show the three commands.
- **Don't cold-pitch influencers.** The X outreach map in the roadmap exists for
  *understanding*, not targeting. Build something they'd cite on merit
  (benchmarks, essays, honest methodology). Never DM, never tag unprompted.
- **Don't market on price alone to paid-model users.** Chinese flat plans
  (GLM Coding Plan ~$3–18/mo, Kimi For Coding) compete on price too. Our edge
  there is *structure* (no lock-in, any endpoint, local escape hatch) - not
  being $0 cheaper than $3.

## Handling common objections

| They say | We say |
|---|---|
| "Free models are dumb." | "Alone, often. With a harness that gives test-and-retry feedback: 15/15 vs paid models in an October 2026 experiment. The harness is the product - try `zen` on your repo and read the ledger." |
| "This will rug-pull like the rest." | "There's nothing to pull. No ZenCode backend exists - your keys, your models, or Ollama on your hardware. `zen export` your sessions; the format is open." |
| "How do you make money?" | "We don't, currently - see `docs/faq.md`. That's the point: no funnel, no conversion metrics, nothing to optimize against you." |
| "Why not just use OpenCode?" | "OpenCode is a good tool. Two differences: their free tier is locked to their app and rotates (Sep 2026); ours is structural. And our harness is optimized for weak models - verify loop, prompt budget, eval - with published methodology." |
| "Is this legal / a Claude Code copy?" | "Clean-room implementation: observable behavior only, no copied code, prompts, or tool descriptions. We never market it as a clone - it's the free-first agent, its own category." |
| "Sounds too good to be true." | "Don't trust us - verify. The ledger is in `zen cost`, the benchmarks ship with methodology, the code is open. Skepticism is the correct default in 2026." |

## Tone reference

**Good:**
> "Gemini CLI died on June 18 with zero notice. 6,000 community PRs now underpin
> a closed successor. ZenCode's answer isn't a better free tier - it's no tier
> at all. There is no ZenCode backend to shut off. Here's the architecture: …"

**Bad:**
> "🚀 Introducing ZenCode - the REVOLUTIONARY AI coding agent that's 100% FREE
> FOREVER! Better than Claude Code and costs NOTHING! ⭐ Star us now!"

The difference: facts vs adjectives, receipts vs promises, respect vs hype.

## Checklist before publishing anything

- [ ] Does it trace to one of the four message pillars?
- [ ] Is every factual claim cited or verifiable? (Prices/events are perishable -
      re-verify if the material will live >30 days.)
- [ ] Does it contain the word "clone"? → Remove it.
- [ ] Does it claim something the benchmarks don't support? → Soften or cut.
- [ ] Would a Gemini-CLI-burned developer find it credible? (The skeptic test.)
- [ ] Is it in English? (ZenCode is a global project; public materials are
      English. Community translations welcome, maintainers publish English.)

---

*This is a living document. Propose changes via PR - messaging is code.*
