# r8-hard bench tasks, Python part (sourced by tools/bench_tasks_r8hard.sh).
# setup_<t> writes the repo, prompt_<t> is all the model sees, check_<t> is the hidden grader
# (run in the repo after the agent; exit 0 = pass), solve_<t> a reference fix for BENCH_SELFTEST.

# ── py_kwarg_rename: rename a keyword argument across modules with a deprecation shim ──────────
setup_py_kwarg_rename() {
  mkdir -p netlib
  touch netlib/__init__.py
  cat > netlib/transport.py <<'EOF'
def send(url, tmo):
    """Pretend network call: slow hosts need a timeout of at least 10 s."""
    if tmo <= 0:
        raise ValueError("tmo must be positive")
    if "slow" in url and tmo < 10:
        raise TimeoutError(url)
    return "OK " + url
EOF
  cat > netlib/client.py <<'EOF'
from netlib.transport import send


def fetch(url, tmo=5, retries=0):
    """Fetch url, retrying on timeout. tmo is the timeout in seconds."""
    last = None
    for _ in range(retries + 1):
        try:
            return send(url, tmo)
        except TimeoutError as e:
            last = e
    raise last
EOF
  cat > netlib/batch.py <<'EOF'
from netlib.client import fetch


def fetch_all(urls, tmo=5, retries=0):
    """Fetch every url in order."""
    return [fetch(u, tmo=tmo, retries=retries) for u in urls]
EOF
  cat > app.py <<'EOF'
from netlib.client import fetch
from netlib.batch import fetch_all

print(fetch("slow.example", tmo=12))
print(fetch_all(["a.example", "b.example"], tmo=3))
EOF
}
prompt_py_kwarg_rename() { echo "Rename the keyword argument 'tmo' to 'timeout' in netlib: fetch(), fetch_all() and transport.send(), and update every call site in the repo to the new name. For one release, fetch() and fetch_all() must still accept tmo= (it works the same but emits a DeprecationWarning); passing both tmo and timeout raises TypeError. Positional calls keep working and the defaults stay the same."; }
check_py_kwarg_rename() {
  python3 - <<'EOF' 2>/dev/null
import inspect, subprocess, sys, warnings
from netlib.client import fetch
from netlib.batch import fetch_all
from netlib import transport
assert fetch("a", timeout=3) == "OK a"
assert fetch("a", 3) == "OK a"
assert fetch("slow", timeout=12) == "OK slow"
try:
    fetch("slow")
    raise SystemExit(1)
except TimeoutError:
    pass
assert fetch_all(["a", "b"], timeout=3) == ["OK a", "OK b"]
for call, want in ((lambda: fetch("a", tmo=3), "OK a"), (lambda: fetch_all(["a"], tmo=3), ["OK a"])):
    with warnings.catch_warnings(record=True) as w:
        warnings.simplefilter("always")
        assert call() == want
    assert any(issubclass(x.category, DeprecationWarning) for x in w)
for call in (lambda: fetch("a", tmo=1, timeout=1), lambda: fetch_all(["a"], tmo=1, timeout=1)):
    try:
        call()
        raise SystemExit(1)
    except TypeError:
        pass
for f in (fetch, fetch_all, transport.send):
    assert "timeout" in inspect.signature(f).parameters, f
out = subprocess.run([sys.executable, "-W", "error::DeprecationWarning", "app.py"], capture_output=True, text=True)
assert out.returncode == 0 and out.stdout == "OK slow.example\n['OK a.example', 'OK b.example']\n", out
EOF
}
solve_py_kwarg_rename() {
  sed -i 's/tmo/timeout/g' netlib/transport.py
  cat > netlib/client.py <<'EOF'
import warnings

from netlib.transport import send


def _resolve(timeout, tmo, default=5):
    if tmo is not None:
        if timeout is not None:
            raise TypeError("pass timeout or tmo, not both")
        warnings.warn("tmo is deprecated; use timeout", DeprecationWarning, stacklevel=3)
        return tmo
    return default if timeout is None else timeout


def fetch(url, timeout=None, retries=0, tmo=None):
    """Fetch url, retrying on timeout. timeout is in seconds (default 5)."""
    timeout = _resolve(timeout, tmo)
    last = None
    for _ in range(retries + 1):
        try:
            return send(url, timeout)
        except TimeoutError as e:
            last = e
    raise last
EOF
  cat > netlib/batch.py <<'EOF'
from netlib.client import _resolve, fetch


def fetch_all(urls, timeout=None, retries=0, tmo=None):
    """Fetch every url in order."""
    timeout = _resolve(timeout, tmo)
    return [fetch(u, timeout=timeout, retries=retries) for u in urls]
EOF
  sed -i 's/tmo=/timeout=/g' app.py
}

