# E2E Test Coverage Notes

This file documents the end-to-end test coverage for zen-cli, mapping each test to the SPEC requirements.

## Test Files Overview

| File | Tests | Status | SPEC Sections |
|------|-------|--------|---------------|
| `headless_json.rs` | 5 | existing | §14 (output formats), §10 (resume), §3 (tools) |
| `headless_output.rs` | 8 | new | §14 (exact field sets), exit codes, stdin |
| `permissions.rs` | 7 | new | §5 (permission modes, rules) |
| `settings_layering.rs` | 6 | new | §12 (settings precedence) |
| `memory_commands_skills.rs` | 8 | new | §6 (slash commands), §7 (memory, skills) |
| `hooks.rs` | 7 | new | §13 (hooks) |
| `mcp.rs` | 4 | new | §8 (MCP servers) |
| `subagents.rs` | 5 | new | §9 (sub-agents/Task tool) |
| `sessions.rs` | 6 | new | §10 (session persistence) |
| `compaction.rs` | 4 | new | §11 (auto-compact, /compact) |

**Total: 60 test scenarios**

---

## Detailed Test Scenarios

### headless_json.rs (existing)
1. `headless_json_against_mock_server` - JSON output shape
2. `headless_stream_json_against_mock_server` - stream-json lines (init, assistant, result)
3. `headless_text_mode_prints_result_only` - text mode prints only answer
4. `resume_replays_prior_messages` - --resume replays transcript
5. `tool_call_round_trip_and_allowed_tools_filter` - tool call + --allowedTools filters init tools

### headless_output.rs (new)
1. `json_output_result_shape_and_session_id` - exact field set on result, session_id from --session-id
2. `text_output_is_just_the_answer` - text mode = only assistant text
3. `stream_json_line_shapes_and_order` - init/assistant/result order, all lines have session_id, init field set
4. `model_error_yields_error_result_and_nonzero_exit` - HTTP 400 → is_error=true, exit≠0
5. `invalid_flag_is_rejected_with_stderr` - unknown flag → exit≠0, stderr message
6. `invalid_output_format_is_rejected` - --output-format yaml → exit≠0
7. `empty_prompt_reports_error` - no prompt arg + no stdin → clean error
8. `stdin_is_prepended_to_prompt` - piped stdin + arg prompt → stdin first in messages
9. `harness_timeout_guard_exists` - sanity check on timeout infrastructure

### permissions.rs (new)
1. `default_headless_denies_write_tool` - default mode + no prompter → Write denied, in permission_denials
2. `bypass_permissions_allows_write` - --permission-mode bypassPermissions → Write allowed
3. `accept_edits_allows_write_but_not_bash` - acceptEdits → Write allowed, Bash denied
4. `plan_mode_denies_write` - plan mode → Write denied
5. `allowed_tools_write_permits_write_in_default_mode` - --allowedTools Write → Write allowed in default
6. `disallowed_tools_removes_tool_from_request` - --disallowedTools Read → Read removed from init tools list
7. `settings_deny_beats_cli_allow` - project deny Write beats --allowedTools Write
8. `read_only_tools_allowed_by_default` - Read/Glob/Grep allowed without --allowedTools

### settings_layering.rs (new)
1. `cli_model_flag_overrides_settings` - --model > all settings
2. `inline_settings_overrides_file_settings` - --settings JSON > project file
3. `project_overrides_user` - project settings.json > user settings.json
4. `local_overrides_project` - settings.local.json > settings.json
5. `empty_setting_sources_loads_no_files` - --setting-sources "" → defaults only
6. `base_url_from_settings` - baseUrl from settings used (verified via config resolution)
7. `permissions_allow_merge_across_layers` - CLI --allowedTools adds to project allow list

### memory_commands_skills.rs (new)
1. `zen_md_project_memory_in_system_prompt` - project ZEN.md in system message
2. `zen_md_user_and_project_both_loaded` - user + project ZEN.md both in system
3. `bare_excludes_memory_and_skills` - --bare excludes ZEN.md and skills
4. `custom_slash_command_expands_arguments` - $ARGUMENTS expansion in custom command
5. `custom_slash_command_positional_args` - $1, $2 expansion
6. `project_custom_command_overrides_user` - project command overrides user same name
7. `skills_listed_and_tool_works` - skill in system prompt, Skill tool returns body
8. `skill_tool_unknown_returns_error` - Skill tool errors for unknown name
9. `multiple_skills_user_and_project` - multiple skills, project overrides by name

