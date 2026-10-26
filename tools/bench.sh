

























set -euo pipefail


export PYTHONDONTWRITEBYTECODE=1

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/docs/e2e-runs/$(date +%F)-bench.json}"
shift || true
MODEL="${ZEN_MODEL:-am/free}"
BASE="${ZEN_BASE_URL:-}"
TIMEOUT="${BENCH_TIMEOUT:-300}"
MAX_TURNS="${BENCH_MAX_TURNS:-25}"
PAUSE="${BENCH_PAUSE:-0}"
R7_TASKS=(fix_failing_test rename_function add_cli_flag tabs_edit implement_from_test)
R8_TASKS=(paginate_bug dotenv_parser js_async_fix big_file_edit report_json)

. "$ROOT/tools/bench_tasks_r8.sh"

. "$ROOT/tools/bench_tasks_r8hard.sh"
case "${BENCH_SET:-base}" in
  base) ALL_TASKS=("${R7_TASKS[@]}" "${R8_TASKS[@]}") ;;
  r8-hard) ALL_TASKS=("${R8HARD_TASKS[@]}") ;;
  all) ALL_TASKS=("${R7_TASKS[@]}" "${R8_TASKS[@]}" "${R8HARD_TASKS[@]}") ;;
  *) echo "bench: unknown BENCH_SET '${BENCH_SET}'" >&2; exit 2 ;;
esac
RUNS="${BENCH_RUNS:-1}"


export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
export GOCACHE="${GOCACHE:-$HOME/.cache/go-build}" GOPATH="${GOPATH:-$HOME/go}"
export GOTOOLCHAIN=local GOPROXY=off



summarize() {
  python3 "$ROOT/tools/bench_summary.py" "$@"
}
if [ "${BENCH_SUMMARY:-}" = 1 ]; then
  summarize "$OUT" "$@"
  exit 0
fi