# ── py_lru_bug: bug fix that needs reading the test ──────────────────────────────────────────
setup_py_lru_bug() {
  cat > lru.py <<'EOF'
from collections import OrderedDict


class LRUCache:
    """Fixed-size cache that evicts the least recently used key.

    Both get() of an existing key and put() count as a use.
    """

    def __init__(self, capacity):
        if capacity < 1:
            raise ValueError("capacity must be >= 1")
        self.capacity = capacity
        self._d = OrderedDict()
        self.hits = 0
        self.misses = 0

    def get(self, key, default=None):
        if key in self._d:
            self.hits += 1
            return self._d[key]
        self.misses += 1
        return default

    def put(self, key, value):
        self._d[key] = value
        if len(self._d) > self.capacity:
            self._d.popitem(last=True)

    def __len__(self):
        return len(self._d)
EOF
  cat > test_lru.py <<'EOF'
import unittest
from lru import LRUCache


class LRUTest(unittest.TestCase):
    def test_evicts_least_recently_used(self):
        c = LRUCache(2)
        c.put("a", 1)
        c.put("b", 2)
        self.assertEqual(c.get("a"), 1)
        c.put("c", 3)
        self.assertIsNone(c.get("b"))
        self.assertEqual(c.get("a"), 1)


if __name__ == "__main__":
    unittest.main()
EOF
}
prompt_py_lru_bug() { echo "test_lru.py fails (run: python3 -m unittest). Fix LRUCache in lru.py so it behaves as its docstring says. Do not change the test."; }
check_py_lru_bug() {
  git diff --quiet HEAD -- test_lru.py && python3 - <<'EOF' 2>/dev/null
from lru import LRUCache
c = LRUCache(2); c.put("a", 1); c.put("b", 2); assert c.get("a") == 1; c.put("c", 3)
assert c.get("b") is None and c.get("a") == 1 and c.get("c") == 3
c = LRUCache(2); c.put("a", 1); c.put("b", 2); c.put("a", 10); c.put("c", 3)
assert c.get("a") == 10 and c.get("b", "x") == "x"
c = LRUCache(1); c.put("x", 1); c.put("y", 2)
assert len(c) == 1 and c.get("y") == 2 and c.get("x") is None
c = LRUCache(3); c.put(1, None); assert c.get(1, "d") is None
c = LRUCache(2); c.put("a", 1); c.get("a"); c.get("zz"); assert (c.hits, c.misses) == (1, 1)
c = LRUCache(3)
for k in "abc":
    c.put(k, k)
c.get("a"); c.get("b"); c.put("d", "d")
assert c.get("c") is None and len(c) == 3
try:
    LRUCache(0)
    raise SystemExit(1)
except ValueError:
    pass
EOF
}
solve_py_lru_bug() {
  python3 - <<'EOF'
s = open("lru.py").read()
s = s.replace("            self.hits += 1\n            return self._d[key]",
              "            self.hits += 1\n            self._d.move_to_end(key)\n            return self._d[key]")
s = s.replace("        self._d[key] = value\n", "        self._d[key] = value\n        self._d.move_to_end(key)\n")
s = s.replace("popitem(last=True)", "popitem(last=False)")
open("lru.py", "w").write(s)
EOF
}