### hooks.rs (new)
1. `pre_tool_use_exit_2_blocks_tool` - PreToolUse exit 2 → tool blocked, stderr in tool result
2. `pre_tool_use_exit_0_allows_tool` - PreToolUse exit 0 → tool proceeds
3. `pre_tool_use_other_exit_logs_but_allows` - PreToolUse exit 3 → logged to stderr, tool proceeds
4. `user_prompt_submit_hook_adds_context` - UserPromptSubmit stdout → added to user prompt
5. `user_prompt_submit_exit_2_blocks_prompt` - UserPromptSubmit exit 2 → stop_run, no model call
6. `stop_hook_feedback_continues_loop` - Stop hook feedback → continues agent loop
7. `session_start_hook_runs` - SessionStart hook executes (no crash)
8. `multiple_pre_tool_use_hooks_one_blocks` - multiple hooks, one blocks wins

### mcp.rs (new)
1. `mcp_server_connected_and_tool_works` - fake python MCP server → connected, tool callable
2. `mcp_server_failed_shows_failed_status` - broken server → "failed" in init mcp_servers
3. `multiple_mcp_servers_mixed_status` - one good + one bad → mixed statuses
4. `strict_mcp_config_ignores_file_configs` - --strict-mcp-config ignores .mcp.json

### subagents.rs (new)
1. `sub_agent_task_tool_works` - Task tool calls sub-agent, sub-agent has custom prompt
2. `sub_agent_inherits_tools_when_not_specified` - null tools → inherits parent tools
3. `builtin_general_purpose_agent_available` - general-purpose always available
4. `sub_agent_depth_limit` - depth limit prevents infinite recursion
5. `sub_agent_custom_model` - sub-agent uses its own model if specified

### sessions.rs (new)
1. `resume_session_replays_transcript` - --resume replays prior messages
2. `continue_picks_latest_session` - --continue picks newest by mtime
3. `fork_session_creates_new_id` - --fork-session creates new session_id but replays
4. `no_session_persistence_writes_no_file` - --no-session-persistence → no .jsonl
5. `session_id_consistent_across_turns` - same session_id across --resume calls
6. `resume_nonexistent_session_creates_new` - --resume missing id → new session (no crash)

### compaction.rs (new)
1. `auto_compact_triggers_summary` - autoCompact threshold → summary request on turn 2
2. `manual_compact_slash_command` - /compact → summary request
3. `compaction_keeps_last_n_messages` - keepAfterCompact keeps last N messages
4. `pre_compact_hook_can_block` - PreCompact hook exit 2 blocks compaction

---

## Test Infrastructure

**Common harness** (`common/mod.rs`):
- `MockServer` - scripted HTTP server with queued replies, captures full requests
- `TempDir` - unique per-test temp directories with auto-cleanup
- `ZenRun` - builder for `zen` subprocess with hermetic env (HOME, no API keys)
- `read_request` - reads full HTTP request including Content-Length body
- `request_json`, `request_messages`, `system_message` - assertion helpers
- `ndjson`, `last_result`, `assert_fields` - output parsing

**Key design decisions**:
- Each test gets unique temp HOME/cwd → no pollution from developer settings
- Mock server binds to `127.0.0.1:0` → random port, no conflicts
- Full request capture → assert on actual wire format sent to model
- Generous timeouts (60s default, 90s for sub-agents) → no flaky hangs
- Subprocess runs with `ZEN_API_KEY=dummy`, all `ZEN_*`/`OPENAI_*` removed

---

## Known Gaps / Future Work

1. **WebFetch tool** - not tested (requires HTTP mock)
2. **TodoWrite tool** - not tested as end-to-end (has unit tests in zen-tools)
3. **REPL mode** - not tested (interactive, hard to script)
4. **Agent /mcp subcommands** - not tested (separate from headless -p)
5. **Input format stream-json** - not tested
6. **Fallback models** - not tested (requires multiple mock responses)
7. **Cost tracking** - total_cost_usd always 0 in mock
8. **Signal handling** - Ctrl-C during run not tested

---

## Running Tests

```bash
export PATH=$HOME/.cargo/bin:$PATH CARGO_TARGET_DIR=/tmp/zen-target
cargo test -p zen-cli --tests
```

All tests should pass once zen-tools compilation is fixed.

---

## Bugs Found Elsewhere

| Crate | Issue | Impact |
|-------|-------|--------|
| zen-tools | Stray doc comment at line 1628 | Blocks compilation; must be fixed before tests run |
| zen-cli | `ReplPrompter` not exported from `agent_run` | REPL may not build |

---