TASKS=("$@")
[ ${





setup_fix_failing_test() {
  cat > calc.py <<'EOF'
def add(a, b):
    return a - b


def mul(a, b):
    return a * b
EOF
  cat > test_calc.py <<'EOF'
import unittest
from calc import add, mul


class CalcTest(unittest.TestCase):
    def test_add(self):
        self.assertEqual(add(2, 3), 5)

    def test_mul(self):
        self.assertEqual(mul(2, 3), 6)


if __name__ == "__main__":
    unittest.main()
EOF
}
prompt_fix_failing_test() { echo "The unit tests in this repo fail (run: python3 -m unittest). Fix the bug in the code, not the tests."; }
check_fix_failing_test() { python3 -m unittest -q test_calc >/dev/null 2>&1 && grep -q "assertEqual(add(2, 3), 5)" test_calc.py; }
solve_fix_failing_test() { sed -i 's/return a - b/return a + b/' calc.py; }

setup_rename_function() {
  mkdir -p app
  cat > app/users.py <<'EOF'
def get_usr(uid):
    return {"id": uid, "name": "user%d" % uid}
EOF
  cat > app/report.py <<'EOF'
from app.users import get_usr


def report(ids):
    return ", ".join(get_usr(i)["name"] for i in ids)
EOF
  cat > main.py <<'EOF'
from app.users import get_usr
from app.report import report

print(get_usr(1)["name"])
print(report([2, 3]))
EOF
  touch app/__init__.py
}
prompt_rename_function() { echo "Rename the function get_usr to get_user everywhere in this repo (definition and all call sites). Keep behavior the same."; }
check_rename_function() {
  ! grep -rq "get_usr\b" --include=*.py . && grep -q "def get_user" app/users.py &&
    [ "$(python3 main.py 2>/dev/null)" = "$(printf 'user1\nuser2, user3')" ]
}
solve_rename_function() { sed -i 's/get_usr/get_user/g' app/users.py app/report.py main.py; }

setup_add_cli_flag() {
  cat > greet.py <<'EOF'
import argparse


def main(argv=None):
    parser = argparse.ArgumentParser(description="Greet someone.")
    parser.add_argument("--name", default="world")
    args = parser.parse_args(argv)
    message = "hello " + args.name
    print(message)


if __name__ == "__main__":
    main()
EOF
}
prompt_add_cli_flag() { echo "Add a --shout flag to greet.py. When given, the greeting is printed in upper case. Without it, output must stay exactly as it is now."; }
check_add_cli_flag() {
  [ "$(python3 greet.py --name bob --shout 2>/dev/null)" = "HELLO BOB" ] &&
    [ "$(python3 greet.py --name bob)" = "hello bob" ] && [ "$(python3 greet.py)" = "hello world" ]
}
solve_add_cli_flag() {
  sed -i 's/    parser.add_argument("--name", default="world")/&\n    parser.add_argument("--shout", action="store_true")/' greet.py
  sed -i 's/    print(message)/    print(message.upper() if args.shout else message)/' greet.py
}

setup_tabs_edit() {
  printf 'class Greeter:\n\tdef __init__(self):\n\t\tself.greeting = "hi"\n\n\tdef greet(self, who):\n\t\tif not who:\n\t\t\treturn self.greeting\n\t\treturn self.greeting + ", " + who\n' > greeter.py
}
prompt_tabs_edit() { echo "In greeter.py, make greet() return 'hello, stranger' when who is empty, and change the default greeting from 'hi' to 'hello'. Keep the file's existing indentation style."; }
check_tabs_edit() {
  python3 - <<'EOF' 2>/dev/null && grep -qP '^\t\tself.greeting' greeter.py
from greeter import Greeter
g = Greeter()
assert g.greet("") == "hello, stranger", g.greet("")
assert g.greet("ann") == "hello, ann", g.greet("ann")
EOF
}
solve_tabs_edit() { sed -i 's/"hi"/"hello"/; s/^\t\t\treturn self.greeting$/\t\t\treturn "hello, stranger"/' greeter.py; }

setup_implement_from_test() {
  cat > textstats.py <<'EOF'
"""Small text helpers."""


def word_count(text):
    return len(text.split())
EOF
  cat > test_textstats.py <<'EOF'
import unittest
from textstats import word_count, top_words


class TextStatsTest(unittest.TestCase):
    def test_word_count(self):
        self.assertEqual(word_count("a b  c"), 3)

    def test_top_words(self):
        text = "the cat and the hat and the bat"
        self.assertEqual(top_words(text, 2), [("the", 3), ("and", 2)])

    def test_top_words_is_case_insensitive_and_ties_alphabetical(self):
        self.assertEqual(top_words("B a b A c", 3), [("a", 2), ("b", 2), ("c", 1)])


if __name__ == "__main__":
    unittest.main()
EOF
}
prompt_implement_from_test() { echo "test_textstats.py imports top_words from textstats.py, which does not exist yet. Implement it so all tests pass. Do not edit the tests."; }
check_implement_from_test() {
  python3 -m unittest -q test_textstats >/dev/null 2>&1 && grep -q '("a", 2), ("b", 2), ("c", 1)' test_textstats.py
}
solve_implement_from_test() {
  printf '\n\ndef top_words(text, n):\n    counts = {}\n    for w in text.lower().split():\n        counts[w] = counts.get(w, 0) + 1\n    return sorted(counts.items(), key=lambda kv: (-kv[1], kv[0]))[:n]\n' >> textstats.py
}



if [ "${BENCH_SELFTEST:-}" = 1 ]; then
  bad=0
  for task in "${TASKS[@]}"; do
    d="$(mktemp -d)"

    ( cd "$d" && "setup_$task" && git init -q . && git add -A &&
      git -c user.email=b@b -c user.name=b commit -qm init ) >/dev/null 2>&1
    if ( cd "$d" && "check_$task" ) 2>/dev/null; then echo "selftest: $task grader passes on the fixture" >&2; bad=1
    elif ! ( cd "$d" && "solve_$task" && "check_$task" ); then echo "selftest: $task grader rejects the reference fix" >&2; bad=1
    else echo "selftest: $task ok (fails before, passes after the reference fix)" >&2; fi
    rm -rf "$d"
  done
  exit $bad
fi


if [ -z "${ZEN_BIN:-}" ]; then
  for c in "${CARGO_TARGET_DIR:-$ROOT/target}/release/zen" "${CARGO_TARGET_DIR:-$ROOT/target}/debug/zen"; do
    [ -x "$c" ] && ZEN_BIN="$c" && break
  done
fi
[ -x "${ZEN_BIN:-}" ] || { echo "bench: build zen first (cargo build --release)" >&2; exit 2; }
case "$BASE" in
  *anymodel.org*)
    case "$MODEL" in
      am/free|am/nemotron-3-ultra-550b-a55b|am/nemotron-3-super-120b-a12b|am/gpt-oss-20b|am/laguna-xs-2.1) ;;
      *) echo "bench: refusing non-free AnyModel model '$MODEL'" >&2; exit 2 ;;
    esac ;;
esac

RUN_OUTS=()
for run in $(seq 1 "$RUNS"); do
if [ "$RUNS" -gt 1 ]; then RUN_OUT="${OUT%.json}-r$run.json"; else RUN_OUT="$OUT"; fi
RUN_OUTS+=("$RUN_OUT")
WORK="$(mktemp -d /tmp/zen-bench.XXXXXX)"
RESULTS="$WORK/results.jsonl"
: > "$RESULTS"
echo "bench: run $run/$RUNS model=$MODEL zen=$ZEN_BIN work=$WORK" >&2