# ── py_long_file: one precise edit in a ~700-line file of near-identical functions ─────────────
setup_py_long_file() {
  python3 - <<'EOF'
out = ['"""Frame codecs for device protocol versions 1-40: one decoder/encoder pair per version."""']
for v in range(1, 41):
    enc = "little" if v == 7 else "big"
    dec = "little" if v in (3, 7) else "big"
    note = "    # v7 devices are legacy little-endian hardware: do not change.\n" if v == 7 else ""
    out.append(f'''

def decode_frame_v{v}(buf):
    """Return the payload of a v{v} frame: header byte 0x{v:02x}, 2-byte length, payload."""
{note}    if len(buf) < 3 or buf[0] != 0x{v:02x}:
        raise ValueError("not a v{v} frame")
    length = int.from_bytes(buf[1:3], "{dec}")
    if len(buf) < 3 + length:
        raise ValueError("short frame")
    return bytes(buf[3:3 + length])


def encode_frame_v{v}(payload):
    """Build a v{v} frame around payload."""
    if len(payload) > 0xFFFF:
        raise ValueError("payload too long")
    return bytes([0x{v:02x}]) + len(payload).to_bytes(2, "{enc}") + bytes(payload)''')
open("protocol.py", "w").write("\n".join(out) + "\n")
EOF
  cat > test_protocol.py <<'EOF'
import unittest
import protocol as p


class RoundTrip(unittest.TestCase):
    def test_roundtrip(self):
        for v in (1, 3, 7, 13, 30, 33):
            enc = getattr(p, "encode_frame_v%d" % v)
            dec = getattr(p, "decode_frame_v%d" % v)
            self.assertEqual(dec(enc(b"hello")), b"hello", "v%d" % v)


if __name__ == "__main__":
    unittest.main()
EOF
}
prompt_py_long_file() { echo "Frames from v3 devices decode with the wrong payload length (python3 -m unittest shows a failing round trip). Fix it in protocol.py. Change nothing else in that file: every other function must stay byte-for-byte the same."; }
check_py_long_file() {
  python3 - <<'EOF' 2>/dev/null
import ast, subprocess
import protocol as p
assert p.decode_frame_v3(bytes([3, 0, 5]) + b"abcdeXYZ") == b"abcde"
big = bytes(range(256)) * 2
assert p.decode_frame_v3(bytes([3, 1, 2]) + big) == big[:258]
assert p.decode_frame_v3(p.encode_frame_v3(b"")) == b""
try:
    p.decode_frame_v3(bytes([3, 0, 9]) + b"abc")
    raise SystemExit(1)
except ValueError:
    pass
assert p.decode_frame_v7(bytes([7, 5, 0]) + b"abcde") == b"abcde"
orig = subprocess.run(["git", "show", "HEAD:protocol.py"], capture_output=True, text=True, check=True).stdout
now = open("protocol.py").read()
def funcs(src):
    t = ast.parse(src)
    return {n.name: ast.get_source_segment(src, n) for n in t.body if isinstance(n, ast.FunctionDef)}
a, b = funcs(orig), funcs(now)
assert set(a) == set(b)
assert all(a[k] == b[k] for k in a if k != "decode_frame_v3")
assert len(now.splitlines()) == len(orig.splitlines())
EOF
}
solve_py_long_file() {
  python3 - <<'EOF'
s = open("protocol.py").read()
i = s.index("def decode_frame_v3(")
j = s.index("def encode_frame_v3(")
s = s[:i] + s[i:j].replace('"little"', '"big"') + s[j:]
open("protocol.py", "w").write(s)
EOF
}

