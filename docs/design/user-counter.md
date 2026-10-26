# User Counter - Design Doc

**Status:** Draft (leader proposal, 2026-10-20)
**Goal:** Count anonymous ZenCode users for research and communication (launch numbers, growth tracking, public community stats).

## 1. Principles

1. **Anonymous only.** No email, no IP, no account, no code content, no file paths. Ever.
2. **Opt-out respected.** `DO_NOT_TRACK=1` or `ZENCODE_TELEMETRY=0` or `zen config set telemetry off` disables all pings. First-run shows a one-line notice.
3. **Transparent.** Method documented publicly (this file); counting backend code is open source; raw data never published, only aggregates.
4. **Minimal.** One ping per 24h max, tiny payload, no cookies, no fingerprinting.
5. **BYOK-clean (Constitution VIII).** The counter uses no API keys at all - not the team's, not the user's. Model calls elsewhere in ZenCode always run on the user's own keys/endpoints; this counter is separate, keyless infrastructure.

## 2. What is counted

- **Installs (total):** unique anonymous `install_id` (UUID v4 generated on first run, stored in the config dir).
- **DAU / WAU / MAU:** distinct install_ids seen in the last 1/7/30 days.
- **Version + OS distribution:** from the ping payload, aggregated only.

A "user" = a distinct install_id. Multi-device installs count as multiple - documented openly, acceptable for a v1 counter.

## 3. Mechanism (client)

On startup, `zen` checks `~/.config/zencode/config.toml`:
- If `telemetry = "off"` or env `DO_NOT_TRACK=1` / `ZENCODE_TELEMETRY=0` → skip entirely.
- If last ping > 24h ago → POST `https://count.zencodego.com/ping` with body:
  ```json
  {"install_id": "<uuid>", "version": "0.1.0", "os": "linux", "arch": "x86_64"}
  ```
- Ping is fire-and-forget with a 3s timeout; failure never blocks or errors visibly.

## 4. Backend (server)

- **Recommended:** Cloudflare Workers + D1 (free tier: 100k requests/day - far above needs).
- Server stores `SHA-256(install_id)`, date, version, os, arch. **IP is never stored.**
- Public read API: `GET /stats` → `{installs, mau, wau, dau, versions, updated_at}` - feeds the website badge and launch communications.
- Anti-gaming: rate-limit one ping per install_id per 12h; drop malformed payloads.
- Retention: raw pings 13 months, aggregates kept forever.

## 5. Privacy & trust (fits "Trust what runs")

- First-run notice: `ZenCode counts anonymous usage (zen config set telemetry off to opt out).`
- Documented in README and `docs/privacy.md` before any release ships this.
- No third-party analytics SDKs. No cross-service correlation.

## 6. Rollout

- Phase 1: implement client ping + Workers/D1 backend + public stats API (target: v0.1.0 internal).
- Phase 2 (public, v1.0.0): website badge + launch press numbers.

## 7. Decision (owner-approved, 2026-10-20)

**OPT-OUT.** Anonymous counter on by default, clear first-run notice + easy opt-out (`DO_NOT_TRACK=1`, `ZENCODE_TELEMETRY=0`, or `zen config set telemetry off`). Industry standard for anonymous install counts (Homebrew, VS Code telemetry levels); numbers stay credible for research and communication. Opt-in was rejected: it under-reports heavily, defeating the counter's purpose.
