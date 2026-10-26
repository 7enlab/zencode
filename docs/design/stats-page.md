# Public Stats Page - Design Doc

**Status:** Draft (leader proposal, 2026-10-20)
**URL:** `zencodego.com/stats` (ships with the website, after v1.0.0)
**Data source:** anonymous user counter API - see `docs/design/user-counter.md`
**References:** Homebrew analytics (formulae.brew.sh/analytics), pypistats, star-history, npmtrends.

## 1. Purpose

- **Research:** credible, public usage numbers anyone can cite.
- **Communication:** launch proof ("X installs, Y monthly users"), growth story for press/community.
- **Trust:** the counting method is as public as the numbers - fits "Trust what runs".

## 2. Metrics shown (public)

- **Hero:** total installs + monthly active users + last-updated timestamp, with the note "counted anonymously" right next to the numbers.
- **Growth:** line chart of DAU / WAU / MAU over 90 days (toggle 30 / 365 days).
- **Platform split:** Linux / macOS / Windows (bar or pie) - proves cross-platform parity.
- **Versions:** % of users per version (horizontal bar); very old versions grouped as "older".
- **Growth rate:** MAU month-over-month % - for release and press comms.

## 3. Metrics NOT shown (deliberately)

- Anything derived per install_id, per hour, or per country (de-anonymization risk; IPs are never stored anyway).
- Build errors / crash counts (internal quality metrics only).
- Opt-out counts (invites argument, adds nothing).

## 4. Page layout (top → bottom)

1. **Hero** - two big numbers (installs, MAU), updated date, "100% anonymous · no IP stored · no accounts".
2. **Growth chart** - MAU/WAU/DAU line chart with range toggle.
3. **Platform split** - OS distribution (+ arch, grouped).
4. **Versions** - adoption bars.
5. **Embed & API** - badge snippets and the public JSON endpoint (`GET /stats`), documented like Homebrew's analytics API.
6. **Methodology & Privacy** - prominent section (not a buried footer): what is sent, what is never sent, how to opt out, retention (raw 13 months → aggregates forever). Links to `docs/privacy.md`.

## 5. Badges & embeds

- shields.io **endpoint badge**: `GET count.zencodego.com/badge/installs.json` → `{"schemaVersion":1,"label":"users","message":"12.4k","color":"brightgreen"}` - used in README, self-updating, free.
- shields.io **dynamic JSON** badge for MAU straight from `/stats` (no extra code).
- **OG/social card** for `/stats` rendering the hero numbers - makes the page shareable on X/LinkedIn during launch.
- After the repo goes public (v1.0.0): star-history chart embedded alongside user stats.

## 6. Notes

- Badge numbers may lag ~15 minutes (shields.io cache) - acceptable, set a sensible `cacheSeconds`.
- VS Code publishes user numbers via blog posts, not a live page - our live page + blog announcements at milestones is the stronger play.
- Implementation lands with the website build (post-v1.0.0 stable); the `/stats` API ships earlier with the counter backend (Việc 7).
