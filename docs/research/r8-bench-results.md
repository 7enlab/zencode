# R8 benchmark results (2026-10-16)

## Research
- zen-C7, 24 r8-hard tasks, 2 runs per model, free models only. 420s timeout. Two sandbox resets lost nemo tail results; 12 tasks re-run in 4 lanes and merged. Per-type error breakdown uses surviving traces only; totals come from bench-json lines.

## Results
| task | H0-oss-r1 | H0b-oss | H0r1-nemo | H0r2-nemo |
|---|---|---|---|---|
| py_kwarg_rename | M | ✓ | T | T |
| py_lru_bug | ✓ | ✗ | ✓ | ✓ |
| py_long_file | M | ✗ | ✓ | ✗ |
| py_distractor | ✓ | ✗ | ✓ | ✗ |
| py_cli_feature | ✗ | ✓ | ✓ | ✓ |
| py_intervals | ✓ | M | ✓ | ✓ |
| py_csv_escape | ✓ | ✓ | ✓ | ✓ |
| py_retry_feature | ✓ | ✓ | ✓ | ✓ |
| rs_ringbuf_bug | ✓ | ✓ | T | ✓ |
| rs_field_rename | M | ✓ | ✓ | ✓ |
| rs_duration_parse | ✓ | ✓ | T | T |
| rs_shape_triangle | ✓ | ✗ | ✓ | T |
| rs_wordfreq_bug | ✓ | ✗ | ✓ | T |
| go_fetchall_bug | ✗ | ✗ | I | I |
| go_semver_bug | ✓ | ✓ | ✓ | T |
| go_rename_config | M | M | ✓ | ✓ |
| go_todo_json | ✓ | ✗ | T | T |
| ts_emitter_bug | ✓ | ✓ | T | I |
| ts_groupby_feature | ✓ | ✓ | ✓ | ✓ |
| ts_money_cents | ✓ | ✓ | ✓ | I |
| js_routes_long | ✓ | ✓ | ✓ | ✓ |
| sh_backup_spaces | ✓ | ✗ | ✓ | ✓ |
| sh_rotate_flags | ✓ | ✓ | ✓ | ✓ |
| sh_csv_sum | M | ✓ | ✓ | ✓ |

| run | passed | timeouts (T) | infra (I) | max-turns (M) | mean turns (finished) | tool calls | tool errors | tool-error rate | 429 retries |
|---|---|---|---|---|---|---|---|---|---|
| H0-oss-r1 | 17/24 | 0 | 0 | 6 | 17.3 | 391 | 80 | 20.5% | 0 |
| H0b-oss | 14/24 | 2 | 0 | 3 | 16.4 | 336 | 72 | 21.4% | 0 |
| H0r1-nemo | 18/24 | 11 | 1 | 1 | 14.7 | 179 | 11 | 6.1% | 0 |
| H0r2-nemo | 13/24 | 11 | 3 | 2 | 13.2 | 160 | 9 | 5.6% | 0 |

Legend: ✓ pass, ✗ fail, T timeout (exit 124, may still pass grader), I infra (gateway 504), M max-turns.

- Best: nemo run1 18/24, oss run1 17/24. oss ~21% tool-error rate (Bash non-zero, leaked `<|channel|>` token in tool names with broken JSON, bad Grep regex). nemo ~6% but ~11 timeouts per run plus 504s. oss fails mostly on max-turns. New baseline; not comparable to r7.

## Ideas (P0 for harness)
1. Sanitize tool names: strip the `<|channel|>...` suffix and repair near-valid JSON args (close brackets) before reporting an error.
2. Grep: fall back to literal search (`rg -F`) on invalid regex.
3. Bounded retries for 504/5xx plus a cap on total 429 wait time.
4. Time budget: warn the model when ~20% of time/turns remain, encourage an early finish.
5. Adaptive max-turns: extend a few turns if the latest turn made progress (more tests passing).