# ── py_distractor: fix the live package, leave the frozen legacy copy alone ─────────────────────
setup_py_distractor() {
  mkdir -p src/billing legacy/billing_old tests docs
  touch src/billing/__init__.py legacy/billing_old/__init__.py
  cat > src/billing/tax.py <<'EOF'
def tax_for(amount, rate):
    """Tax on amount (currency units) at rate (0.1 = 10%), rounded half up to the cent."""
    return round(amount * rate, 2)
EOF
  cat > src/billing/invoice.py <<'EOF'
from billing.tax import tax_for


def invoice_total(lines, rate):
    """lines: list of (unit_price, qty). Tax is computed and rounded per line.

    Returns the total as a float rounded to the cent.
    """
    total = 0.0
    for price, qty in lines:
        sub = price * qty
        total += sub + tax_for(sub, rate)
    return round(total, 2)
EOF
  cp src/billing/tax.py legacy/billing_old/tax.py
  sed 's/from billing.tax/from billing_old.tax/' src/billing/invoice.py > legacy/billing_old/invoice.py
  cat > docs/billing.md <<'EOF'
# Billing

The app imports `billing` from `src/`. `legacy/billing_old` is a frozen copy of the 2024 code
kept so old invoices can be re-printed exactly as issued; auditors diff it, so it never changes.
EOF
  cat > tests/test_billing.py <<'EOF'
import sys
import unittest

sys.path.insert(0, "src")
from billing.invoice import invoice_total  # noqa: E402
from billing.tax import tax_for  # noqa: E402


class BillingTest(unittest.TestCase):
    def test_half_cent_rounds_up(self):
        self.assertEqual(tax_for(0.125, 1.0), 0.13)

    def test_invoice(self):
        self.assertEqual(invoice_total([(10.0, 2)], 0.1), 22.0)


if __name__ == "__main__":
    unittest.main()
EOF
}
prompt_py_distractor() { echo "Customers report invoice totals that are off by a cent (python3 -m unittest discover tests shows a failing case). Tax must be rounded half up to the cent, per line. Fix the billing package the app uses (src/billing). See docs/billing.md before touching anything else."; }
check_py_distractor() {
  git diff --quiet HEAD -- legacy tests && [ -z "$(git status --porcelain legacy)" ] && python3 - <<'EOF' 2>/dev/null
import sys
sys.path.insert(0, "src")
from billing.tax import tax_for
from billing.invoice import invoice_total
cases = {(10.05, 0.5): 5.03, (0.125, 1.0): 0.13, (2.675, 1.0): 2.68, (1.005, 1.0): 1.01,
         (10.0, 0.1): 1.0, (19.99, 0.0825): 1.65, (0.0, 0.2): 0.0, (3.3, 0.15): 0.5}
for (a, r), want in cases.items():
    got = tax_for(a, r)
    assert abs(got - want) < 1e-9, (a, r, got, want)
assert abs(invoice_total([(10.05, 1), (0.1, 3)], 0.5) - 15.53) < 1e-9
assert abs(invoice_total([(2.675, 1), (1.005, 1)], 1.0) - 7.37) < 1e-9
assert abs(invoice_total([(10.0, 2)], 0.1) - 22.0) < 1e-9
assert abs(invoice_total([], 0.1)) < 1e-9
EOF
}
solve_py_distractor() {
  cat > src/billing/tax.py <<'EOF'
from decimal import ROUND_HALF_UP, Decimal


def tax_for(amount, rate):
    """Tax on amount (currency units) at rate (0.1 = 10%), rounded half up to the cent."""
    d = Decimal(repr(amount)) * Decimal(repr(rate))
    return float(d.quantize(Decimal("0.01"), rounding=ROUND_HALF_UP))
EOF
  cat > src/billing/invoice.py <<'EOF'
from decimal import Decimal

from billing.tax import tax_for


def invoice_total(lines, rate):
    """lines: list of (unit_price, qty). Tax is computed and rounded per line.

    Returns the total as a float rounded to the cent.
    """
    total = Decimal(0)
    for price, qty in lines:
        sub = Decimal(repr(price)) * qty
        total += sub + Decimal(repr(tax_for(float(sub), rate)))
    return float(total.quantize(Decimal("0.01")))
EOF
}

