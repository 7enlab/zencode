# zen bench, "r8-hard" task set (sourced by tools/bench.sh; select with BENCH_SET=r8-hard).
# The r7+r8 set (10 tasks) is saturated (nemotron-3-super and gpt-oss-20b both 10/10), so this
# set is harder: multi-file refactors, bug fixes that need the test read, small features,
# precise edits in long files and repos with distractor copies, in Python, Rust, Go, TypeScript
# (Node 22 --experimental-strip-types, no tsc), JavaScript and shell.
# Same conventions as the other sets: setup_<t> writes the repo, prompt_<t> is all the model
# sees, check_<t> is the hidden grader (it runs in the repo after the agent, may write a hidden
# test file there and removes it again; the model never sees it), solve_<t> is the reference fix
# that BENCH_SELFTEST=1 uses to prove each grader fails on the fixture and passes after the fix.
# Needs on PATH: python3, node >= 22.6, cargo, go >= 1.22, bash, awk, git.

R8HARD_TASKS=(
  py_kwarg_rename py_lru_bug py_long_file py_distractor py_cli_feature py_intervals
  py_csv_escape py_retry_feature
  rs_ringbuf_bug rs_field_rename rs_duration_parse rs_shape_triangle rs_wordfreq_bug
  go_fetchall_bug go_semver_bug go_rename_config go_todo_json
  ts_emitter_bug ts_groupby_feature ts_money_cents js_routes_long
  sh_backup_spaces sh_rotate_flags sh_csv_sum
)

# Hidden Rust test: stdin is the test file, $1 its name; built and run, then removed.
_r8_rs_hidden() {
  mkdir -p tests
  cat > "tests/$1.rs"
  cargo test --quiet --test "$1" >/dev/null 2>&1
  local r=$?
  rm -f "tests/$1.rs"
  return $r
}
# Hidden Go test: stdin is the test file, $1 the package dir.
_r8_go_hidden() {
  cat > "$1/zz_hidden_test.go"
  go test -count=1 -timeout 90s "./$1" >/dev/null 2>&1
  local r=$?
  rm -f "$1/zz_hidden_test.go"
  return $r
}
# Hidden TS/JS test: stdin is the test file (written at the repo root as $1).
_r8_node_hidden() {
  cat > "$1"
  node --experimental-strip-types --no-warnings --test "$1" >/dev/null 2>&1
  local r=$?
  rm -f "$1"
  return $r
}

_R8HARD_DIR="$(dirname "${BASH_SOURCE[0]}")/r8hard"
for _f in py rs go ts sh; do
  # shellcheck source=/dev/null
  . "$_R8HARD_DIR/$_f.sh"
done
unset _f
