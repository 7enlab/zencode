# Harness² (arXiv:2610.14224, Google Cloud AI Research, 2026-10-09) — lessons for ZenCode

Source: https://arxiv.org/abs/2610.14224 · code github.com/google-research/harness2 (CC BY 4.0). Read 2026-10-15.

## What it is
Self-improving harness at test time. Per task: run reference → propose 2 harness edits (invariant: planning/tool use/context; domain-specific) → probe each on the same task under matched conditions → compose edits backed by behavioural contrast. After each task, lessons + reusable components go to a library ("improvement harness") that guides future proposals. Harness = 8 files: system prompt, guardrails, memory, tool guidance, subagents, skills, scripts, plugin (middleware hook).

## Numbers that matter for us
- Same model (Gemini 3.5 Flash), OpenCode → Harness² k=5: +7.4/+8.4/+9.4 on WB Code/Web/Office; beats unchanged Claude Code/Codex by 4.6–5.8. Harness, not model, is the lever (validates ZenCode r7: oss-20b 8→10/10 from harness fixes only).
- Component ablation (E.1): removing skills −5.3 avg, scripts −3.7, prompt group −1.5. Prompt edits are the weakest lever — matches our C2 (neutral) and C6 (reverted).
- Tool-error rate −20 to −37% after one step; calls not always fewer.
- Failure analysis: wrong candidate selection, shared wrong premise across candidates, and run-to-run noise. They recommend repeated executions to separate edit effect from variance.
- Case: model emitting tool calls as plain text; a guidance edit forbidding pseudo-calls 60→100. ZenCode already repairs this (repair.rs `tool_call_in_text`).

## Applicable to ZenCode
1. **Bench rule (S, process):** keep/revert a change only on ≥2 runs per model; ±1/10 is noise (we saw it at C2/C3). Write into bench README.
2. **Prefer skills/scripts/middleware over prompt text (S, policy):** for r8 items, ship behaviour as enforced code (hooks, repairs) or on-demand skills, not longer system prompt. Keep prompt byte-stable for cache.
3. **Harness-as-files profile (M):** `--harness <dir>` loading system prompt, guardrails, tool guidance, skills, scripts from files so variants are diffable and A/B-able without rebuilding. Mostly wiring existing pieces (`--system-prompt`, skills, hooks).
4. **zen-bench autoharness loop (L, experimental):** script that runs Propose-Probe-Compose on our 10 hidden-grader tasks with free models, a lessons file carried across iterations. Automates what r7 did by hand. Needs 429-resilient free routing first.
5. **Tool-error rate as a bench metric (S):** log per-run tool error % next to pass/turns.

Status: proposals only; not in PLAN.md until Zen approves.
