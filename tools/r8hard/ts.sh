# r8-hard bench tasks, TypeScript/JavaScript part (sourced by tools/bench_tasks_r8hard.sh).
# TypeScript runs on Node 22 with --experimental-strip-types (no tsc, no npm deps): imports
# use the .ts extension and only erasable syntax works (no enums/namespaces/parameter properties).

_r8_ts_pkg() {
  cat > package.json <<EOF
{"name": "$1", "version": "1.0.0", "type": "module", "private": true,
 "scripts": {"test": "node --experimental-strip-types --no-warnings --test $2"}}
EOF
  cat > README.md <<'EOF'
TypeScript run directly by Node 22 (`--experimental-strip-types`, no tsc, no dependencies).
Imports end in `.ts`. Run the tests with `npm test`.
EOF
}

# ── ts_emitter_bug: once() skips the next listener, off() removes the wrong one ───────────────
setup_ts_emitter_bug() {
  _r8_ts_pkg emitter test/emitter.test.ts
  mkdir -p src test
  cat > src/emitter.ts <<'EOF'
type Listener = (...args: any[]) => void;

export class Emitter {
  private listeners = new Map<string, Listener[]>();

  on(event: string, fn: Listener): this {
    const list = this.listeners.get(event) ?? [];
    list.push(fn);
    this.listeners.set(event, list);
    return this;
  }

  /** Like on(), but the listener runs at most once. */
  once(event: string, fn: Listener): this {
    const wrapper = (...args: any[]) => {
      this.off(event, wrapper);
      fn(...args);
    };
    return this.on(event, wrapper);
  }

  /** Remove one registration of fn (added with on() or once()) for event. */
  off(event: string, fn: Listener): this {
    const list = this.listeners.get(event) ?? [];
    list.splice(list.indexOf(fn), 1);
    return this;
  }

  /** Call every listener of event in registration order; true if there were any. */
  emit(event: string, ...args: any[]): boolean {
    const list = this.listeners.get(event) ?? [];
    for (const fn of list) fn(...args);
    return list.length > 0;
  }

  listenerCount(event: string): number {
    return (this.listeners.get(event) ?? []).length;
  }
}
EOF
  cat > test/emitter.test.ts <<'EOF'
import { test } from "node:test";
import assert from "node:assert/strict";
import { Emitter } from "../src/emitter.ts";

test("once listener does not skip the next one", () => {
  const e = new Emitter();
  const log: string[] = [];
  e.once("x", () => log.push("a"));
  e.on("x", () => log.push("b"));
  e.emit("x");
  assert.deepEqual(log, ["a", "b"]);
});
EOF
}
prompt_ts_emitter_bug() { echo "src/emitter.ts has bugs: a listener registered with once() makes the next listener of the same event get skipped (npm test fails), and off() sometimes removes the wrong listener. Fix the Emitter so every method does what its doc comment says, including off(event, fn) removing a pending once(event, fn). Keep the public API."; }
check_ts_emitter_bug() {
  git diff --quiet HEAD -- test && _r8_node_hidden zz_hidden_emitter.test.ts <<'EOF'
import { test } from "node:test";
import assert from "node:assert/strict";
import { Emitter } from "./src/emitter.ts";

test("once in the middle", () => {
  const e = new Emitter();
  const log: string[] = [];
  e.on("x", () => log.push("a"));
  e.once("x", () => log.push("b"));
  e.on("x", () => log.push("c"));
  e.once("x", () => log.push("d"));
  e.on("x", () => log.push("f"));
  e.emit("x");
  e.emit("x");
  assert.deepEqual(log, ["a", "b", "c", "d", "f", "a", "c", "f"]);
  assert.equal(e.listenerCount("x"), 3);
});

test("off of an unknown fn removes nothing", () => {
  const e = new Emitter();
  const log: number[] = [];
  e.on("x", () => log.push(1));
  e.on("x", () => log.push(2));
  e.off("x", () => {});
  e.off("nope", () => {});
  e.emit("x");
  assert.deepEqual(log, [1, 2]);
});

test("off removes a pending once", () => {
  const e = new Emitter();
  let n = 0;
  const f = () => n++;
  e.once("x", f);
  e.off("x", f);
  assert.equal(e.emit("x"), false);
  assert.equal(n, 0);
  assert.equal(e.listenerCount("x"), 0);
});

test("same fn twice, off removes one; args are passed", () => {
  const e = new Emitter();
  const got: number[] = [];
  const f = (a: number, b: number) => got.push(a + b);
  e.on("x", f).on("x", f);
  e.off("x", f);
  assert.equal(e.listenerCount("x"), 1);
  assert.equal(e.emit("x", 2, 3), true);
  assert.deepEqual(got, [5]);
  assert.equal(e.emit("y"), false);
});
EOF
}
solve_ts_emitter_bug() {
  cat > src/emitter.ts <<'EOF'
type Listener = (...args: any[]) => void;
type Entry = { fn: Listener; orig: Listener };

export class Emitter {
  private listeners = new Map<string, Entry[]>();

  on(event: string, fn: Listener): this {
    return this.add(event, { fn, orig: fn });
  }

  /** Like on(), but the listener runs at most once. */
  once(event: string, fn: Listener): this {
    const entry: Entry = {
      orig: fn,
      fn: (...args: any[]) => {
        this.remove(event, entry);
        fn(...args);
      },
    };
    return this.add(event, entry);
  }

  /** Remove one registration of fn (added with on() or once()) for event. */
  off(event: string, fn: Listener): this {
    const list = this.listeners.get(event) ?? [];
    const i = list.findIndex((e) => e.orig === fn || e.fn === fn);
    if (i >= 0) list.splice(i, 1);
    return this;
  }

  /** Call every listener of event in registration order; true if there were any. */
  emit(event: string, ...args: any[]): boolean {
    const list = [...(this.listeners.get(event) ?? [])];
    for (const e of list) e.fn(...args);
    return list.length > 0;
  }

  listenerCount(event: string): number {
    return (this.listeners.get(event) ?? []).length;
  }

  private add(event: string, entry: Entry): this {
    const list = this.listeners.get(event) ?? [];
    list.push(entry);
    this.listeners.set(event, list);
    return this;
  }

  private remove(event: string, entry: Entry): void {
    const list = this.listeners.get(event) ?? [];
    const i = list.indexOf(entry);
    if (i >= 0) list.splice(i, 1);
  }
}
EOF
}

