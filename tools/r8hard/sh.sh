


setup_sh_backup_spaces() {
  cat > backup.sh <<'EOF'
#!/usr/bin/env bash
# usage: backup.sh SRC DST
# Copy every regular file directly inside SRC (not subdirectories, not names starting with '.')
# to DST as NAME.bak, except names listed one per line (exact match) in SRC/.backupignore.
# DST is created if needed. Prints "backed up N files".
set -e
src=$1
dst=$2
mkdir -p $dst
n=0
for f in $(ls $src); do
  if [ -f $src/$f ] && ! grep -q "^$f$" $src/.backupignore 2>/dev/null; then
    cp $src/$f $dst/$f.bak
    n=$((n + 1))
  fi
done
echo "backed up $n files"
EOF
  chmod +x backup.sh
  cat > test.sh <<'EOF'
#!/usr/bin/env bash
# Smoke test for backup.sh.
set -u
t=$(mktemp -d)
mkdir -p "$t/src"
echo one > "$t/src/my notes.txt"
echo two > "$t/src/plain.txt"
out=$(bash ./backup.sh "$t/src" "$t/dst")
if [ "$out" = "backed up 2 files" ] && [ -f "$t/dst/my notes.txt.bak" ] && [ -f "$t/dst/plain.txt.bak" ]; then
  echo PASS
else
  echo "FAIL: got '$out'"; ls -la "$t/dst"; exit 1
fi
EOF
}
prompt_sh_backup_spaces() { echo "backup.sh breaks on unusual file names (bash test.sh fails). Make it work for any file name (spaces, leading dashes, glob characters, regex characters in .backupignore entries, a DST path with spaces) without changing what it does; the comment at the top of the script is the spec."; }
check_sh_backup_spaces() {
  local t
  t=$(mktemp -d)
  mkdir -p "$t/in dir/sub"
  for f in "my notes.txt" "-rf" "a*b" "plain" "aXtxt" "a.txt" ".hidden" "sub/inner"; do
    printf 'data:%s\n' "$f" > "$t/in dir/$f"
  done
  printf 'a.txt\nplain\n' > "$t/in dir/.backupignore"
  local out
  out=$(cd "$t" && bash "$OLDPWD/backup.sh" "in dir" "out dir/x" 2>/dev/null) || { rm -rf "$t"; return 1; }
  local got want r=0
  got=$(cd "$t/out dir/x" 2>/dev/null && find . -type f | sort | tr '\n' '|')
  want='./-rf.bak|./a*b.bak|./aXtxt.bak|./my notes.txt.bak|'
  [ "$out" = "backed up 4 files" ] && [ "$got" = "$want" ] &&
    [ "$(cat "$t/out dir/x/-rf.bak")" = "data:-rf" ] && [ "$(cat "$t/out dir/x/a*b.bak")" = "data:a*b" ] || r=1

  mkdir -p "$t/e" "$t/f"; echo x > "$t/f/only one"
  [ "$(bash backup.sh "$t/e" "$t/e2" 2>/dev/null)" = "backed up 0 files" ] || r=1
  [ "$(bash backup.sh "$t/f" "$t/f2" 2>/dev/null)" = "backed up 1 files" ] && [ -f "$t/f2/only one.bak" ] || r=1
  rm -rf "$t"
  return $r
}
solve_sh_backup_spaces() {
  cat > backup.sh <<'EOF'
#!/usr/bin/env bash
# usage: backup.sh SRC DST
# Copy every regular file directly inside SRC (not subdirectories, not names starting with '.')
# to DST as NAME.bak, except names listed one per line (exact match) in SRC/.backupignore.
# DST is created if needed. Prints "backed up N files".
set -e
src=$1
dst=$2
mkdir -p -- "$dst"
n=0
for path in "$src"/*; do
  [ -f "$path" ] || continue
  f=${path##*/}
  if [ -f "$src/.backupignore" ] && grep -qxF -- "$f" "$src/.backupignore"; then
    continue
  fi
  cp -- "$path" "$dst/$f.bak"
  n=$((n + 1))
done
echo "backed up $n files"
EOF
}


