# ZenCode - daily feature ideas / Ý tưởng tính năng hằng ngày

Appended by the daily research task. Ideas already on the roadmap (`ZenCode-notion/02-roadmap.md`, `top-coding-agents-2026.md` #1–#17, r8 Code Mode / background subagents) are not repeated here.
Do mỗi ngày tác vụ nghiên cứu ghi thêm. Không lặp lại ý đã có trong roadmap.

## 2026-10-15

**News / Tin chính (Oct 13–15):**
- OpenCode v2.0.29 (Oct 14): responses that hit the output-token limit now continue automatically, with bounded retries and recovery for truncated tool calls - https://opencode.ai/changelog (#53876). v2.0.28: streamed-chunk timeout 5 → 15 min for models that pause while reasoning; request-body gzip/zstd.
- Claude Code v2.1.299 / v2.1.300 (Oct 14): idle compaction before the 1-hour cache expires; stream wait through custom base URL 180 → 300 s; fixed replies cut off at the output limit ending the turn silently; `@`-mentions can pass only the path - https://github.com/anthropics/claude-code/releases/tag/v2.1.299 , https://github.com/anthropics/claude-code/releases/tag/v2.1.300
- Pi 1.0 (Oct 14): Code Mode (QuickJS) + native MCP with tool search / progressive disclosure - https://eu.36kr.com/en/p/4025329229631363
- Martin Fowler / Naresh Jain, "Are Harnesses Worth the Cost?" (Oct 13): cheap model GPT-6 Luna 3/10 → 10/10 with tests running during development (sensors), still 33% cheaper than the frontier model - https://martinfowler.com/articles/are-harnesses-worth-the-cost.html
- Stackness (Oct 13), citing a Terminal-Bench 2.0 study: tool results are 56% of context, tool calls 28% - https://stackness.dev/blog/when-more-context-makes-your-coding-agent-worse-subagent-compact-clear-or-a-headless-loop

### 1. Auto-continue on output-token limit / Tự viết tiếp khi chạm giới hạn token đầu ra
- **Source / Nguồn:** OpenCode v2.0.29 #53876 (https://opencode.ai/changelog); Claude Code v2.1.299 fix "replies cut off at the output token limit ending the turn silently".
- **Why zen / Vì sao hợp:** EN: free models often run with small max-output limits (we already had to raise Pi's maxTokens to 32000). zen-agent passes `finish_reason` through but never acts on `"length"`: a cut text answer ends the turn, and a cut tool call goes to `repair.rs`, whose truncated-JSON repair may close the braces and *write a half file*. VI: model free hay có giới hạn đầu ra nhỏ; zen hiện bỏ qua `finish_reason: "length"` - câu trả lời bị cắt kết thúc lượt, còn tool call bị cắt có thể bị repair "đóng ngoặc" rồi ghi nửa file.
- **Sketch / Phác thảo:** `zen-agent/src/agent.rs`: if `finish_reason == "length"`: (a) text only → append partial assistant text, requery with a one-line "continue exactly where you stopped" (max 2), stitch into one assistant message; (b) tool call present → never repair-and-run; return an error result "your call was cut by the output limit; resend it smaller (split Write into Write + Edit/MultiEdit)". `repair.rs`: mark truncated-JSON repair as unsafe when the response was length-limited. Emit an `ApiRetry`-like event.
- **Effort / Công sức:** S. **Risk / Rủi ro:** some gateways return `length` with a complete call (check JSON validity first); stitching may duplicate a few tokens.
- **Measure / Đo:** add a bench task in `/workspace/work/zen-r7-bench` that forces a long file write (≥ 400 lines) with `max_tokens` capped (e.g. 2048) via settings; compare pass rate + "half-written file" count, B0 vs new bin, on `nemo`/`oss`/`free` (`run.sh LABEL BIN nemo`).

### 2. Non-destructive pruning of old tool output + spill to file / Tỉa output tool cũ (không phá transcript) + đổ ra file
- **Source / Nguồn:** Stackness / Terminal-Bench 2.0 context study (tool results = 56% of context); Pi 1.0 "context edits omit or replace content in the model's view while preserving the stored entry" (https://blog.invidelabs.com/coding-harness-architecture/); SambaNova note that passing SWE-bench runs stay under ~20–30k tokens.
- **Why zen / Vì sao hợp:** EN: free tiers meter *tokens per day* (argon 2.5M/day ran out in PLAN Step 1) and 429 more on large prompts; weak models degrade fastest as context grows. Every request re-sends every old Read/Bash/Grep output until compaction. VI: gói free tính token/ngày (argon hết 2.5M/ngày), prompt to dễ 429; model yếu kém đi nhanh khi ngữ cảnh dài. Hiện mỗi request gửi lại toàn bộ output tool cũ.
- **Sketch / Phác thảo:** `zen-agent/src/agent.rs` (request builder only, history untouched): tool results older than K model calls (default 8) and > 1500 chars are sent as a stub `[zen: output of Read src/x.rs (312 lines) elided - re-run the tool if needed]`; keep the latest result per file and any error results. `zen-tools`: outputs above the cap are written to `.zen/tmp/<call-id>.txt` and the result says "full output saved to …, Read it with offset/limit" instead of only `truncate_middle`. Settings `context.pruneAfterCalls`, `0` = off. Keep the elided prefix stable between requests so provider caching still works (prune in steps of K, not every call).
- **Effort / Công sức:** M. **Risk / Rủi ro:** the model may re-read files (more calls); careful with prompt-cache stability; must not elide the output a pending edit depends on (keep files in `read_files` stamps that are still current).
- **Measure / Đo:** bench B0 vs new bin: total `usage.prompt_tokens` per task (from the json result), pass rate, num_turns, count of 429 retries in the logs; goal ≥ 30% fewer prompt tokens with no pass-rate drop.

### 3. Deferred MCP tools behind a ToolSearch tool / Ẩn bớt tool MCP sau một tool tìm kiếm
- **Source / Nguồn:** Pi 1.0 native MCP with tool search + progressive disclosure (https://eu.36kr.com/en/p/4025329229631363); Claude Code 2.1.300 tool-host/tool-search changes (https://github.com/anthropics/claude-code/releases/tag/v2.1.300).
- **Why zen / Vì sao hợp:** EN: each MCP server adds many schemas to every request; small models pick wrong tools when the list is long, and the schemas cost tokens each call. VI: mỗi server MCP thêm nhiều schema vào mọi request; model nhỏ chọn sai tool khi danh sách dài, lại tốn token.
- **Sketch / Phác thảo:** `zen-mcp`: when MCP tools > N (default 15) or setting `mcp.deferTools: true`, register only a `ToolSearch` tool (query → top 5 names + one-line descriptions, keyword/BM25 over name+description) and a `ToolLoad` step that adds chosen schemas to the registry for the rest of the session (append-only, so the cached prefix grows instead of changing). Built-in tools always stay visible. Permission rules unchanged.
- **Effort / Công sức:** M. **Risk / Rủi ro:** an extra round trip per new tool (bad under 429); weak models may forget to search - list server names + counts in the system prompt.
- **Measure / Đo:** bench task variant with a mock MCP server exposing 40 tools of which 1 is needed; compare correct-tool rate, prompt tokens/request, pass rate (B0 vs new).

### 4. Longer, configurable model-request timeout / Timeout gọi model dài hơn, cấu hình được
- **Source / Nguồn:** OpenCode v2.0.28 (stream chunk timeout 5 → 15 min for reasoning pauses, #55032); Claude Code v2.1.299 (custom base URL stream wait 180 → 300 s).
- **Why zen / Vì sao hợp:** EN: `OpenAiClient` uses a fixed 120 s total timeout (non-streaming). Slow free reasoning models (nemotron, gpt-oss via relay) can exceed it; the retry then re-sends the whole prompt, burning daily quota and inviting 429. VI: client đặt cứng 120 s; model free suy luận chậm dễ vượt, retry gửi lại cả prompt → tốn hạn mức, dễ 429.
- **Sketch / Phác thảo:** `zen-agent/src/client.rs`: default 300 s, `ZEN_REQUEST_TIMEOUT` / settings `api.timeoutSecs`; separate connect timeout (10 s); a timeout counts as one retry, then go straight to `fallback_models`. When C8.5 (real SSE) lands, switch to an idle-chunk timeout.
- **Effort / Công sức:** S. **Risk / Rủi ro:** a dead gateway now hangs longer before failover (mitigated by the connect timeout).
- **Measure / Đo:** grep bench logs for timeout retries per run (B0 vs new) on `nemo`; wall time and pass rate per task.

**Supporting evidence for existing roadmap #7 (post-edit checker) / Bằng chứng thêm cho mục #7:** Fowler/Jain: running sensors *during* development, not only at the end, took a cheap model from 3/10 to 10/10. zen's `verify` gate runs only at finish - consider a mid-turn cadence (e.g. after every 3rd edit) when #7 is built.
