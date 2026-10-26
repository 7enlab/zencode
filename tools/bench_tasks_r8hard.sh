










R8HARD_TASKS=(
  py_kwarg_rename py_lru_bug py_long_file py_distractor py_cli_feature py_intervals
  py_csv_escape py_retry_feature
  rs_ringbuf_bug rs_field_rename rs_duration_parse rs_shape_triangle rs_wordfreq_bug
  go_fetchall_bug go_semver_bug go_rename_config go_todo_json
  ts_emitter_bug ts_groupby_feature ts_money_cents js_routes_long
  sh_backup_spaces sh_rotate_flags sh_csv_sum
)


_r8_rs_hidden() {
  mkdir -p tests
  cat > "tests/$1.rs"
  cargo test --quiet --test "$1" >/dev/null 2>&1
  local r=$?
  rm -f "tests/$1.rs"
  return $r
}

_r8_go_hidden() {
  cat > "$1/zz_hidden_test.go"
  go test -count=1 -timeout 90s "./$1" >/dev/null 2>&1
  local r=$?
  rm -f "$1/zz_hidden_test.go"
  return $r
}

_r8_node_hidden() {
  cat > "$1"
  node --experimental-strip-types --no-warnings --test "$1" >/dev/null 2>&1
  local r=$?
  rm -f "$1"
  return $r
}

_R8HARD_DIR="$(dirname "${BASH_SOURCE[0]}")/r8hard"
for _f in py rs go ts sh; do

  . "$_R8HARD_DIR/$_f.sh"
done
unset _f
