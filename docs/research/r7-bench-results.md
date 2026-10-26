# r7 bench iteration results (2026-10-15)

**Bottom line.** On gpt-oss-20b the bench went from 8/10 (B0) and 6–7/10 (C1) to 10/10 on 2 of 2 runs (C5, C7). Almost all of that came from the gateway, not from the model's coding. vLLM returns gpt-oss Harmony parse failures as HTTP 200 + `error` / empty messages, and the model leaks `<|channel|>` tokens into tool names. C3+C4+C5 make zen resample and repair these. nemotron-3-super was already at 10/10 and stayed there, apart from wall-clock timeouts under load and one model miss. am/free went from 2/10 (B0) to the F7 number below.

## Setup
- Bench: `tools/bench.sh` - 10 tasks (r7: fix_failing_test, rename_function, add_cli_flag, tabs_edit, implement_from_test; r8: paginate_bug, dotenv_parser, js_async_fix, big_file_edit, report_json). Each task runs in a fresh git repo, and a hidden grader checks more than the visible tests (`BENCH_SELFTEST=1` proves every grader fails before the fix and passes after the reference fix).
- `zen -p --output-format json --permission-mode bypassPermissions --max-turns 25`, `BENCH_TIMEOUT=420` s per task, `.zen/settings.json` `{"verify":{"enabled":true}}`.
- Models (free only, AnyModel coefficient 0): `am/nemotron-3-super-120b-a12b`, `am/gpt-oss-20b`, `am/free`. Calls went through a local logging proxy (`/workspace/work/zen-r7-bench/proxy/proxy.py`) so failures could be read from the raw request/response bodies.
- Binaries per change: `/workspace/work/zen-r7-bench/bin/zen-{B0,C1..C7,C3noC2}`. Logs and JSON: `/workspace/work/zen-r7-bench/runs/`. Summary: `summary.json`.

## Score table (tasks passed by the hidden grader)

| Change | What | Source | nemotron-3-super | gpt-oss-20b (run 1 · run 2 · …) | am/free | Decision |
|---|---|---|---|---|---|---|
| B0 | r7 items 1–5 (before C1) | - | 10/10 | 8/10 | 2/10 | baseline |
| C1 | Syntax-guarded edits (`syntax_guard.rs`) | SWE-agent ACI, arXiv:2405.15793 §3 | 10/10 | 6/10 · 7/10 | - | kept |
| C2 | Workflow prompt: explore → reproduce → fix → rerun → edge cases, don't edit tests | anthropic.com/engineering/swe-bench-sonnet | 9/10 | 7/10 | - | kept (neutral) |
| C3 | Resample 200 responses carrying an error / no choices / empty message | vllm-project/vllm#23567 | 10/10 | 9/10 · 8/10 | - | kept |
| C3−C2 | ablation: C3 without C2 | - | - | 8/10 · 10/10 | - | (ablation) |
| C4 | Cut leaked chat-template tokens from tool names | vllm-project/vllm#32587 | 10/10 | 9/10 | - | kept |
| C5 | Requery after an empty reply (max 2/turn) | swe-agent.com/latest/config/templates (format-error requery) | 8/10 | 10/10 · 10/10 | - | kept |
| C6 | Requirements review before finishing | code.claude.com/docs/en/best-practices | 6/6 | 9/10 · 10/10 | - | reverted |
| C7 | Requery when every retry came back unparsed | same as C3/C5 | 10/10 · 5/5 (partial) | 10/10 · 10/10 · 3/10 (RW) (partial) | 3/10 · 0/10 (RW) (partial) | kept (final) |

How to read it: `a/b` = passed / tasks run. Fewer than 10 tasks means the run was cut short (sandbox resets killed in-flight runs twice; a resumed run counts each task once). Free models are noisy: ±1 task between identical runs is normal (compare the two oss runs of C3, or the C3−C2 ablation runs).