first=1
for task in "${TASKS[@]}"; do
  [ $first = 1 ] || sleep "$PAUSE"
  first=0
  dir="$WORK/$task"
  mkdir -p "$dir" "$WORK/home"
  ( cd "$dir" && "setup_$task" && git init -q . && git add -A && git -c user.email=b@b -c user.name=b commit -qm init )
  mkdir -p "$dir/.zen"


  echo '{"verify": {"enabled": true}}' > "$dir/.zen/settings.json"
  prompt="$("prompt_$task")"
  start=$(date +%s)
  set +e
  ( cd "$dir" && HOME="$WORK/home" timeout "$TIMEOUT" "$ZEN_BIN" -p "$prompt" --output-format stream-json \
      --model "$MODEL" ${BASE:+--base-url "$BASE"} --permission-mode bypassPermissions \
      --max-turns "$MAX_TURNS" --setting-sources project --no-session-persistence ) \
      > "$WORK/$task.out.json" 2> "$WORK/$task.err"
  code=$?
  set -e
  secs=$(( $(date +%s) - start ))
  if ( cd "$dir" && "check_$task" ) 2>/dev/null; then pass=true; else pass=false; fi
  python3 - "$task" "$pass" "$code" "$secs" "$WORK/$task.out.json" "$WORK/$task.err" "$dir" >> "$RESULTS" <<'EOF'
import json, subprocess, sys
task, passed, code, secs, out_path, err_path, repo = sys.argv[1:]
out = open(out_path, encoding="utf-8", errors="replace").read().strip()
res, calls, errs, r429 = {}, 0, 0, 0
for line in out.splitlines():
    try:
        ev = json.loads(line)
    except Exception:
        continue
    t = ev.get("type")
    if t == "result":
        res = ev
    elif t == "assistant" and ev.get("parent_tool_use_id") is None:
        calls += sum(1 for b in (ev.get("message") or {}).get("content") or []
                     if isinstance(b, dict) and b.get("type") == "tool_use")
        calls += len((ev.get("message") or {}).get("tool_calls") or [])
    elif t == "user" and ev.get("parent_tool_use_id") is None:
        errs += sum(1 for b in (ev.get("message") or {}).get("content") or []
                    if isinstance(b, dict) and b.get("type") == "tool_result" and b.get("is_error"))
    elif t == "system" and ev.get("subtype") == "api_retry" and ev.get("error_status") == 429:
        r429 += 1
err_text = open(err_path, errors="replace").read()
tail = ((res.get("result") or "") + " " + err_text[-2000:]).lower()
infra = bool(res.get("is_error")) and any(k in tail for k in (
    "429", "rate limit", "network error", "connection refused", "error sending request",
    "502", "503", "504", "quota"))
diff = subprocess.run(["git", "-C", repo, "diff", "--stat"], capture_output=True, text=True).stdout.strip()
print(json.dumps({
    "task": task,
    "pass": passed == "true",
    "exit_code": int(code),
    "wall_seconds": int(secs),
    "num_turns": res.get("num_turns"),
    "tool_calls": calls,
    "tool_errors": errs,
    "retries_429": r429,
    "infra": infra,
    "subtype": res.get("subtype"),
    "is_error": res.get("is_error"),
    "usage": res.get("usage"),
    "result_excerpt": (res.get("result") or "")[:300],
    "stderr_excerpt": err_text[-300:],
    "diff_stat": diff.splitlines()[-1] if diff else "",
}))
EOF

  echo "bench-json: $(tail -n 1 "$RESULTS")" >&2
  echo "bench: $task pass=$pass exit=$code ${secs}s" >&2
done

mkdir -p "$(dirname "$RUN_OUT")"
python3 - "$RESULTS" "$RUN_OUT" "$MODEL" "${BASE:-default}" "$("$ZEN_BIN" --version 2>/dev/null || echo zen)" <<'EOF'
import json, sys, datetime
rows = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
calls = sum(r.get("tool_calls") or 0 for r in rows)
errs = sum(r.get("tool_errors") or 0 for r in rows)
summary = {
    "date": datetime.datetime.now().astimezone().isoformat(timespec="seconds"),
    "model": sys.argv[3],
    "base_url": sys.argv[4],
    "zen": sys.argv[5],
    "passed": sum(r["pass"] for r in rows),
    "total": len(rows),
    "tool_calls": calls,
    "tool_errors": errs,
    "tool_error_rate": round(errs / calls, 4) if calls else 0.0,
    "infra_failed": sum(1 for r in rows if r.get("infra") and not r["pass"]),
    "tasks": rows,
}
json.dump(summary, open(sys.argv[2], "w"), indent=2)
print(f"bench: {summary['passed']}/{summary['total']} passed -> {sys.argv[2]}")
EOF
done
[ "$RUNS" -gt 1 ] && summarize "${RUN_OUTS[@]}"
exit 0