# ── py_cli_feature: new subcommand with validation and exit codes ─────────────────────────────
setup_py_cli_feature() {
  mkdir -p inventory
  touch inventory/__init__.py
  cat > inventory/store.py <<'EOF'
import json
import os


def path():
    return os.environ.get("INVENTORY_FILE", "inventory.json")


def load():
    try:
        with open(path()) as f:
            return json.load(f)
    except FileNotFoundError:
        return {}


def save(items):
    with open(path(), "w") as f:
        json.dump(items, f, indent=2, sort_keys=True)
EOF
  cat > inventory/cli.py <<'EOF'
import argparse
import sys

from inventory import store


def positive_int(text):
    try:
        n = int(text)
    except ValueError:
        raise argparse.ArgumentTypeError("not an integer: %r" % text)
    if n < 1:
        raise argparse.ArgumentTypeError("must be positive: %r" % text)
    return n


def cmd_add(args):
    items = store.load()
    items[args.name] = items.get(args.name, 0) + args.qty
    store.save(items)
    print("%s: %d" % (args.name, items[args.name]))
    return 0


def cmd_list(args):
    for name, qty in sorted(store.load().items()):
        print("%s: %d" % (name, qty))
    return 0


def main(argv=None):
    p = argparse.ArgumentParser(prog="inventory")
    sub = p.add_subparsers(dest="cmd", required=True)
    a = sub.add_parser("add", help="add stock")
    a.add_argument("name")
    a.add_argument("qty", type=positive_int)
    a.set_defaults(func=cmd_add)
    ls = sub.add_parser("list", help="show stock")
    ls.set_defaults(func=cmd_list)
    args = p.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
EOF
}
prompt_py_cli_feature() { echo "Add a 'remove NAME [QTY]' subcommand to the inventory CLI (run as: python3 -m inventory.cli). Without QTY it deletes the item; with QTY it subtracts, and an item that reaches 0 is deleted. On success print 'NAME: <new qty>' or 'NAME: removed' when the item is gone. Removing an unknown item or more than is in stock prints an error naming the item on stderr, exits 1 and leaves the data file untouched. QTY must be a positive integer, validated the same way as for add (argparse error, exit 2)."; }
check_py_cli_feature() {
  python3 - <<'EOF' 2>/dev/null
import json, os, subprocess, sys, tempfile
d = tempfile.mkdtemp()
env = dict(os.environ, INVENTORY_FILE=os.path.join(d, "inv.json"))
def run(*a):
    return subprocess.run([sys.executable, "-m", "inventory.cli", *a], capture_output=True, text=True, env=env)
def data():
    return json.load(open(env["INVENTORY_FILE"]))
assert run("add", "apple", "5").returncode == 0
assert run("add", "pear", "2").returncode == 0
r = run("remove", "apple", "2"); assert r.returncode == 0 and r.stdout.strip() == "apple: 3", r
assert data() == {"apple": 3, "pear": 2}
r = run("remove", "apple", "3"); assert r.returncode == 0 and r.stdout.strip() == "apple: removed", r
assert data() == {"pear": 2}
before = open(env["INVENTORY_FILE"]).read()
r = run("remove", "pear", "5"); assert r.returncode == 1 and "pear" in r.stderr, r
r = run("remove", "kiwi"); assert r.returncode == 1 and "kiwi" in r.stderr, r
assert open(env["INVENTORY_FILE"]).read() == before
for bad in ("0", "-1", "x"):
    r = run("remove", "pear", bad); assert r.returncode == 2, (bad, r)
assert open(env["INVENTORY_FILE"]).read() == before
r = run("remove", "pear"); assert r.returncode == 0 and r.stdout.strip() == "pear: removed", r
assert data() == {}
r = run("list"); assert r.returncode == 0 and r.stdout == ""
EOF
}
solve_py_cli_feature() {
  python3 - <<'EOF'
s = open("inventory/cli.py").read()
s = s.replace("def main(argv=None):", '''def cmd_remove(args):
    items = store.load()
    have = items.get(args.name)
    if have is None:
        print("inventory: no such item: %s" % args.name, file=sys.stderr)
        return 1
    take = have if args.qty is None else args.qty
    if take > have:
        print("inventory: only %d of %s in stock" % (have, args.name), file=sys.stderr)
        return 1
    left = have - take
    if left:
        items[args.name] = left
        msg = "%s: %d" % (args.name, left)
    else:
        del items[args.name]
        msg = "%s: removed" % args.name
    store.save(items)
    print(msg)
    return 0


def main(argv=None):''')
s = s.replace("    ls = sub.add_parser(", '''    r = sub.add_parser("remove", help="remove stock")
    r.add_argument("name")
    r.add_argument("qty", type=positive_int, nargs="?")
    r.set_defaults(func=cmd_remove)
    ls = sub.add_parser(''')
open("inventory/cli.py", "w").write(s)
EOF
}