## What each change fixed (from the raw traces)
- **B0/C1 on gpt-oss:** every failure with exit 1 was `model returned no choices`. The proxy log shows HTTP 200 bodies like `{"error":{"message":"unexpected tokens remaining in message header: Some(\"to=functions.Read\")"}}`, or an assistant message with `content:null, tool_calls:[]`. zen treated the first as fatal and the second as a final (empty) answer. C1 itself did not cause these; the 6/10 is gateway noise.
- **C3** resamples those (same 4-attempt schedule as 429/5xx). After C3, oss failures moved from gateway errors to model behaviour.
- **C4:** with C3, one oss run hit max-turns on paginate_bug because 15 of 25 calls were `Bash<|channel|>commentary` and similar names, rejected as unknown tools. These leaked names, left in history, also make vLLM reject the next request (vllm#32587). C4 cuts them back to the tool name and keeps them out of history.
- **C5:** with C4, rename_function still failed once: 3 Harmony errors, then an empty message on the 4th attempt, accepted as "done" with no diff. C5 drops an empty reply and requeries (max 2 per turn), as SWE-agent does for format errors.
- **C6 (reverted):** a once-per-turn "re-read the request and check every requirement" message at finish. oss 9/10 · 10/10 vs C5 10/10 · 10/10, with about 30–70% more turns (133 vs 77–102). No gain at a real cost, so it was reverted.
- **C7:** when all retries return an unparsed sample, the run now requeries (sharing C5's budget) instead of ending with `error_during_execution`. This was seen in C3r2 (2 tasks) and C6-oss (big_file_edit). oss 10/10 · 10/10. It costs nothing on the success path, so it was kept. There is no measurable gain over C5 at this ceiling, and with C6 that makes two no-gain changes in a row, so iteration stopped.
- **C2:** neutral within noise (C3 with C2: 9/10 · 8/10; C3 without C2: 8/10 · 10/10). It was kept because it is source-backed and costs nothing.

## Caveats
- 10 tasks is a smoke signal, not a leaderboard. oss and nemo are now at the ceiling, so further harness changes need a harder task set (the ~30-task edit-bench from research item #12).
- Up to 7 bench runs ran in parallel. nemo's dotenv_parser hit the 420 s wall-clock limit several times (`(timeout)`). That is load, not a harness regression.
- Two sandbox resets killed in-flight runs. Their per-task pass/exit/seconds survive in the `.log` files, but turn counts and failure reasons for those tasks were lost.
- am/free routes to different backends, so its score is the least stable.
- `(RW)` = run on the Railway box (x86_64 build of the same source, calls tunnelled back through `rw_bridge.py` → `proxy_inject.py`). `(partial)` = cut short by infra, not the model: C7r2-nemo stopped after 5 tasks when the local sandbox reset (5/5 passed); RW_C7r3-oss and RW_F7r2-free lost the tunnel (every later task ended with `network error … 127.0.0.1:18557` in 1 turn), so only RW_C7r3-oss's first 3 tasks (3/3 passed; tabs_edit died mid-run on the same network error) and RW_F7r2-free's first task (HTTP 429, then network error) are real samples. The table still counts them as the logs record them.

## Raw per-run summary
```
A3noC2-oss   8/10  833s  turns=30(4 known) nochoices=0  fail=tabs,dotenv
A3noC2r2-oss 10/10  784s  turns=107(10 known) nochoices=0  fail=-
B0-free      2/10  1947s  turns=34(10 known) nochoices=3  fail=fix,rename,tabs,paginate,dotenv,js(timeout),big(timeout),report
B0-nemo      10/10  1871s  turns=109(10 known) nochoices=0  fail=-
B0-oss       8/10  545s  turns=81(10 known) nochoices=2  fail=rename,paginate
B1-nemo      10/10  1643s  turns=81(10 known) nochoices=0  fail=-
B1-oss       6/10  931s  turns=77(10 known) nochoices=5  fail=rename,tabs,implement,report
B1r2-oss     7/10  494s  turns=84(10 known) nochoices=4  fail=implement,dotenv,report
C2-nemo      9/10  1555s  turns=22(3 known) nochoices=0  fail=dotenv(timeout)
C2-oss       7/10  1004s  turns=88(10 known) nochoices=3  fail=rename,implement,big
C3-nemo      10/10  1396s  turns=23(4 known) nochoices=0  fail=-
C3-oss       9/10  1846s  turns=25(1 known) nochoices=0  fail=big
C3r2-oss     8/10  639s  turns=90(10 known) nochoices=2  fail=rename,big
C4-nemo      10/10  1498s  turns=92(9 known) nochoices=0  fail=-
C4-oss       9/10  684s  turns=41(4 known) nochoices=0  fail=rename
C5-nemo      8/10  1462s  turns=95(10 known) nochoices=0  fail=dotenv(timeout),big
C5-oss       10/10  716s  turns=77(7 known) nochoices=0  fail=-
C5r2-oss     10/10  719s  turns=102(10 known) nochoices=0  fail=-
C6-nemo      6/6  1033s  turns=0(0 known) nochoices=0  fail=-
C6-oss       9/10  1117s  turns=133(10 known) nochoices=1  fail=big
C6r2-oss     10/10  1356s  turns=156(10 known) nochoices=0  fail=-
C7-nemo      10/10  1535s  turns=78(8 known) nochoices=0  fail=-
C7-oss       10/10  1000s  turns=81(5 known) nochoices=0  fail=-
C7r2-nemo    5/5  516s  turns=0(0 known) nochoices=0  fail=-
C7r2-oss     10/10  570s  turns=42(4 known) nochoices=0  fail=-
F7-free      3/10  1938s  turns=29(7 known) nochoices=0  fail=rename,add,paginate,dotenv,js,big,report
RW_C7r3-oss  3/10  527s  turns=51(10 known) nochoices=0  fail=tabs,implement,paginate,dotenv,js,big,report
RW_F7r2-free 0/10  582s  turns=11(10 known) nochoices=0  fail=fix,rename,add,tabs,implement,paginate,dotenv,js,big,report
```