setup_sh_rotate_flags() {
  cat > rotate.sh <<'EOF'
#!/usr/bin/env bash
# usage: rotate.sh FILE
# Rotate FILE: FILE.4 -> FILE.5, ..., FILE.1 -> FILE.2, FILE -> FILE.1, keeping at most 5 old
# copies (FILE.5 is deleted first).
set -euo pipefail
keep=5
f=${1:-}
[ -n "$f" ] || { echo "usage: rotate.sh FILE" >&2; exit 2; }
[ -e "$f" ] || { echo "rotate: $f: no such file" >&2; exit 1; }
rm -f "$f.$keep"
for ((i = keep - 1; i >= 1; i--)); do
  if [ -e "$f.$i" ]; then
    mv "$f.$i" "$f.$((i + 1))"
  fi
done
mv "$f" "$f.1"
EOF
  chmod +x rotate.sh
}
prompt_sh_rotate_flags() { echo "Add options to rotate.sh (options come before FILE): -n N keeps N old copies instead of 5 (FILE.N is deleted first, like FILE.5 today); N must be a positive integer, otherwise print a usage message to stderr and exit 2. -d is a dry run: print every command it would run, one per line, exactly in the form 'rm -f FILE.N' and 'mv SRC DST' (paths as given on the command line, no quotes), in the order it would run them, and change nothing. Unknown options or a missing FILE: usage on stderr, exit 2. A FILE that does not exist still exits 1. Without options the behavior is unchanged."; }
check_sh_rotate_flags() {
  local t s r=0
  s="$PWD/rotate.sh"
  t=$(mktemp -d)
  (
    cd "$t" || exit 1

    echo 0 > log; for i in 1 2 3 4 5; do echo $i > log.$i; done
    bash "$s" log || exit 1
    [ ! -e log ] && [ "$(cat log.1)" = 0 ] && [ "$(cat log.5)" = 4 ] && [ ! -e log.6 ] || exit 1

    rm -f log*; echo a > log; echo b > log.1; echo c > log.2; echo z > log.3
    bash "$s" -n 2 log || exit 1
    [ "$(cat log.1)" = a ] && [ "$(cat log.2)" = b ] && [ ! -e log ] || exit 1

    rm -f log*; echo a > log; echo b > log.1; echo c > log.2
    out=$(bash "$s" -d -n 3 log) || exit 1
    [ "$out" = "$(printf 'rm -f log.3\nmv log.2 log.3\nmv log.1 log.2\nmv log log.1')" ] || exit 1
    [ "$(cat log)" = a ] && [ "$(cat log.2)" = c ] && [ ! -e log.3 ] || exit 1
    out=$(bash "$s" -n 1 -d log) || exit 1
    [ "$out" = "$(printf 'rm -f log.1\nmv log log.1')" ] || exit 1

    echo s > "my log"; bash "$s" -n 2 "my log" || exit 1
    [ "$(cat "my log.1")" = s ] || exit 1

    for bad in "-n 0 log" "-n -1 log" "-n x log" "-n 1.5 log" "-z log" "-n 2" ""; do

      bash "$s" $bad >/dev/null 2>"$t/err"; c=$?
      [ $c -eq 2 ] && [ -s "$t/err" ] || exit 1
    done
    [ "$(cat log)" = a ] || exit 1
    bash "$s" -n 2 missing 2>/dev/null; [ $? -eq 1 ] || exit 1
    bash "$s" -d missing >/dev/null 2>&1; [ $? -eq 1 ] || exit 1
  ) >/dev/null 2>&1 || r=1
  rm -rf "$t"
  return $r
}
solve_sh_rotate_flags() {
  cat > rotate.sh <<'EOF'
#!/usr/bin/env bash
# usage: rotate.sh [-n N] [-d] FILE
# Rotate FILE: FILE.(N-1) -> FILE.N, ..., FILE.1 -> FILE.2, FILE -> FILE.1, keeping at most N
# (default 5) old copies (FILE.N is deleted first). -d prints the commands instead.
set -euo pipefail
usage() { echo "usage: rotate.sh [-n N] [-d] FILE" >&2; exit 2; }
keep=5
dry=0
while getopts ":n:d" opt; do
  case $opt in
    n) [[ $OPTARG =~ ^[0-9]+$ ]] && [ "$((10#$OPTARG))" -ge 1 ] || usage; keep=$((10#$OPTARG)) ;;
    d) dry=1 ;;
    *) usage ;;
  esac
