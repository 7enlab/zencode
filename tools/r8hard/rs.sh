# r8-hard bench tasks, Rust part (sourced by tools/bench_tasks_r8hard.sh). No crates.io deps.

_r8_cargo_toml() {
  printf '[package]\nname = "%s"\nversion = "0.1.0"\nedition = "2021"\n\n[dependencies]\n' "$1" > Cargo.toml
  printf 'target/\n' > .gitignore
}

# ── rs_ringbuf_bug: two bugs in a ring buffer, one visible test ───────────────────────────────
setup_rs_ringbuf_bug() {
  _r8_cargo_toml ringbuf
  mkdir -p src tests
  cat > src/lib.rs <<'EOF'
//! Fixed-capacity ring buffer.

pub struct RingBuffer<T> {
    buf: Vec<Option<T>>,
    head: usize,
    len: usize,
}

impl<T: Clone> RingBuffer<T> {
    /// A buffer holding at most `cap` elements. Panics if `cap` is 0.
    pub fn new(cap: usize) -> Self {
        assert!(cap > 0, "capacity must be positive");
        RingBuffer { buf: vec![None; cap], head: 0, len: 0 }
    }

    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Append `v`; when the buffer is full the oldest element is overwritten.
    pub fn push(&mut self, v: T) {
        let idx = (self.head + self.len) % self.capacity();
        self.buf[idx] = Some(v);
        if self.len < self.capacity() {
            self.len += 1;
        }
    }

    /// Remove and return the oldest element.
    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        let v = self.buf[self.head].take();
        self.head = (self.head + 1) % self.capacity();
        self.len -= 1;
        v
    }

    /// The elements from oldest to newest.
    pub fn to_vec(&self) -> Vec<T> {
        (0..self.len)
            .map(|i| self.buf[(self.head + i) % self.len].clone().unwrap())
            .collect()
    }
}
EOF
  cat > tests/ring.rs <<'EOF'
use ringbuf::RingBuffer;

#[test]
fn overwrite_keeps_newest() {
    let mut r = RingBuffer::new(3);
    for i in 1..=5 {
        r.push(i);
    }
    assert_eq!(r.to_vec(), vec![3, 4, 5]);
}
EOF
}
prompt_rs_ringbuf_bug() { echo "cargo test fails for the ring buffer in src/lib.rs. Fix RingBuffer so it behaves as its doc comments say for any sequence of push and pop calls. Do not change the existing test."; }
check_rs_ringbuf_bug() {
  git diff --quiet HEAD -- tests/ring.rs && _r8_rs_hidden zz_hidden <<'EOF'
use ringbuf::RingBuffer;

#[test]
fn overwrite_then_pop() {
    let mut r = RingBuffer::new(3);
    for i in 1..=5 {
        r.push(i);
    }
    assert_eq!(r.to_vec(), vec![3, 4, 5]);
    assert_eq!(r.len(), 3);
    assert_eq!(r.pop(), Some(3));
    assert_eq!(r.to_vec(), vec![4, 5]);
}

#[test]
fn wrap_after_pops() {
    let mut r = RingBuffer::new(4);
    r.push(1);
    r.push(2);
    r.push(3);
    assert_eq!(r.pop(), Some(1));
    assert_eq!(r.pop(), Some(2));
    r.push(4);
    r.push(5);
    assert_eq!(r.to_vec(), vec![3, 4, 5]);
    r.push(6);
    r.push(7);
    assert_eq!(r.to_vec(), vec![4, 5, 6, 7]);
    assert_eq!(r.pop(), Some(4));
    assert_eq!(r.len(), 3);
}

#[test]
fn capacity_one() {
    let mut r = RingBuffer::new(1);
    r.push("a");
    r.push("b");
    assert_eq!(r.to_vec(), vec!["b"]);
    assert_eq!(r.pop(), Some("b"));
    assert_eq!(r.pop(), None);
    assert!(r.is_empty());
    assert_eq!(r.capacity(), 1);
}
EOF
}
solve_rs_ringbuf_bug() {
  python3 - <<'EOF'
s = open("src/lib.rs").read()
s = s.replace("        if self.len < self.capacity() {\n            self.len += 1;\n        }",
              "        if self.len < self.capacity() {\n            self.len += 1;\n        } else {\n"
              "            self.head = (self.head + 1) % self.capacity();\n        }")
s = s.replace("(self.head + i) % self.len]", "(self.head + i) % self.capacity()]")
open("src/lib.rs", "w").write(s)
EOF
}

