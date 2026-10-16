# R8 benchmark results / Kết quả benchmark r8 (2026-10-16)

## Nghiên cứu / Research
- VI: zen-C7, 24 bài r8-hard (py/rs/go/ts/js/sh), 2 lượt mỗi model, chỉ model free (gpt-oss-20b, nemotron-3-super). Timeout 420s/bài. Sandbox reset 2 lần (~11:09, ~11:56 UTC) làm mất đuôi kết quả nemo; 12 bài chạy lại qua 4 lane (lanes3) rồi gộp bằng merge.sh. Trace oss mất một phần nên thống kê lỗi theo loại chỉ dựa trên trace còn lại; tổng tool call/lỗi lấy từ bench-json (đầy đủ).
- EN: zen-C7, 24 r8-hard tasks, 2 runs per model, free models only. 420s timeout. Two sandbox resets lost nemo tail results; 12 tasks re-run in 4 lanes and merged. Per-type error breakdown uses surviving traces only; totals come from bench-json lines.

## Kết quả / Results
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

Ký hiệu / Legend: ✓ pass, ✗ fail, T timeout (exit 124, may still pass grader), I infra (gateway 504), M max-turns.

- VI: Tốt nhất: nemotron lượt 1 18/24, oss lượt 1 17/24. oss lỗi tool ~21% (Bash exit≠0 chủ yếu do test fail; rò token `<|channel|>` vào tên tool + JSON args hỏng; Grep regex sai). nemo chỉ ~6% lỗi tool nhưng 11 timeout/lượt + 504. oss trượt chủ yếu do chạm max-turns. Bộ khó hơn r7 (10/10) nên đây là baseline mới, không so trực tiếp.
- EN: Best: nemo run1 18/24, oss run1 17/24. oss ~21% tool-error rate (Bash non-zero, leaked `<|channel|>` token in tool names with broken JSON, bad Grep regex). nemo ~6% but ~11 timeouts per run plus 504s. oss fails mostly on max-turns. New baseline; not comparable to r7.

## Ý tưởng / Ideas (P0 cho harness)
1. Sanitize tên tool: cắt hậu tố `<|channel|>...` và sửa JSON args gần đúng (đóng ngoặc) trước khi báo lỗi / strip channel suffix, repair near-valid JSON.
2. Grep: regex lỗi thì tự fallback tìm literal (`rg -F`) / literal fallback on invalid regex.
3. Retry có giới hạn cho 504/5xx + trần tổng thời gian chờ 429 / bounded 5xx retry and total 429 wait cap.
4. Ngân sách thời gian: cảnh báo model khi còn ~20% thời gian/lượt, khuyến khích chốt sớm / time-budget nudge near the limit.
5. Max-turns động: gia hạn vài lượt nếu lượt gần nhất có tiến bộ (test pass tăng) / adaptive max-turns on progress.