# ── py_intervals: implement from a docstring spec with edge cases ─────────────────────────────
setup_py_intervals() {
  cat > intervals.py <<'EOF'
"""Helpers for half-open integer intervals [start, end)."""


def merge(intervals):
    """Merge half-open intervals.

    - intervals is an iterable of (start, end) pairs of ints, in any order; it is not modified.
    - Overlapping or touching intervals merge: (1, 3) and (3, 5) become (1, 5).
    - Empty intervals (start == end) are dropped.
    - An interval with start > end raises ValueError.
    - Returns a new list of (start, end) tuples sorted by start.
    """
    raise NotImplementedError


def covered(intervals):
    """Total length covered by the intervals; overlapping parts count once."""
    raise NotImplementedError


def gaps(intervals, lo, hi):
    """The parts of [lo, hi) not covered by any interval, as a sorted list of (start, end) tuples.

    Intervals may reach outside [lo, hi). lo > hi raises ValueError; lo == hi gives [].
    """
    raise NotImplementedError
EOF
  cat > test_intervals.py <<'EOF'
import unittest
from intervals import merge


class IntervalsTest(unittest.TestCase):
    def test_overlap(self):
        self.assertEqual(merge([(1, 3), (2, 4)]), [(1, 4)])


if __name__ == "__main__":
    unittest.main()
EOF
}
prompt_py_intervals() { echo "Implement merge(), covered() and gaps() in intervals.py exactly as their docstrings say. test_intervals.py only checks one case; the whole documented behavior must work."; }
check_py_intervals() {
  python3 - <<'EOF' 2>/dev/null
from intervals import merge, covered, gaps
assert merge([(1, 3), (2, 4)]) == [(1, 4)]
assert merge([(5, 7), (1, 3), (3, 4)]) == [(1, 4), (5, 7)]
assert merge([]) == [] and merge([(2, 2)]) == [] and merge([(1, 2), (2, 2), (2, 3)]) == [(1, 3)]
assert merge([(1, 10), (2, 3), (4, 5)]) == [(1, 10)]
src = [[4, 6], [1, 2]]
assert merge(src) == [(1, 2), (4, 6)] and src == [[4, 6], [1, 2]]
assert merge(iter([(3, 4), (0, 1)])) == [(0, 1), (3, 4)]
assert all(type(x) is tuple for x in merge([[1, 2]]))
for bad in ([(3, 1)], [(1, 2), (5, 4)]):
    try:
        merge(bad)
        raise SystemExit(1)
    except ValueError:
        pass
assert covered([(1, 3), (2, 6), (10, 11)]) == 6 and covered([]) == 0 and covered([(1, 1)]) == 0
assert gaps([(2, 4), (6, 8)], 0, 10) == [(0, 2), (4, 6), (8, 10)]
assert gaps([(-5, 3), (8, 20)], 0, 10) == [(3, 8)]
assert gaps([], 1, 4) == [(1, 4)] and gaps([(0, 100)], 1, 4) == [] and gaps([(1, 2)], 5, 5) == []
assert gaps([(1, 3), (3, 5)], 1, 5) == []
try:
    gaps([], 5, 1)
    raise SystemExit(1)
except ValueError:
    pass
EOF
}
solve_py_intervals() {
  python3 - <<'EOF'
s = open("intervals.py").read()
parts = s.split("    raise NotImplementedError\n")
impl = [
'''    items = []
    for start, end in intervals:
        if start > end:
            raise ValueError("start > end: (%r, %r)" % (start, end))
        if start < end:
            items.append((start, end))
    items.sort()
    out = []
    for start, end in items:
        if out and start <= out[-1][1]:
            out[-1] = (out[-1][0], max(out[-1][1], end))
        else:
            out.append((start, end))
    return out
''',
'''    return sum(end - start for start, end in merge(intervals))
''',
'''    if lo > hi:
        raise ValueError("lo > hi")
    out, pos = [], lo
    for start, end in merge(intervals):
        if end <= pos:
            continue
        if start >= hi:
            break
        if start > pos:
            out.append((pos, start))
        pos = max(pos, end)
    if pos < hi:
        out.append((pos, hi))
    return out
''']
open("intervals.py", "w").write(parts[0] + impl[0] + parts[1] + impl[1] + parts[2] + impl[2] + parts[3])
EOF
}