# ── rs_field_rename: rename a struct field across modules + add Display ───────────────────────
setup_rs_field_rename() {
  _r8_cargo_toml orders
  mkdir -p src tests
  cat > src/lib.rs <<'EOF'
pub mod order;
pub mod pricing;
pub mod report;

pub use order::LineItem;
EOF
  cat > src/order.rs <<'EOF'
#[derive(Debug, Clone, PartialEq)]
pub struct LineItem {
    pub sku: String,
    pub qty: u32,
    pub unit_cents: u64,
}

impl LineItem {
    pub fn new(sku: &str, qty: u32, unit_cents: u64) -> Self {
        LineItem { sku: sku.to_string(), qty, unit_cents }
    }

    pub fn line_cents(&self) -> u64 {
        self.qty as u64 * self.unit_cents
    }
}
EOF
  cat > src/pricing.rs <<'EOF'
use crate::order::LineItem;

/// Sum of all lines in cents. A line with 10 or more units gets 10% off that line
/// (the discount is rounded down to the cent).
pub fn subtotal(items: &[LineItem]) -> u64 {
    items
        .iter()
        .map(|it| {
            let line = it.line_cents();
            if it.qty >= 10 {
                line - line / 10
            } else {
                line
            }
        })
        .sum()
}

pub fn total_units(items: &[LineItem]) -> u32 {
    items.iter().map(|it| it.qty).sum()
}
EOF
  cat > src/report.rs <<'EOF'
use crate::order::LineItem;
use crate::pricing::{subtotal, total_units};

/// One-line summary such as "15 units, total 113.04".
pub fn summary(items: &[LineItem]) -> String {
    let c = subtotal(items);
    format!("{} units, total {}.{:02}", total_units(items), c / 100, c % 100)
}
EOF
  cat > tests/basic.rs <<'EOF'
use orders::{report, LineItem};

#[test]
fn summary_counts_units() {
    let items = vec![LineItem::new("A", 2, 100), LineItem::new("B", 1, 50)];
    assert_eq!(items[0].qty, 2);
    assert_eq!(report::summary(&items), "3 units, total 2.50");
}
EOF
}
prompt_rs_field_rename() { echo "In this Rust crate, rename the LineItem field 'qty' to 'quantity' everywhere (struct, constructor, all modules and tests) so the old name no longer appears anywhere. Then implement std::fmt::Display for LineItem as '<quantity> x <sku> @ <unit price>', the unit price in currency units with 2 decimals, e.g. '3 x ABC @ 1.50'. Behavior must otherwise stay the same."; }
check_rs_field_rename() {
  ! grep -rqw qty src tests && _r8_rs_hidden zz_hidden <<'EOF'
use orders::{pricing, report, LineItem};

#[test]
fn renamed_field_and_display() {
    let a = LineItem { sku: "ABC".into(), quantity: 3, unit_cents: 150 };
    assert_eq!(a.to_string(), "3 x ABC @ 1.50");
    let b = LineItem::new("Z9", 12, 1005);
    assert_eq!(b.quantity, 12);
    assert_eq!(format!("{}", b), "12 x Z9 @ 10.05");
    assert_eq!(LineItem::new("c", 1, 5).to_string(), "1 x c @ 0.05");
    assert_eq!(pricing::subtotal(&[a.clone(), b.clone()]), 450 + 12060 - 1206);
    assert_eq!(pricing::total_units(&[a.clone(), b.clone()]), 15);
    assert_eq!(report::summary(&[a, b]), "15 units, total 113.04");
}
EOF
}
solve_rs_field_rename() {
  sed -i 's/\bqty\b/quantity/g' src/*.rs tests/basic.rs
  cat >> src/order.rs <<'EOF'

impl std::fmt::Display for LineItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} x {} @ {}.{:02}", self.quantity, self.sku, self.unit_cents / 100, self.unit_cents % 100)
    }
}
EOF
}

# ── rs_duration_parse: implement a parser with a precise error enum ───────────────────────────
setup_rs_duration_parse() {
  _r8_cargo_toml durparse
  mkdir -p src tests
  cat > src/lib.rs <<'EOF'
//! Parse compact durations.

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// The input is empty.
    Empty,
    /// A unit letter with no digits before it, e.g. "h" or "1hm".
    MissingNumber,
    /// Digits at the end with no unit after them, e.g. "10" or "1h5".
    MissingUnit,
    /// A character that is not a digit or one of the units h, m, s (e.g. ' ', '-', 'd').
    BadUnit(char),
    /// A unit repeated or out of h, m, s order, e.g. "1m1h" or "1s1s".
    OutOfOrder,
    /// A number or the total does not fit in u64 seconds.
    Overflow,
}

/// Parse durations like "1h30m", "45s", "2h", "1h0m5s" or "90m" into seconds.
///
/// Each of the units h, m, s may appear at most once and in that order; leading zeros are fine.
/// The input is read left to right and the first problem found is reported.
pub fn parse_duration(s: &str) -> Result<u64, ParseError> {
    let _ = s;
    todo!()
}
EOF
  cat > tests/basic.rs <<'EOF'
use durparse::parse_duration;

#[test]
fn simple() {
    assert_eq!(parse_duration("1h30m"), Ok(5400));
    assert_eq!(parse_duration("45s"), Ok(45));
}
EOF
}
prompt_rs_duration_parse() { echo "Implement parse_duration in src/lib.rs exactly as the doc comments describe, returning the right ParseError variant for each kind of bad input. Do not change the public API. The test in tests/ only covers the happy path."; }
check_rs_duration_parse() {
  _r8_rs_hidden zz_hidden <<'EOF'
use durparse::{parse_duration as p, ParseError as E};

#[test]
fn ok_values() {
    assert_eq!(p("1h30m"), Ok(5400));
    assert_eq!(p("2h"), Ok(7200));
    assert_eq!(p("90m"), Ok(5400));
    assert_eq!(p("1h0m5s"), Ok(3605));
    assert_eq!(p("007s"), Ok(7));
    assert_eq!(p("0s"), Ok(0));
    assert_eq!(p("1m1s"), Ok(61));
    assert_eq!(p("18446744073709551615s"), Ok(u64::MAX));
}

#[test]
fn errors() {
    assert_eq!(p(""), Err(E::Empty));
    assert_eq!(p("h"), Err(E::MissingNumber));
    assert_eq!(p("1hm"), Err(E::MissingNumber));
    assert_eq!(p("10"), Err(E::MissingUnit));
    assert_eq!(p("1h5"), Err(E::MissingUnit));
    assert_eq!(p("1h 30m"), Err(E::BadUnit(' ')));
    assert_eq!(p("-5s"), Err(E::BadUnit('-')));
    assert_eq!(p("1d"), Err(E::BadUnit('d')));
    assert_eq!(p("1m1h"), Err(E::OutOfOrder));
    assert_eq!(p("1s1s"), Err(E::OutOfOrder));
    assert_eq!(p("18446744073709551616s"), Err(E::Overflow));
    assert_eq!(p("5124095576030432h"), Err(E::Overflow));
    assert_eq!(p("1m18446744073709551615s"), Err(E::Overflow));
}
EOF
}
solve_rs_duration_parse() {
  python3 - <<'EOF'
s = open("src/lib.rs").read()
s = s.replace("    let _ = s;\n    todo!()\n", '''    if s.is_empty() {
        return Err(ParseError::Empty);
    }
    let mut total: u64 = 0;
    let mut num: Option<u64> = None;
    let mut last_rank = 0;
    for c in s.chars() {
        if let Some(d) = c.to_digit(10) {
            let n = num.unwrap_or(0);
            num = Some(n.checked_mul(10).and_then(|n| n.checked_add(d as u64)).ok_or(ParseError::Overflow)?);
            continue;
        }
        let (rank, mult) = match c {
            'h' => (1, 3600u64),
            'm' => (2, 60),
            's' => (3, 1),
            other => return Err(ParseError::BadUnit(other)),
        };
        let n = num.take().ok_or(ParseError::MissingNumber)?;
        if rank <= last_rank {
            return Err(ParseError::OutOfOrder);
        }
        last_rank = rank;
        total = n.checked_mul(mult).and_then(|v| total.checked_add(v)).ok_or(ParseError::Overflow)?;
    }
    if num.is_some() {
        return Err(ParseError::MissingUnit);
    }
    Ok(total)
''')
open("src/lib.rs", "w").write(s)
EOF
}

# ── rs_shape_triangle: new trait impl + registry entry + aggregate fn across modules ───────────
setup_rs_shape_triangle() {
  _r8_cargo_toml shapes
  mkdir -p src/shapes tests
  cat > src/lib.rs <<'EOF'
pub mod registry;
pub mod shapes;

pub use shapes::Shape;
EOF
  cat > src/shapes/mod.rs <<'EOF'
mod circle;
mod rect;

pub use circle::Circle;
pub use rect::Rect;

pub trait Shape {
    fn area(&self) -> f64;
    fn name(&self) -> &'static str;
}
EOF
  cat > src/shapes/circle.rs <<'EOF'
use super::Shape;

pub struct Circle {
    pub r: f64,
}

impl Shape for Circle {
    fn area(&self) -> f64 {
        std::f64::consts::PI * self.r * self.r
    }
    fn name(&self) -> &'static str {
        "circle"
    }
}
EOF
  cat > src/shapes/rect.rs <<'EOF'
use super::Shape;

pub struct Rect {
    pub w: f64,
    pub h: f64,
}

impl Shape for Rect {
    fn area(&self) -> f64 {
        self.w * self.h
    }
    fn name(&self) -> &'static str {
        "rect"
    }
}
EOF
  cat > src/registry.rs <<'EOF'
use crate::shapes::{Circle, Rect, Shape};

/// Build a shape from a spec such as "circle:2" or "rect:2x3". Returns None for anything
/// invalid: unknown kind, wrong number of values, unparsable or non-positive numbers.
pub fn from_spec(spec: &str) -> Option<Box<dyn Shape>> {
    let (kind, args) = spec.split_once(':')?;
    match kind {
        "circle" => {
            let r: f64 = args.parse().ok()?;
            (r > 0.0).then(|| Box::new(Circle { r }) as Box<dyn Shape>)
        }
        "rect" => {
            let (w, h) = args.split_once('x')?;
            let (w, h): (f64, f64) = (w.parse().ok()?, h.parse().ok()?);
            (w > 0.0 && h > 0.0).then(|| Box::new(Rect { w, h }) as Box<dyn Shape>)
        }
        _ => None,
    }
}
EOF
  cat > tests/basic.rs <<'EOF'
use shapes::registry::from_spec;

#[test]
fn rect_area() {
    assert_eq!(from_spec("rect:2x3").unwrap().area(), 6.0);
    assert!(from_spec("rect:0x3").is_none());
}
EOF
}
prompt_rs_shape_triangle() { echo "Add a triangle shape to this crate: pub struct Triangle { pub a: f64, pub b: f64, pub c: f64 } in src/shapes/triangle.rs, exported as shapes::shapes::Triangle, implementing Shape (Heron's formula, name \"triangle\"). from_spec must accept \"triangle:3,4,5\" and return None for invalid triangles (a side <= 0, wrong number of sides, unparsable, or sides that break the strict triangle inequality, e.g. 1,2,3). Also add pub fn total_area(specs: &[&str]) -> Result<f64, String> to registry.rs: the sum of the areas, or an Err whose message contains the first invalid spec."; }
check_rs_shape_triangle() {
  _r8_rs_hidden zz_hidden <<'EOF'
use shapes::registry::{from_spec, total_area};
use shapes::shapes::Triangle;
use shapes::Shape;

#[test]
fn triangle() {
    let t = Triangle { a: 3.0, b: 4.0, c: 5.0 };
    assert!((t.area() - 6.0).abs() < 1e-9);
    assert_eq!(t.name(), "triangle");
    let s = from_spec("triangle:3,4,5").unwrap();
    assert!((s.area() - 6.0).abs() < 1e-9);
    assert_eq!(s.name(), "triangle");
    assert!((from_spec("triangle:2,2,2").unwrap().area() - 3f64.sqrt()).abs() < 1e-9);
    for bad in ["triangle:1,2,3", "triangle:1,1,5", "triangle:0,4,5", "triangle:3,4", "triangle:3,4,5,6",
                "triangle:a,b,c", "triangle:-3,4,5", "triangle:"] {
        assert!(from_spec(bad).is_none(), "{bad}");
    }
    assert!((from_spec("circle:1").unwrap().area() - std::f64::consts::PI).abs() < 1e-9);
}

#[test]
fn total() {
    let t = total_area(&["rect:2x3", "triangle:3,4,5"]).unwrap();
    assert!((t - 12.0).abs() < 1e-9);
    let e = total_area(&["rect:2x3", "rect:2y3", "circle:x"]).unwrap_err();
    assert!(e.contains("rect:2y3"), "{e}");
    assert!(!e.contains("circle:x"), "{e}");
    assert_eq!(total_area(&[]).unwrap(), 0.0);
}
EOF
}
solve_rs_shape_triangle() {
  cat > src/shapes/triangle.rs <<'EOF'
use super::Shape;

pub struct Triangle {
    pub a: f64,
    pub b: f64,
    pub c: f64,
}

impl Shape for Triangle {
    fn area(&self) -> f64 {
        let s = (self.a + self.b + self.c) / 2.0;
        (s * (s - self.a) * (s - self.b) * (s - self.c)).sqrt()
    }
    fn name(&self) -> &'static str {
        "triangle"
    }
}
EOF
  sed -i 's/^mod rect;/mod rect;\nmod triangle;/; s/^pub use rect::Rect;/pub use rect::Rect;\npub use triangle::Triangle;/' src/shapes/mod.rs
  python3 - <<'EOF'
s = open("src/registry.rs").read()
s = s.replace("use crate::shapes::{Circle, Rect, Shape};", "use crate::shapes::{Circle, Rect, Shape, Triangle};")
s = s.replace("        _ => None,", '''        "triangle" => {
            let v: Vec<f64> = args.split(',').map(|x| x.parse().ok()).collect::<Option<_>>()?;
            if v.len() != 3 || v.iter().any(|x| *x <= 0.0) {
                return None;
            }
            let (a, b, c) = (v[0], v[1], v[2]);
            (a + b > c && a + c > b && b + c > a).then(|| Box::new(Triangle { a, b, c }) as Box<dyn Shape>)
        }
        _ => None,''')
s += '''
/// Sum of the areas of all specs, or an error naming the first invalid one.
pub fn total_area(specs: &[&str]) -> Result<f64, String> {
    specs.iter().try_fold(0.0, |acc, s| {
        from_spec(s).map(|sh| acc + sh.area()).ok_or_else(|| format!("invalid shape spec: {s}"))
    })
}
'''
open("src/registry.rs", "w").write(s)
EOF
}

# ── rs_wordfreq_bug: tokenizing + tie order, needs the test and doc comment read ──────────────
setup_rs_wordfreq_bug() {
  _r8_cargo_toml wordfreq
  mkdir -p src tests
  cat > src/lib.rs <<'EOF'
use std::collections::HashMap;

/// Count words in `text` and return the `n` most frequent as (word, count).
///
/// Words are lower-cased. A word is a run of letters, digits or apostrophes, so "don't" is one
/// word and "end." is "end"; a run made only of apostrophes is not a word. Results are ordered by
/// count, highest first, then by word in ascending order.
pub fn top_n(text: &str, n: usize) -> Vec<(String, usize)> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for w in text.split_whitespace() {
        *counts.entry(w.to_string()).or_insert(0) += 1;
    }
    let mut v: Vec<(String, usize)> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v.truncate(n);
    v
}
EOF
  cat > tests/basic.rs <<'EOF'
use wordfreq::top_n;

fn owned(v: &[(&str, usize)]) -> Vec<(String, usize)> {
    v.iter().map(|(w, c)| (w.to_string(), *c)).collect()
}

#[test]
fn punctuation_case_and_ties() {
    assert_eq!(top_n("Hello, hello world. World! b a", 3), owned(&[("hello", 2), ("world", 2), ("a", 1)]));
}
EOF
}
prompt_rs_wordfreq_bug() { echo "cargo test fails in this crate. Fix top_n in src/lib.rs so it follows its doc comment for all inputs. Do not edit the test."; }
check_rs_wordfreq_bug() {
  git diff --quiet HEAD -- tests/basic.rs && _r8_rs_hidden zz_hidden <<'EOF'
use wordfreq::top_n;

fn owned(v: &[(&str, usize)]) -> Vec<(String, usize)> {
    v.iter().map(|(w, c)| (w.to_string(), *c)).collect()
}

#[test]
fn rules() {
    assert_eq!(top_n("Hello, hello world. World! b a", 3), owned(&[("hello", 2), ("world", 2), ("a", 1)]));
    assert_eq!(top_n("Don't don't stop ' '' (stop)", 5), owned(&[("don't", 2), ("stop", 2)]));
    assert_eq!(top_n("Ünïcode ünïcode x", 1), owned(&[("ünïcode", 2)]));
    assert_eq!(top_n("a1 a1 b2;b2;b2 c-3", 10), owned(&[("b2", 3), ("a1", 2), ("3", 1), ("c", 1)]));
    assert_eq!(top_n("z y x w", 2), owned(&[("w", 1), ("x", 1)]));
    assert_eq!(top_n("one two", 0), owned(&[]));
    assert_eq!(top_n("", 3), owned(&[]));
    assert_eq!(top_n("...!!!", 3), owned(&[]));
}
EOF
}
solve_rs_wordfreq_bug() {
  python3 - <<'EOF'
s = open("src/lib.rs").read()
s = s.replace("""    for w in text.split_whitespace() {
        *counts.entry(w.to_string()).or_insert(0) += 1;
    }""", """    for w in text.split(|c: char| !(c.is_alphanumeric() || c == '\\'')) {
        if w.chars().all(|c| c == '\\'') {
            continue;
        }
        *counts.entry(w.to_lowercase()).or_insert(0) += 1;
    }""")
s = s.replace("v.sort_by(|a, b| b.1.cmp(&a.1));", "v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));")
open("src/lib.rs", "w").write(s)
EOF
}