# ── ts_groupby_feature: new module + barrel export ────────────────────────────────────────────
setup_ts_groupby_feature() {
  _r8_ts_pkg collections test/collections.test.ts
  mkdir -p src/collections test
  cat > src/collections/chunk.ts <<'EOF'
/** Split items into arrays of at most `size` items. */
export function chunk<T>(items: readonly T[], size: number): T[][] {
  if (!Number.isInteger(size) || size < 1) throw new RangeError("size must be a positive integer");
  const out: T[][] = [];
  for (let i = 0; i < items.length; i += size) out.push(items.slice(i, i + size));
  return out;
}
EOF
  cat > src/collections/unique.ts <<'EOF'
/** Items without duplicates (by ===), first occurrence kept. */
export function unique<T>(items: readonly T[]): T[] {
  return [...new Set(items)];
}
EOF
  cat > src/index.ts <<'EOF'
export { chunk } from "./collections/chunk.ts";
export { unique } from "./collections/unique.ts";
EOF
  cat > test/collections.test.ts <<'EOF'
import { test } from "node:test";
import assert from "node:assert/strict";
import { chunk, unique } from "../src/index.ts";

test("chunk and unique", () => {
  assert.deepEqual(chunk([1, 2, 3], 2), [[1, 2], [3]]);
  assert.deepEqual(unique([1, 1, 2]), [1, 2]);
});
EOF
}
prompt_ts_groupby_feature() { echo "Add src/collections/group.ts with three functions and export them from src/index.ts: groupBy(items, keyFn) returns a Map from key to the items with that key (keys in order of first appearance, items in input order); countBy(items, keyFn) returns a Map from key to count, same key order; partition(items, pred) returns [matching, rest], both in input order. keyFn and pred are called exactly once per item, as fn(item, index). None of them modifies the input array."; }
check_ts_groupby_feature() {
  _r8_node_hidden zz_hidden_group.test.ts <<'EOF'
import { test } from "node:test";
import assert from "node:assert/strict";
import { groupBy, countBy, partition, chunk } from "./src/index.ts";

test("groupBy", () => {
  const words = ["bb", "a", "cc", "d", "eee"];
  const copy = [...words];
  const g = groupBy(words, (w: string) => w.length);
  assert.ok(g instanceof Map);
  assert.deepEqual([...g.keys()], [2, 1, 3]);
  assert.deepEqual(g.get(2), ["bb", "cc"]);
  assert.deepEqual(g.get(1), ["a", "d"]);
  assert.deepEqual(words, copy);
  const idx: number[] = [];
  const g2 = groupBy(["x", "y", "z"], (_: string, i: number) => { idx.push(i); return i % 2 ? "odd" : "even"; });
  assert.deepEqual(idx, [0, 1, 2]);
  assert.deepEqual([...g2.entries()], [["even", ["x", "z"]], ["odd", ["y"]]]);
  assert.equal(groupBy([], (x: number) => x).size, 0);
});

test("countBy", () => {
  let calls = 0;
  const c = countBy([3, 1, 3, 2, 1, 3], (n: number) => { calls++; return "k" + n; });
  assert.equal(calls, 6);
  assert.deepEqual([...c.entries()], [["k3", 3], ["k1", 2], ["k2", 1]]);
});

test("partition", () => {
  const seen: number[] = [];
  const [even, odd] = partition([5, 2, 8, 1, 4], (n: number, i: number) => { seen.push(i); return n % 2 === 0; });
  assert.deepEqual(even, [2, 8, 4]);
  assert.deepEqual(odd, [5, 1]);
  assert.deepEqual(seen, [0, 1, 2, 3, 4]);
  assert.deepEqual(partition([], () => true), [[], []]);
  assert.deepEqual(chunk([1], 1), [[1]]);
});
EOF
}
solve_ts_groupby_feature() {
  cat > src/collections/group.ts <<'EOF'
/** Map from key to the items with that key; keys in first-appearance order. */
export function groupBy<T, K>(items: readonly T[], keyFn: (item: T, index: number) => K): Map<K, T[]> {
  const out = new Map<K, T[]>();
  items.forEach((item, i) => {
    const k = keyFn(item, i);
    const list = out.get(k);
    if (list) list.push(item);
    else out.set(k, [item]);
  });
  return out;
}

/** Map from key to how many items have it; keys in first-appearance order. */
export function countBy<T, K>(items: readonly T[], keyFn: (item: T, index: number) => K): Map<K, number> {
  const out = new Map<K, number>();
  items.forEach((item, i) => {
    const k = keyFn(item, i);
    out.set(k, (out.get(k) ?? 0) + 1);
  });
  return out;
}

/** [items matching pred, the rest], both in input order. */
export function partition<T>(items: readonly T[], pred: (item: T, index: number) => boolean): [T[], T[]] {
  const yes: T[] = [];
  const no: T[] = [];
  items.forEach((item, i) => (pred(item, i) ? yes : no).push(item));
  return [yes, no];
}
EOF
  echo 'export { groupBy, countBy, partition } from "./collections/group.ts";' >> src/index.ts
}