# ── py_csv_escape: shared helper fix + migrate a second call site ─────────────────────────────
setup_py_csv_escape() {
  mkdir -p exporter
  touch exporter/__init__.py
  cat > exporter/csvout.py <<'EOF'
def format_row(fields):
    """Format one CSV line (no line terminator)."""
    out = []
    for f in fields:
        s = str(f)
        if "," in s:
            s = '"' + s + '"'
        out.append(s)
    return ",".join(out)
EOF
  cat > exporter/users.py <<'EOF'
from exporter.csvout import format_row


def export_users(users):
    lines = [format_row(["id", "name", "email"])]
    for u in users:
        lines.append(format_row([u["id"], u["name"], u.get("email")]))
    return "\r\n".join(lines) + "\r\n"
EOF
  cat > exporter/orders.py <<'EOF'
def export_orders(orders):
    lines = ["order_id,customer,note,total"]
    for o in orders:
        lines.append(",".join([str(o["id"]), o["customer"], o.get("note") or "", "%.2f" % o["total"]]))
    return "\r\n".join(lines) + "\r\n"
EOF
}
prompt_py_csv_escape() { echo "Our CSV exports break in spreadsheets when a field contains quotes or line breaks, and a missing email shows up as 'None'. Make exporter.csvout.format_row produce RFC 4180 fields: quote a field when it contains a comma, a double quote, CR or LF, and double any inner quotes; None becomes an empty field. exporter/orders.py builds its lines by hand: make it use format_row too, keeping its header and number format."; }
check_py_csv_escape() {
  python3 - <<'EOF' 2>/dev/null
import csv, io
from exporter.csvout import format_row
from exporter.users import export_users
from exporter.orders import export_orders
assert format_row(["a", "b,c", 'say "hi"', None, 3, "x\ny", "plain"]) == 'a,"b,c","say ""hi""",,3,"x\ny",plain'
assert format_row([]) == "" and format_row(["cr\rx"]) == '"cr\rx"' and format_row([""]) == ""
users = [{"id": 1, "name": 'Ann "A" Lee', "email": None}, {"id": 2, "name": "Bo, Jr.", "email": "b@x\nevil"}]
rows = list(csv.reader(io.StringIO(export_users(users), newline="")))
assert rows == [["id", "name", "email"], ["1", 'Ann "A" Lee', ""], ["2", "Bo, Jr.", "b@x\nevil"]], rows
orders = [{"id": 7, "customer": "Smith, J", "note": 'fragile "glass"\r\nleave at door', "total": 12.5},
          {"id": 8, "customer": "Lee", "total": 3}]
text = export_orders(orders)
assert text.startswith("order_id,customer,note,total\r\n")
rows = list(csv.reader(io.StringIO(text, newline="")))
assert rows == [["order_id", "customer", "note", "total"],
                ["7", "Smith, J", 'fragile "glass"\r\nleave at door', "12.50"], ["8", "Lee", "", "3.00"]], rows
assert "format_row" in open("exporter/orders.py").read()
EOF
}
solve_py_csv_escape() {
  cat > exporter/csvout.py <<'EOF'
def format_row(fields):
    """Format one CSV line (no line terminator), RFC 4180 quoting; None is an empty field."""
    out = []
    for f in fields:
        s = "" if f is None else str(f)
        if any(c in s for c in ',"\r\n'):
            s = '"' + s.replace('"', '""') + '"'
        out.append(s)
    return ",".join(out)
EOF
  cat > exporter/orders.py <<'EOF'
from exporter.csvout import format_row


def export_orders(orders):
    lines = [format_row(["order_id", "customer", "note", "total"])]
    for o in orders:
        lines.append(format_row([o["id"], o["customer"], o.get("note") or "", "%.2f" % o["total"]]))
    return "\r\n".join(lines) + "\r\n"
EOF
}

