# PLAN-mcpcap — cap MCP tool output, spill to file (competitor proposal 2)

Goal: an MCP tool result larger than the Bash cap no longer floods the model's context. The model
gets the same head/tail preview + file path that Bash gives (`zen_tools::spill_output`).

Scope (minimal, no new external deps, no refactor):
1. `crates/zen-mcp/Cargo.toml`: add the internal workspace crate `zen-tools` (already in
   `[workspace.dependencies]`, no cycle: zen-tools depends only on zen-core).
   Proof: `cargo build -p zen-mcp`.
2. `crates/zen-mcp/src/manager.rs`: `render_result(result, ctx)` runs the joined content through
   `spill_output(&content, MCP_MAX_CHARS, ctx)`; `MCP_MAX_CHARS = zen_tools::builtin::BASH_MAX_CHARS`
   (30_000 chars; Bash's cap is a constant, not a setting, so this is too). isError kept.
   Non-text parts still render as `[<type> content]`; spill only sees the joined text.
   Proof: unit tests in `crates/zen-mcp/src/tests.rs`:
   - small text result unchanged (exact equality),
   - 200k-char result: reply < cap+400 chars, names the spill file, file holds the full text exactly,
   - image + small text: `[image content]` placeholder and text untouched; isError preserved on huge error.
3. Verify: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`.
4. PR to main (REVIEW.md template), CI green, backup tag `backup/main-pre-mcpcap-20261017`, merge.

Not touched: zen-agent session/event-log (another worker on feat/durable-tool-results).