# ── ts_money_cents: integer-cent arithmetic across three modules ──────────────────────────────
setup_ts_money_cents() {
  _r8_ts_pkg shop test/checkout.test.ts
  mkdir -p src test
  cat > src/cart.ts <<'EOF'
export interface Line {
  name: string;
  price: number;
  qty: number;
}

export class Cart {
  lines: Line[] = [];

  /** price in dollars with at most 2 decimals, e.g. 19.99 */
  add(name: string, price: number, qty = 1): void {
    if (!Number.isInteger(qty) || qty < 1) throw new Error("bad qty");
    this.lines.push({ name, price, qty });
  }

  itemCount(): number {
    return this.lines.reduce((n, l) => n + l.qty, 0);
  }

  /** Sum of price * qty over all lines. */
  subtotal(): number {
    return this.lines.reduce((s, l) => s + l.price * l.qty, 0);
  }
}
EOF
  cat > src/money.ts <<'EOF'
export function formatMoney(amount: number): string {
  return "$" + amount.toFixed(2);
}
EOF
  cat > src/checkout.ts <<'EOF'
import { Cart } from "./cart.ts";
import { formatMoney } from "./money.ts";

export interface Receipt {
  subtotal: string;
  discount: string;
  tax: string;
  total: string;
}

/**
 * discountPct: e.g. 15 for 15% off the subtotal. taxRate: e.g. 0.0825, applied to
 * (subtotal - discount). Discount and tax are each rounded half up to the cent;
 * total = subtotal - discount + tax.
 */
export function checkout(cart: Cart, discountPct: number, taxRate: number): Receipt {
  const sub = cart.subtotal();
  const discount = (sub * discountPct) / 100;
  const tax = (sub - discount) * taxRate;
  const total = sub - discount + tax;
  return { subtotal: formatMoney(sub), discount: formatMoney(discount), tax: formatMoney(tax), total: formatMoney(total) };
}
EOF
  cat > src/summary.ts <<'EOF'
import { Cart } from "./cart.ts";
import { formatMoney } from "./money.ts";

export function cartSummary(cart: Cart): string {
  return `${cart.itemCount()} items, ${formatMoney(cart.subtotal())}`;
}
EOF
  cat > test/checkout.test.ts <<'EOF'
import { test } from "node:test";
import assert from "node:assert/strict";
import { Cart } from "../src/cart.ts";
import { checkout } from "../src/checkout.ts";

test("10% off 10.05 rounds the discount half up", () => {
  const c = new Cart();
  c.add("book", 10.05);
  assert.deepEqual(checkout(c, 10, 0), { subtotal: "$10.05", discount: "$1.01", tax: "$0.00", total: "$9.04" });
});
EOF
}
prompt_ts_money_cents() { echo "checkout() is sometimes off by a cent (npm test fails). Switch the shop to integer cents: Cart.add still takes prices in dollars (at most 2 decimals), but Cart.subtotal() returns integer cents, formatMoney(cents) in src/money.ts takes integer cents, and checkout() computes discount and tax in cents, each rounded half up as its doc comment says. Update every caller (src/summary.ts too) so all output strings stay in the same '\$12.34' format."; }
check_ts_money_cents() {
  git diff --quiet HEAD -- test && _r8_node_hidden zz_hidden_money.test.ts <<'EOF'
import { test } from "node:test";
import assert from "node:assert/strict";
import { Cart } from "./src/cart.ts";
import { checkout } from "./src/checkout.ts";
import { formatMoney } from "./src/money.ts";
import { cartSummary } from "./src/summary.ts";

const cart = (...lines: [number, number][]) => {
  const c = new Cart();
  lines.forEach(([p, q], i) => c.add("item" + i, p, q));
  return c;
};

test("cents everywhere", () => {
  const a = cart([19.99, 3], [0.1, 1]);
  assert.equal(a.subtotal(), 6007);
  assert.deepEqual(checkout(a, 15, 0.0825), { subtotal: "$60.07", discount: "$9.01", tax: "$4.21", total: "$55.27" });
  assert.deepEqual(checkout(cart([10.05, 1]), 10, 0), { subtotal: "$10.05", discount: "$1.01", tax: "$0.00", total: "$9.04" });
  assert.deepEqual(checkout(cart([1.15, 2], [0.01, 3]), 0, 0.2), { subtotal: "$2.33", discount: "$0.00", tax: "$0.47", total: "$2.80" });
  assert.deepEqual(checkout(cart([0.07, 1]), 50, 0.07), { subtotal: "$0.07", discount: "$0.04", tax: "$0.00", total: "$0.03" });
  assert.deepEqual(checkout(cart([4.35, 7]), 12.5, 0.0725), { subtotal: "$30.45", discount: "$3.81", tax: "$1.93", total: "$28.57" });
  assert.equal(cart([1.15, 1]).subtotal(), 115);
  assert.equal(formatMoney(1234), "$12.34");
  assert.equal(formatMoney(5), "$0.05");
  assert.equal(formatMoney(0), "$0.00");
  assert.equal(cartSummary(a), "4 items, $60.07");
  assert.throws(() => new Cart().add("x", 1, 0));
});
EOF
}
solve_ts_money_cents() {
  python3 - <<'EOF'
s = open("src/cart.ts").read()
s = s.replace("  /** Sum of price * qty over all lines. */\n  subtotal(): number {\n    return this.lines.reduce((s, l) => s + l.price * l.qty, 0);",
              "  /** Sum of price * qty over all lines, in integer cents. */\n  subtotal(): number {\n    return this.lines.reduce((s, l) => s + Math.round(l.price * 100) * l.qty, 0);")
open("src/cart.ts", "w").write(s)
open("src/money.ts", "w").write('export function formatMoney(cents: number): string {\n'
    '  const sign = cents < 0 ? "-" : "";\n  const c = Math.abs(cents);\n'
    '  return sign + "$" + Math.floor(c / 100) + "." + String(c % 100).padStart(2, "0");\n}\n')
s = open("src/checkout.ts").read()
s = s.replace("  const discount = (sub * discountPct) / 100;\n  const tax = (sub - discount) * taxRate;",
              "  const discount = Math.round((sub * discountPct) / 100);\n  const tax = Math.round((sub - discount) * taxRate);")
open("src/checkout.ts", "w").write(s)
EOF
}