done
shift $((OPTIND - 1))
[ $# -eq 1 ] || usage
f=$1
[ -e "$f" ] || { echo "rotate: $f: no such file" >&2; exit 1; }
run() { if [ $dry = 1 ]; then echo "$*"; else "$@"; fi; }
run rm -f "$f.$keep"
for ((i = keep - 1; i >= 1; i--)); do
  if [ -e "$f.$i" ]; then
    run mv "$f.$i" "$f.$((i + 1))"
  fi
done
run mv "$f" "$f.1"
EOF
}


setup_sh_csv_sum() {
  cat > sum.sh <<'EOF'
#!/usr/bin/env bash
# usage: sum.sh FILE COLUMN
# Print the sum of the named column of a CSV file (first line is the header), 2 decimals.
file=$1
col=$2
idx=$(head -1 "$file" | tr ',' '\n' | grep -n "$col" | head -1 | cut -d: -f1)
total=0
tail -n +2 "$file" | while IFS=, read -r -a f; do
  total=$(echo "$total + ${f[$((idx - 1))]}" | bc)
done
printf '%.2f\n' "$total"
EOF
  chmod +x sum.sh
  cat > test.sh <<'EOF'
#!/usr/bin/env bash
t=$(mktemp)
printf 'region,amount\nnorth,1.5\nsouth,2.25\n' > "$t"
got=$(bash ./sum.sh "$t" amount)
[ "$got" = "3.75" ] && echo PASS || { echo "FAIL: got '$got', want 3.75"; exit 1; }
EOF
}
prompt_sh_csv_sum() { echo "sum.sh prints wrong totals (bash test.sh fails). Fix it so that: the column is chosen by exact header name; empty cells count as 0; files may have Windows (CRLF) line endings and may lack a final newline; an unknown column prints 'sum.sh: no column NAME' to stderr and exits 1. Output stays the sum with 2 decimals. Values are plain numbers (no quoted fields)."; }
check_sh_csv_sum() {
  local t r=0
  t=$(mktemp -d)
  printf 'id,amount_tax,amount\n1,9,1.5\n2,9,2.25\n3,9,-0.75' > "$t/a.csv"
  printf 'id,amount\r\n1,0.1\r\n2,0.2\r\n3,\r\n4,0.7\r\n' > "$t/b.csv"
  printf 'x,amount\n' > "$t/c.csv"
  printf 'amount,y\n10,1\n20,2\n' > "$t/d.csv"
  [ "$(bash sum.sh "$t/a.csv" amount 2>/dev/null)" = "3.00" ] || r=1
  [ "$(bash sum.sh "$t/a.csv" amount_tax 2>/dev/null)" = "27.00" ] || r=1
  [ "$(bash sum.sh "$t/b.csv" amount 2>/dev/null)" = "1.00" ] || r=1
  [ "$(bash sum.sh "$t/c.csv" amount 2>/dev/null)" = "0.00" ] || r=1
  [ "$(bash sum.sh "$t/d.csv" amount 2>/dev/null)" = "30.00" ] || r=1
  [ "$(bash sum.sh "$t/d.csv" y 2>/dev/null)" = "3.00" ] || r=1
  bash sum.sh "$t/a.csv" amt >/dev/null 2>"$t/err"
  [ $? -eq 1 ] && grep -q "no column amt" "$t/err" || r=1
  bash sum.sh "$t/b.csv" id >/dev/null 2>&1 || r=1
  rm -rf "$t"
  return $r
}
solve_sh_csv_sum() {
  cat > sum.sh <<'EOF'
#!/usr/bin/env bash
# usage: sum.sh FILE COLUMN
# Print the sum of the named column of a CSV file (first line is the header), 2 decimals.
file=$1
col=$2
awk -F, -v col="$col" '
  { sub(/\r$/, "") }
  NR == 1 { for (i = 1; i <= NF; i++) if ($i == col) idx = i
            if (!idx) { print "sum.sh: no column " col > "/dev/stderr"; bad = 1; exit 1 }
            next }
  { total += $idx }
  END { if (!bad) printf "%.2f\n", total }
' "$file"
EOF
}
