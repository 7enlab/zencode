# PLAN: folder trust (branch feat/folder-trust)

Policy (Zen, 2026-10-17): in an UNTRUSTED folder ZenCode does not load project-level config that can
execute things or widen permissions. User-level (~/.zen) config always loads.

## Design
- Trust store: `~/.zen/trusted-folders.json` = `{"trusted": ["/abs/canonical/path", ...]}` (user level,
  never in the repo). A trusted path covers its subfolders. cwd == $HOME counts as trusted (the
  "project" files there are the user's own).
- Trusted when: `--trust-folder` flag, or `ZEN_TRUST_FOLDER=1|true|yes`, or cwd is in the store.
- Untrusted → from the project/local layers (`<cwd>/.zen/settings.json`, `settings.local.json`) drop:
  `hooks`, `permissions.allow`, `permissions.additionalDirectories`, `env`, `verify` (runs `sh -c`).
  `permissions.deny`/`ask`, model, baseUrl etc. still load (deny is never weakened).
  Project MCP files (`<cwd>/.mcp.json`, `<cwd>/.zen/mcp.json`) are not read.
  `allowed-tools` frontmatter of a project slash command/skill (`<cwd>/.zen/...`) is ignored.
  ZEN.md/AGENTS.md (instructions only), project skills/commands/agents bodies still load.
- REPL: if untrusted AND the folder has something gated AND stdin is a TTY → ask once at startup
  `Trust this folder? [y/N]`; yes → saved to the store; no → this session runs untrusted.
- Headless `-p`: never prompt; untrusted unless flag/env/store. When something was skipped print one
  line to stderr. stream-json `system/init` gets add-only `folder_trusted: bool`.

## Steps (each with proof)
1. `zen-ext/src/trust.rs`: is_folder_trusted / trust_folder (atomic write) / gated_project_config.
   Proof: unit tests (store roundtrip, subfolder, home, gated detection).
2. `zen-ext/src/settings.rs`: `load_settings_trust(.., trusted)`; strip keys when untrusted;
   `LoadedSettings.skipped`. Proof: unit test untrusted → hooks/allow/env/verify gone, deny kept;
   trusted → loaded; existing tests unchanged.
3. `zen-mcp/src/config.rs`: `load_mcp_configs_scoped(.., include_project)`. Proof: unit test.
4. `zen-cli`: `--trust-folder`; ResolvedConfig.folder_trusted; agent_run passes trust to MCP + clears
   project slash-command allowed-tools; init field; stderr notice; REPL prompt in lib.rs; doctor line.
   Proof: e2e tests (hook PWNED file absent untrusted / present with flag; project MCP skipped;
   project allow rule not applied; init `folder_trusted` false/true; store entry → trusted).
5. Test harness: `ZenRun` sets `ZEN_TRUST_FOLDER=1` by default (existing fixtures keep their project
   config); new trust tests remove it. Proof: `cargo test --workspace` ≥ 484 + new, clippy clean.
6. README section. PR, CI green, backup tag, merge.

## Status
- [x] 1 [x] 2 [x] 3 [x] 4 [x] 5 (496 tests pass, clippy clean; REPL prompt checked by hand under `script`: y saves, n skips, non-TTY prints notice) [ ] 6 PR/CI/merge
