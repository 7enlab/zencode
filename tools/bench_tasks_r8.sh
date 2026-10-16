# zen mini-bench, r8 task set (sourced by tools/bench.sh). Harder than the r7 set: the hidden
# graders check more cases than the visible tests, the way SWE-bench grades with FAIL_TO_PASS
# tests the agent never sees (https://www.swebench.com/SWE-bench/guides/evaluation/).
# Each task: setup_<t> writes the repo, prompt_<t> is the task text, check_<t> the hidden grader,
# solve_<t> a reference fix used only by BENCH_SELFTEST.

setup_paginate_bug() {
  mkdir -p shop
  touch shop/__init__.py
  cat > shop/paging.py <<'EOF'
def page_bounds(page, per_page):
    """Return (start, end) slice indexes for a 1-based page number."""
    start = page * per_page
    return start, start + per_page
EOF
  cat > shop/catalog.py <<'EOF'
from shop.paging import page_bounds

ITEMS = ["item%02d" % i for i in range(1, 24)]


def list_items(page=1, per_page=10):
    start, end = page_bounds(page, per_page)
    return ITEMS[start:end]


def page_count(per_page=10):
    return len(ITEMS) // per_page
EOF
  cat > test_catalog.py <<'EOF'
import unittest
from shop.catalog import list_items


class CatalogTest(unittest.TestCase):
    def test_first_page_starts_at_first_item(self):
        self.assertEqual(list_items(1, 10)[0], "item01")


if __name__ == "__main__":
    unittest.main()
EOF
}
prompt_paginate_bug() { echo "Users report that page 1 of the catalog skips the first items and the last page is never reachable (see test_catalog.py). Find the root cause and fix pagination so every item appears on exactly one page and page_count() is right."; }
check_paginate_bug() {
  python3 - <<'EOF' 2>/dev/null
from shop.catalog import list_items, page_count, ITEMS
assert list_items(1, 10) == ITEMS[0:10]
assert list_items(3, 10) == ITEMS[20:23]
assert page_count(10) == 3 and page_count(23) == 1 and page_count(5) == 5
seen = [x for p in range(1, page_count(7) + 1) for x in list_items(p, 7)]
assert seen == ITEMS
EOF
}
solve_paginate_bug() {
  sed -i 's/start = page \* per_page/start = (page - 1) * per_page/' shop/paging.py
  sed -i 's|return len(ITEMS) // per_page|return -(-len(ITEMS) // per_page)|' shop/catalog.py
}