# ── py_retry_feature: extend a decorator without breaking existing callers ────────────────────
setup_py_retry_feature() {
  cat > retry.py <<'EOF'
import time


def retry(times=3, delay=0.0, sleep=time.sleep):
    """Call the function up to `times` times, sleeping `delay` seconds between failed attempts."""
    def deco(fn):
        def wrapper(*args, **kwargs):
            for attempt in range(times):
                try:
                    return fn(*args, **kwargs)
                except Exception:
                    if attempt == times - 1:
                        raise
                    sleep(delay)
        return wrapper
    return deco
EOF
  cat > jobs.py <<'EOF'
from retry import retry

calls = []


@retry(times=2, delay=0.0)
def flaky_job():
    """Fails once, then works."""
    calls.append(1)
    if len(calls) < 2:
        raise RuntimeError("first call fails")
    return "done"


if __name__ == "__main__":
    print(flaky_job())
EOF
}
prompt_py_retry_feature() { echo "Extend the retry decorator in retry.py: (1) a new 'on' argument: an exception class or tuple of classes to retry (default Exception); any other exception propagates at once, without sleeping; (2) a new 'backoff' multiplier: the n-th wait is delay * backoff**(n-1) (default 1.0); (3) times < 1 raises ValueError when retry(...) is called; (4) the decorated function keeps its __name__ and __doc__. Existing callers (jobs.py) must keep working."; }
check_py_retry_feature() {
  python3 - <<'EOF' 2>/dev/null
from retry import retry
import jobs
assert jobs.flaky_job() == "done" and jobs.flaky_job.__doc__ == "Fails once, then works."
sl, n = [], {"c": 0}
@retry(times=4, delay=1.0, backoff=2.0, sleep=sl.append)
def f():
    """doc"""
    n["c"] += 1
    if n["c"] < 4:
        raise IOError("x")
    return "ok"
assert f() == "ok" and sl == [1.0, 2.0, 4.0], sl
assert f.__name__ == "f" and f.__doc__ == "doc"
sl.clear()
@retry(times=3, on=KeyError, sleep=sl.append)
def g():
    raise ValueError("v")
try:
    g()
    raise SystemExit(1)
except ValueError:
    pass
assert sl == []
m = {"c": 0}
@retry(times=3, on=(KeyError, IOError), delay=0.5, sleep=sl.append)
def h():
    m["c"] += 1
    raise KeyError("k")
try:
    h()
    raise SystemExit(1)
except KeyError:
    pass
assert m["c"] == 3 and sl == [0.5, 0.5], (m, sl)
for bad in (0, -1):
    try:
        retry(times=bad)
        raise SystemExit(1)
    except ValueError:
        pass
s2 = []
@retry(sleep=s2.append)
def k():
    raise RuntimeError
try:
    k()
    raise SystemExit(1)
except RuntimeError:
    pass
assert s2 == [0.0, 0.0]
EOF
}
solve_py_retry_feature() {
  cat > retry.py <<'EOF'
import functools
import time


def retry(times=3, delay=0.0, sleep=time.sleep, on=Exception, backoff=1.0):
    """Call the function up to `times` times, sleeping between failed attempts.

    Only exceptions matching `on` are retried; the n-th wait is delay * backoff**(n-1).
    """
    if times < 1:
        raise ValueError("times must be >= 1")

    def deco(fn):
        @functools.wraps(fn)
        def wrapper(*args, **kwargs):
            wait = delay
            for attempt in range(times):
                try:
                    return fn(*args, **kwargs)
                except on:
                    if attempt == times - 1:
                        raise
                    sleep(wait)
                    wait *= backoff
        return wrapper
    return deco
EOF
}