# ── js_routes_long: precise edit + insertion in a 600-line table with near-duplicates ─────────
setup_js_routes_long() {
  python3 - <<'EOF'
out = ["// Route table. Generated from the API spec; edit with care, order matters (first match wins).",
       "module.exports = ["]
for v in ("v1", "v2"):
    for i in range(150 if v == "v1" else 40):
        r = "res%d" % i
        out.append('  { method: "GET", path: "/api/%s/%s", role: "user", handler: "%s.index" },' % (v, r, r))
        out.append('  { method: "GET", path: "/api/%s/%s/:id", role: "user", handler: "%s.show" },' % (v, r, r))
        out.append('  { method: "POST", path: "/api/%s/%s", role: "editor", handler: "%s.create" },' % (v, r, r))
        out.append('  { method: "DELETE", path: "/api/%s/%s/:id", role: "user", handler: "%s.destroy" },' % (v, r, r))
out.append("];")
open("routes.js", "w").write("\n".join(out) + "\n")
EOF
  cat > package.json <<'EOF'
{"name": "routes", "version": "1.0.0", "type": "commonjs", "private": true}
EOF
}
prompt_js_routes_long() { echo "In routes.js, make DELETE /api/v1/res17/:id require role \"admin\" instead of \"user\", and add a new route { method: \"POST\", path: \"/api/v1/res17/:id/restore\", role: \"admin\", handler: \"res17.restore\" } directly after it. Change nothing else: every other route keeps its values and position."; }
check_js_routes_long() {
  git show HEAD:routes.js > zz_orig_routes.cjs 2>/dev/null && node -e '
const assert = require("node:assert/strict");
const orig = require("./zz_orig_routes.cjs");
const now = require("./routes.js");
const i = orig.findIndex((r) => r.method === "DELETE" && r.path === "/api/v1/res17/:id");
assert.ok(i > 0);
assert.equal(now.length, orig.length + 1);
assert.deepEqual(now[i], { ...orig[i], role: "admin" });
assert.deepEqual(now[i + 1], { method: "POST", path: "/api/v1/res17/:id/restore", role: "admin", handler: "res17.restore" });
assert.deepEqual(now.slice(0, i), orig.slice(0, i));
assert.deepEqual(now.slice(i + 2), orig.slice(i + 1));
' >/dev/null 2>&1
  local r=$?
  rm -f zz_orig_routes.cjs
  return $r
}
solve_js_routes_long() {
  python3 - <<'EOF'
s = open("routes.js").read()
old = '  { method: "DELETE", path: "/api/v1/res17/:id", role: "user", handler: "res17.destroy" },\n'
new = ('  { method: "DELETE", path: "/api/v1/res17/:id", role: "admin", handler: "res17.destroy" },\n'
       '  { method: "POST", path: "/api/v1/res17/:id/restore", role: "admin", handler: "res17.restore" },\n')
assert s.count(old) == 1
open("routes.js", "w").write(s.replace(old, new))
EOF
}