setup_dotenv_parser() {
  cat > README.md <<'EOF'
# envfile

`parse(text)` turns the text of a `.env` file into a dict. Rules:

1. Blank lines and lines whose first non-space character is `#` are ignored.
2. A line is `KEY=VALUE`; an optional leading `export ` is ignored. Whitespace around KEY and
   around VALUE is stripped.
3. A VALUE wrapped in double quotes keeps inner spaces and `#`, and `\n` inside it becomes a
   newline. A VALUE wrapped in single quotes is taken literally (no escapes).
4. In an unquoted VALUE, ` #` (space then hash) starts a comment that is dropped.
5. A line without `=` raises `ValueError` mentioning its 1-based line number.
6. A later KEY overrides an earlier one.
EOF
  cat > envfile.py <<'EOF'
def parse(text):
    raise NotImplementedError
EOF
  cat > test_envfile.py <<'EOF'
import unittest
from envfile import parse


class EnvTest(unittest.TestCase):
    def test_basic(self):
        self.assertEqual(parse("A=1\nB = two\n"), {"A": "1", "B": "two"})


if __name__ == "__main__":
    unittest.main()
EOF
}
prompt_dotenv_parser() { echo "Implement parse() in envfile.py so it follows every rule in README.md. test_envfile.py only covers the basics; make the whole spec work."; }
check_dotenv_parser() {
  python3 - <<'EOF' 2>/dev/null
from envfile import parse
assert parse("A=1\nB = two\n") == {"A": "1", "B": "two"}
assert parse("# c\n\n   # d\nexport X=1\n") == {"X": "1"}
assert parse('Q="a b # c"\n') == {"Q": "a b # c"}
assert parse('N="l1\\nl2"\n') == {"N": "l1\nl2"}
assert parse("S='a\\nb #x'\n") == {"S": "a\\nb #x"}
assert parse("U=val # note\nV=a#b\n") == {"U": "val", "V": "a#b"}
assert parse("K=1\nK=2\n") == {"K": "2"}
assert parse("E=\n") == {"E": ""}
try:
    parse("A=1\n\nbroken\n")
    raise SystemExit(1)
except ValueError as e:
    assert "3" in str(e), str(e)
EOF
}
solve_dotenv_parser() {
  cat > envfile.py <<'EOF'
def parse(text):
    out = {}
    for n, raw in enumerate(text.splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("export "):
            line = line[len("export "):]
        if "=" not in line:
            raise ValueError("line %d: missing '='" % n)
        key, val = line.split("=", 1)
        val = val.strip()
        if len(val) >= 2 and val[0] == val[-1] == '"':
            val = val[1:-1].replace("\\n", "\n")
        elif len(val) >= 2 and val[0] == val[-1] == "'":
            val = val[1:-1]
        elif " #" in val:
            val = val.split(" #", 1)[0].rstrip()
        out[key.strip()] = val
    return out
EOF
}

setup_js_async_fix() {
  cat > package.json <<'EOF'
{"name": "fetcher", "version": "1.0.0", "type": "commonjs", "scripts": {"test": "node --test"}}
EOF
  cat > fetcher.js <<'EOF'
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function fetchOne(id) {
  await sleep(5 * (5 - id));
  if (id < 0) throw new Error("bad id " + id);
  return { id, name: "n" + id };
}

// Fetch every id and return the results in the same order as `ids`.
async function fetchAll(ids) {
  const results = [];
  ids.forEach(async (id) => {
    results.push(await fetchOne(id));
  });
  return results;
}

module.exports = { fetchAll, fetchOne };
EOF
  cat > fetcher.test.js <<'EOF'
const test = require("node:test");
const assert = require("node:assert");
const { fetchAll } = require("./fetcher");

test("fetchAll returns every result", async () => {
  const r = await fetchAll([1, 2, 3]);
  assert.strictEqual(r.length, 3);
});
EOF
}
prompt_js_async_fix() { echo "npm test fails: fetchAll() in fetcher.js returns too early. Fix it so it returns all results in input order and rejects if any fetch fails."; }
check_js_async_fix() {
  node -e '
const { fetchAll } = require("./fetcher");
(async () => {
  const r = await fetchAll([1, 2, 3, 4]);
  if (JSON.stringify(r.map((x) => x.id)) !== "[1,2,3,4]") process.exit(1);
  let threw = false;
  try { await fetchAll([1, -1, 2]); } catch (e) { threw = /bad id -1/.test(e.message); }
  process.exit(threw ? 0 : 1);
})().catch(() => process.exit(1));' 2>/dev/null
}
solve_js_async_fix() {
  python3 - <<'EOF'
s = open("fetcher.js").read()
old = "  const results = [];\n  ids.forEach(async (id) => {\n    results.push(await fetchOne(id));\n  });\n  return results;"
s = s.replace(old, "  return Promise.all(ids.map(fetchOne));")
open("fetcher.js", "w").write(s)
EOF
}

setup_big_file_edit() {
  python3 - <<'EOF'
tiers = ["bronze", "silver", "gold", "platinum", "diamond", "staff", "partner", "student",
         "senior", "veteran", "teacher", "nurse", "founder", "beta", "legacy"]
out = ['"""Discount rules per customer tier. Generated; edit by hand with care."""', ""]
for i, t in enumerate(tiers):
    out += [
        "",
        "def discount_for_%s(total):" % t,
        '    """Discount in percent for a %s customer."""' % t,
        "    if total >= %d:" % (100 + 10 * i),
        "        return %d" % (5 + i % 4),
        "    if total >= 50:",
        "        return 2",
        "    return 0",
    ]
open("discounts.py", "w").write("\n".join(out) + "\n")
EOF
}
prompt_big_file_edit() { echo "In discounts.py, change only the gold tier: gold customers get 12 percent at a total of 100 or more (instead of the current rule), and 4 percent from 50. Leave every other tier exactly as it is."; }
check_big_file_edit() {
  python3 - <<'EOF' 2>/dev/null
import re, subprocess
import discounts as d
assert d.discount_for_gold(100) == 12 and d.discount_for_gold(119) == 12 and d.discount_for_gold(500) == 12
assert d.discount_for_gold(50) == 4 and d.discount_for_gold(99) == 4 and d.discount_for_gold(10) == 0
orig = subprocess.run(["git", "show", "HEAD:discounts.py"], capture_output=True, text=True).stdout
if not orig:
    orig = open(".orig_discounts.py").read()
now = open("discounts.py").read()
def bodies(src):
    return {m.group(1): m.group(0) for m in re.finditer(r"def discount_for_(\w+)\(total\):.*?return 0\n", src, re.S)}
a, b = bodies(orig), bodies(now)
assert set(a) == set(b)
assert all(a[k] == b[k] for k in a if k != "gold")
EOF
}
solve_big_file_edit() {
  cp discounts.py .orig_discounts.py
  python3 - <<'EOF'
s = open("discounts.py").read()
s = s.replace("    if total >= 120:\n        return 7\n    if total >= 50:\n        return 2",
              "    if total >= 100:\n        return 12\n    if total >= 50:\n        return 4", 1)
open("discounts.py", "w").write(s)
EOF
}

setup_report_json() {
  mkdir -p report
  touch report/__init__.py
  cat > report/data.py <<'EOF'
SALES = [
    {"region": "north", "month": "2026-01", "amount": 120.5},
    {"region": "south", "month": "2026-01", "amount": 80.0},
    {"region": "north", "month": "2026-02", "amount": 99.5},
    {"region": "east", "month": "2026-02", "amount": 10.0},
]
EOF
  cat > report/summary.py <<'EOF'
from report.data import SALES


def totals_by_region(sales=SALES):
    out = {}
    for row in sales:
        out[row["region"]] = out.get(row["region"], 0) + row["amount"]
    return out
EOF
  cat > report/cli.py <<'EOF'
import argparse
from report.summary import totals_by_region


def render_text(totals):
    return "\n".join("%-6s %8.2f" % (k, v) for k, v in sorted(totals.items()))


def main(argv=None):
    p = argparse.ArgumentParser(prog="report")
    sub = p.add_subparsers(dest="cmd", required=True)
    sub.add_parser("totals", help="total sales per region")
    args = p.parse_args(argv)
    if args.cmd == "totals":
        print(render_text(totals_by_region()))


if __name__ == "__main__":
    main()
EOF
}
prompt_report_json() { echo "Add a --format option to the 'totals' subcommand of report/cli.py (run as: python3 -m report.cli totals). --format text keeps today's output and is the default; --format json prints a JSON object mapping region to total, keys sorted, amounts rounded to 2 decimals. Any other value must be rejected by argparse with exit code 2."; }
check_report_json() {
  [ "$(python3 -m report.cli totals 2>/dev/null)" = "$(printf 'east      10.00\nnorth    220.00\nsouth     80.00')" ] &&
  [ "$(python3 -m report.cli totals --format text 2>/dev/null)" = "$(python3 -m report.cli totals)" ] &&
  python3 -c '
import json, subprocess, sys
o = subprocess.run([sys.executable, "-m", "report.cli", "totals", "--format", "json"], capture_output=True, text=True).stdout
d = json.loads(o)
assert d == {"east": 10.0, "north": 220.0, "south": 80.0}, d
assert list(d) == sorted(d)
' 2>/dev/null && { python3 -m report.cli totals --format xml >/dev/null 2>&1; [ $? -eq 2 ]; }
}
solve_report_json() {
  python3 - <<'EOF'
s = open("report/cli.py").read()
s = s.replace('    sub.add_parser("totals", help="total sales per region")',
              '    t = sub.add_parser("totals", help="total sales per region")\n'
              '    t.add_argument("--format", choices=["text", "json"], default="text")')
s = s.replace("        print(render_text(totals_by_region()))",
              "        totals = totals_by_region()\n"
              "        if args.format == \"json\":\n"
              "            import json\n"
              "            print(json.dumps({k: round(v, 2) for k, v in sorted(totals.items())}))\n"
              "        else:\n"
              "            print(render_text(totals))")
open("report/cli.py", "w").write(s)
EOF
}
