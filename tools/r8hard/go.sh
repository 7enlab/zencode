


setup_go_fetchall_bug() {
  printf 'module example.com/pool\n\ngo 1.22\n' > go.mod
  cat > pool.go <<'EOF'
// Package pool runs work concurrently.
package pool

// FetchAll calls f for every id using at most `workers` goroutines at once (workers < 1 means
// 1). It returns the results in the order of ids. If any call fails, it returns nil and the
// error of the failing id that comes first in ids; every call has finished when it returns.
func FetchAll(ids []int, workers int, f func(int) (string, error)) ([]string, error) {
	var out []string
	var firstErr error
	sem := make(chan struct{}, workers)
	for _, id := range ids {
		go func(id int) {
			sem <- struct{}{}
			defer func() { <-sem }()
			s, err := f(id)
			if err != nil {
				firstErr = err
				return
			}
			out = append(out, s)
		}(id)
	}
	return out, firstErr
}
EOF
  cat > pool_test.go <<'EOF'
package pool

import (
	"fmt"
	"reflect"
	"testing"
	"time"
)

func TestFetchAllReturnsAll(t *testing.T) {
	got, err := FetchAll([]int{1, 2, 3}, 2, func(i int) (string, error) {
		time.Sleep(time.Duration(4-i) * time.Millisecond)
		return fmt.Sprint("r", i), nil
	})
	if err != nil || !reflect.DeepEqual(got, []string{"r1", "r2", "r3"}) {
		t.Fatalf("got %v, %v", got, err)
	}
}
EOF
}
prompt_go_fetchall_bug() { echo "go test fails: FetchAll in pool.go returns before its goroutines finish. Fix FetchAll so it does everything its doc comment promises. Do not edit pool_test.go."; }
check_go_fetchall_bug() {
  git diff --quiet HEAD -- pool_test.go && _r8_go_hidden . <<'EOF'
package pool

import (
	"errors"
	"fmt"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

func TestHiddenOrderAndLimit(t *testing.T) {
	var cur, peak int32
	var mu sync.Mutex
	ids := make([]int, 40)
	for i := range ids {
		ids[i] = i
	}
	got, err := FetchAll(ids, 4, func(i int) (string, error) {
		n := atomic.AddInt32(&cur, 1)
		mu.Lock()
		if n > peak {
			peak = n
		}
		mu.Unlock()
		time.Sleep(time.Duration((i*7)%5+1) * time.Millisecond)
		atomic.AddInt32(&cur, -1)
		return fmt.Sprint(i), nil
	})
	if err != nil || len(got) != 40 {
		t.Fatalf("got %d results, err %v", len(got), err)
	}
	for i, s := range got {
		if s != fmt.Sprint(i) {
			t.Fatalf("result %d = %q", i, s)
		}
	}
	if peak > 4 || peak < 2 {
		t.Fatalf("peak concurrency %d, want 2..4", peak)
	}
}

func TestHiddenFirstErrorByInputOrder(t *testing.T) {
	var calls int32
	got, err := FetchAll([]int{1, 2, 3, 4}, 4, func(i int) (string, error) {
		defer atomic.AddInt32(&calls, 1)
		if i == 2 {
			time.Sleep(20 * time.Millisecond)
			return "", errors.New("bad 2")
		}
		if i == 4 {
			return "", errors.New("bad 4")
		}
		time.Sleep(5 * time.Millisecond)
		return "ok", nil
	})
	if got != nil || err == nil || err.Error() != "bad 2" {
		t.Fatalf("got %v, %v", got, err)
	}
	if atomic.LoadInt32(&calls) != 4 {
		t.Fatalf("returned before all calls finished: %d", calls)
	}
}

func TestHiddenZeroWorkersAndEmpty(t *testing.T) {
	var cur, peak int32
	got, err := FetchAll([]int{5, 6, 7}, 0, func(i int) (string, error) {
		if n := atomic.AddInt32(&cur, 1); n > atomic.LoadInt32(&peak) {
			atomic.StoreInt32(&peak, n)
		}
		time.Sleep(2 * time.Millisecond)
		atomic.AddInt32(&cur, -1)
		return fmt.Sprint(i), nil
	})
	if err != nil || fmt.Sprint(got) != "[5 6 7]" || peak != 1 {
		t.Fatalf("got %v, %v, peak %d", got, err, peak)
	}
	got, err = FetchAll(nil, 3, func(int) (string, error) { return "", nil })
	if err != nil || len(got) != 0 {
		t.Fatalf("empty: %v %v", got, err)
	}
}
EOF
}
solve_go_fetchall_bug() {
  cat > pool.go <<'EOF'
// Package pool runs work concurrently.
package pool

import "sync"

// FetchAll calls f for every id using at most `workers` goroutines at once (workers < 1 means
// 1). It returns the results in the order of ids. If any call fails, it returns nil and the
// error of the failing id that comes first in ids; every call has finished when it returns.
func FetchAll(ids []int, workers int, f func(int) (string, error)) ([]string, error) {
	if workers < 1 {
		workers = 1
	}
	out := make([]string, len(ids))
	errs := make([]error, len(ids))
	sem := make(chan struct{}, workers)
	var wg sync.WaitGroup
	for i, id := range ids {
		wg.Add(1)
		go func(i, id int) {
			defer wg.Done()
			sem <- struct{}{}
			defer func() { <-sem }()
			out[i], errs[i] = f(id)
		}(i, id)
	}
	wg.Wait()
	for _, err := range errs {
		if err != nil {
			return nil, err
		}
	}
	return out, nil
}
EOF
}


setup_go_semver_bug() {
  printf 'module example.com/semver\n\ngo 1.22\n' > go.mod
  cat > semver.go <<'EOF'
// Package semver compares version strings.
package semver

// Compare returns -1, 0 or 1 as version a is lower than, equal to or higher than b.
//
// Versions look like "1.2.3", "v1.2", "2" or "1.0.0-rc.1": an optional "v", one to three
// dot-separated numeric parts (missing parts count as 0) and an optional "-" prerelease.
// Numeric parts compare as numbers (1.10.0 > 1.9.0). A version with a prerelease is lower than
// the same version without one (1.0.0-rc.1 < 1.0.0). Prereleases compare identifier by
// identifier (split on "."): numeric identifiers as numbers, others as strings, a numeric
// identifier is lower than a non-numeric one, and when all shared identifiers are equal the
// shorter list is lower (rc.2 < rc.10, alpha < alpha.1 < beta).
func Compare(a, b string) int {
	if a == b {
		return 0
	}
	if a < b {
		return -1
	}
	return 1
}

// Max returns the highest version in vs, or "" for an empty list. On ties the first one wins.
func Max(vs []string) string {
	best := ""
	for i, v := range vs {
		if i == 0 || Compare(v, best) > 0 {
			best = v
		}
	}
	return best
}
EOF
  cat > semver_test.go <<'EOF'
package semver

import "testing"

func TestNumericParts(t *testing.T) {
	if got := Compare("1.10.0", "1.9.0"); got != 1 {
		t.Fatalf("Compare(1.10.0, 1.9.0) = %d, want 1", got)
	}
}
EOF
}
prompt_go_semver_bug() { echo "Compare in semver.go is wrong (go test shows one failing case). Fix it so it implements every rule in its doc comment; Max must keep working. Do not edit the test."; }
check_go_semver_bug() {
  git diff --quiet HEAD -- semver_test.go && _r8_go_hidden . <<'EOF'
package semver

import "testing"

func TestHiddenCompare(t *testing.T) {
	cases := []struct {
		a, b string
		want int
	}{
		{"1.10.0", "1.9.0", 1}, {"1.2", "1.2.0", 0}, {"v1.2.3", "1.2.3", 0}, {"2", "1.99.99", 1},
		{"1.0.0-rc.1", "1.0.0", -1}, {"1.0.0", "1.0.0-rc.1", 1}, {"1.0.0-rc.2", "1.0.0-rc.10", -1},
		{"1.0.0-alpha", "1.0.0-alpha.1", -1}, {"1.0.0-alpha.1", "1.0.0-beta", -1},
		{"1.0.0-1", "1.0.0-alpha", -1}, {"1.0.0-rc.1", "1.0.0-rc.1", 0}, {"0.0.9", "0.0.10", -1},
		{"v2.0", "v10.0", -1}, {"1.2.3-beta", "1.2.2", 1},
	}
	for _, c := range cases {
		if got := Compare(c.a, c.b); got != c.want {
			t.Errorf("Compare(%q, %q) = %d, want %d", c.a, c.b, got, c.want)
		}
	}
}

func TestHiddenMax(t *testing.T) {
	if got := Max([]string{"1.2", "v1.10", "1.9.9"}); got != "v1.10" {
		t.Errorf("Max = %q", got)
	}
	if got := Max(nil); got != "" {
		t.Errorf("Max(nil) = %q", got)
	}
	if got := Max([]string{"1.0", "1.0.0"}); got != "1.0" {
		t.Errorf("tie: %q", got)
	}
	if got := Max([]string{"2.0.0-rc.1", "1.9", "2.0.0-rc.2"}); got != "2.0.0-rc.2" {
		t.Errorf("pre: %q", got)
	}
}
EOF
}
solve_go_semver_bug() {
  python3 - <<'EOF'
s = open("semver.go").read()
s = s.replace('package semver\n', 'package semver\n\nimport (\n\t"strconv"\n\t"strings"\n)\n', 1)
s = s.replace('''func Compare(a, b string) int {
	if a == b {
		return 0
	}
	if a < b {
		return -1
	}
	return 1
}''', '''func Compare(a, b string) int {
	an, ap := split(a)
	bn, bp := split(b)
	for i := 0; i < 3; i++ {
		if c := cmpInt(an[i], bn[i]); c != 0 {
			return c
		}
	}
	switch {
	case ap == "" && bp == "":
		return 0
	case ap == "":
		return 1
	case bp == "":
		return -1
	}
	ai, bi := strings.Split(ap, "."), strings.Split(bp, ".")
	for i := 0; i < len(ai) && i < len(bi); i++ {
		x, xerr := strconv.Atoi(ai[i])
		y, yerr := strconv.Atoi(bi[i])
		switch {
		case xerr == nil && yerr == nil:
			if c := cmpInt(x, y); c != 0 {
				return c
			}
		case xerr == nil:
			return -1
		case yerr == nil:
			return 1
		default:
			if c := strings.Compare(ai[i], bi[i]); c != 0 {
				return c
			}
		}
	}
	return cmpInt(len(ai), len(bi))
}

func split(v string) ([3]int, string) {
	v = strings.TrimPrefix(v, "v")
	core, pre, _ := strings.Cut(v, "-")
	var n [3]int
	for i, p := range strings.SplitN(core, ".", 3) {
		n[i], _ = strconv.Atoi(p)
	}
	return n, pre
}

func cmpInt(a, b int) int {
	switch {
	case a < b:
		return -1
	case a > b:
		return 1
	}
	return 0
}''')
open("semver.go", "w").write(s)
EOF
}


setup_go_rename_config() {
  printf 'module example.com/cfgtool\n\ngo 1.22\n' > go.mod
  mkdir -p config internal/report cmd/cfgtool testdata
  cat > config/config.go <<'EOF'
// Package config parses key = value files.
package config

import (
	"bufio"
	"fmt"
	"io"
	"sort"
	"strings"
)

// Cfg holds parsed settings.
type Cfg struct {
	values map[string]string
}

// ParseCfg reads "key = value" lines; blank lines and lines starting with # are skipped.
func ParseCfg(r io.Reader) (*Cfg, error) {
	c := &Cfg{values: map[string]string{}}
	sc := bufio.NewScanner(r)
	n := 0
	for sc.Scan() {
		n++
		line := strings.TrimSpace(sc.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		k, v, ok := strings.Cut(line, "=")
		if !ok {
			return nil, fmt.Errorf("line %d: missing '='", n)
		}
		c.values[strings.TrimSpace(k)] = strings.TrimSpace(v)
	}
	return c, sc.Err()
}

// Keys returns the keys in sorted order.
func (c *Cfg) Keys() []string {
	keys := make([]string, 0, len(c.values))
	for k := range c.values {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	return keys
}

// Lookup returns the value for key and whether it was set.
func (c *Cfg) Lookup(key string) (string, bool) {
	v, ok := c.values[key]
	return v, ok
}
EOF
  cat > config/load.go <<'EOF'
package config

import "os"

// LoadFile parses the file at path.
func LoadFile(path string) (*Cfg, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	return ParseCfg(f)
}
EOF
  cat > config/config_test.go <<'EOF'
package config

import (
	"strings"
	"testing"
)

func TestParseCfg(t *testing.T) {
	c, err := ParseCfg(strings.NewReader("a = 1\n# comment\n\nb=2\n"))
	if err != nil {
		t.Fatal(err)
	}
	if v, _ := c.Lookup("b"); v != "2" || len(c.Keys()) != 2 {
		t.Fatalf("got %v", c.Keys())
	}
}
EOF
  cat > internal/report/report.go <<'EOF'
// Package report renders a config for humans.
package report

import (
	"strings"

	"example.com/cfgtool/config"
)

// Render prints one "key=value" line per setting, sorted by key.
func Render(c *config.Cfg) string {
	var b strings.Builder
	for _, k := range c.Keys() {
		v, _ := c.Lookup(k)
		b.WriteString(k + "=" + v + "\n")
	}
	return b.String()
}
EOF
  cat > cmd/cfgtool/main.go <<'EOF'
package main

import (
	"fmt"
	"os"

	"example.com/cfgtool/config"
	"example.com/cfgtool/internal/report"
)

func main() {
	if len(os.Args) != 2 {
		fmt.Fprintln(os.Stderr, "usage: cfgtool FILE")
		os.Exit(2)
	}
	var c *config.Cfg
	c, err := config.LoadFile(os.Args[1])
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	fmt.Print(report.Render(c))
}
EOF
  printf '# sample\nname = demo\nport=8080\n' > testdata/sample.conf
}
prompt_go_rename_config() { echo "In this Go module, rename config.ParseCfg to config.Parse and the type config.Cfg to config.Config everywhere (all packages, the command and the tests), so neither old name is left in any .go file. Then add a method (c *Config) Get(key, def string) string that returns the value for key, or def when the key is not set. go build ./... and go vet ./... must stay clean."; }
check_go_rename_config() {
  ! grep -rqwE 'ParseCfg|Cfg' --include=*.go . && go build ./... >/dev/null 2>&1 && go vet ./... >/dev/null 2>&1 &&
  [ "$(go run ./cmd/cfgtool testdata/sample.conf 2>/dev/null)" = "$(printf 'name=demo\nport=8080')" ] &&
  _r8_go_hidden config <<'EOF'
package config_test

import (
	"strings"
	"testing"

	"example.com/cfgtool/config"
)

func TestHiddenParseAndGet(t *testing.T) {
	var c *config.Config
	c, err := config.Parse(strings.NewReader("a = 1\n# x\n\nb=two\nempty=\n"))
	if err != nil {
		t.Fatal(err)
	}
	if c.Get("a", "d") != "1" || c.Get("b", "d") != "two" || c.Get("zz", "d") != "d" || c.Get("empty", "d") != "" {
		t.Fatal("Get")
	}
	if _, err := config.Parse(strings.NewReader("ok=1\nbad\n")); err == nil || !strings.Contains(err.Error(), "line 2") {
		t.Fatalf("err %v", err)
	}
	f, err := config.LoadFile("../testdata/sample.conf")
	if err != nil || f.Get("port", "") != "8080" {
		t.Fatalf("LoadFile: %v", err)
	}
}
EOF
}
solve_go_rename_config() {
  find . -name '*.go' -exec sed -i 's/\bParseCfg\b/Parse/g; s/\bCfg\b/Config/g' {} +
  cat >> config/config.go <<'EOF'

// Get returns the value for key, or def when key is not set.
func (c *Config) Get(key, def string) string {
	if v, ok := c.values[key]; ok {
		return v
	}
	return def
}
EOF
}


setup_go_todo_json() {
  printf 'module example.com/todo\n\ngo 1.22\n' > go.mod
  cat > todo.go <<'EOF'
// Package todo is a tiny in-memory todo list.
package todo

import "errors"

// Item is one todo entry.
type Item struct {
	ID    int
	Title string
	Done  bool
	Tags  []string
}

// Store holds items in insertion order.
type Store struct {
	items []Item
	next  int
}

// Add appends an item and returns it. IDs start at 1.
func (s *Store) Add(title string, tags ...string) Item {
	s.next++
	it := Item{ID: s.next, Title: title, Tags: tags}
	s.items = append(s.items, it)
	return it
}

// Complete marks an item done.
func (s *Store) Complete(id int) error {
	for i := range s.items {
		if s.items[i].ID == id {
			s.items[i].Done = true
			return nil
		}
	}
	return errors.New("no such item")
}

// List returns a copy of all items.
func (s *Store) List() []Item {
	return append([]Item(nil), s.items...)
}
EOF
  cat > todo_test.go <<'EOF'
package todo

import "testing"

func TestAddComplete(t *testing.T) {
	var s Store
	a := s.Add("a")
	if err := s.Complete(a.ID); err != nil || !s.List()[0].Done {
		t.Fatal("complete")
	}
}
EOF
}
prompt_go_todo_json() { echo "Extend package todo: (1) Item marshals to JSON with keys id, title, done and tags, tags omitted when empty; (2) export var ErrNotFound and make Complete return it for an unknown id (errors.Is must match); (3) add (s *Store) Filter(done *bool, tag string) []Item: done == nil matches any state, tag == \"\" matches any tags, results in insertion order; (4) *Store marshals to JSON as {\"items\": [...], \"open\": N} where open counts items not done and items is [] (never null) for an empty store."; }
check_go_todo_json() {
  _r8_go_hidden . <<'EOF'
package todo

import (
	"encoding/json"
	"errors"
	"testing"
)

func TestHiddenJSON(t *testing.T) {
	b, _ := json.Marshal(Item{ID: 1, Title: "x"})
	if string(b) != `{"id":1,"title":"x","done":false}` {
		t.Fatalf("item: %s", b)
	}
	b, _ = json.Marshal(Item{ID: 2, Title: "y", Done: true, Tags: []string{"a"}})
	if string(b) != `{"id":2,"title":"y","done":true,"tags":["a"]}` {
		t.Fatalf("item: %s", b)
	}
	var s Store
	b, err := json.Marshal(&s)
	if err != nil || string(b) != `{"items":[],"open":0}` {
		t.Fatalf("empty store: %s %v", b, err)
	}
	s.Add("a", "home")
	s.Add("b", "work", "home")
	s.Add("c")
	if err := s.Complete(2); err != nil {
		t.Fatal(err)
	}
	b, _ = json.Marshal(&s)
	want := `{"items":[{"id":1,"title":"a","done":false,"tags":["home"]},{"id":2,"title":"b","done":true,"tags":["work","home"]},{"id":3,"title":"c","done":false}],"open":2}`
	if string(b) != want {
		t.Fatalf("store: %s", b)
	}
	if err := s.Complete(9); !errors.Is(err, ErrNotFound) {
		t.Fatalf("err: %v", err)
	}
}

func TestHiddenFilter(t *testing.T) {
	var s Store
	s.Add("a", "home")
	s.Add("b", "work", "home")
	s.Add("c")
	_ = s.Complete(1)
	yes, no := true, false
	ids := func(items []Item) (out []int) {
		for _, it := range items {
			out = append(out, it.ID)
		}
		return
	}
	if got := ids(s.Filter(nil, "")); len(got) != 3 {
		t.Fatalf("all: %v", got)
	}
	if got := ids(s.Filter(&no, "home")); len(got) != 1 || got[0] != 2 {
		t.Fatalf("open home: %v", got)
	}
	if got := ids(s.Filter(&yes, "")); len(got) != 1 || got[0] != 1 {
		t.Fatalf("done: %v", got)
	}
	if got := ids(s.Filter(nil, "home")); len(got) != 2 || got[0] != 1 || got[1] != 2 {
		t.Fatalf("home: %v", got)
	}
	if got := s.Filter(nil, "nope"); len(got) != 0 {
		t.Fatalf("nope: %v", got)
	}
}
EOF
}
solve_go_todo_json() {
  python3 - <<'EOF'
s = open("todo.go").read()
s = s.replace('import "errors"', 'import (\n\t"encoding/json"\n\t"errors"\n)\n\n// ErrNotFound is returned for an unknown item id.\nvar ErrNotFound = errors.New("no such item")')
s = s.replace('''	ID    int
	Title string
	Done  bool
	Tags  []string''', '''	ID    int      `json:"id"`
	Title string   `json:"title"`
	Done  bool     `json:"done"`
	Tags  []string `json:"tags,omitempty"`''')
s = s.replace('return errors.New("no such item")', 'return ErrNotFound')
s += '''
// Filter returns the items matching done (nil = any) and tag ("" = any), in insertion order.
func (s *Store) Filter(done *bool, tag string) []Item {
	var out []Item
	for _, it := range s.items {
		if done != nil && it.Done != *done {
			continue
		}
		if tag != "" {
			found := false
			for _, t := range it.Tags {
				found = found || t == tag
			}
			if !found {
				continue
			}
		}
		out = append(out, it)
	}
	return out
}

// MarshalJSON encodes the store as {"items": [...], "open": N}.
func (s *Store) MarshalJSON() ([]byte, error) {
	open := 0
	for _, it := range s.items {
		if !it.Done {
			open++
		}
	}
	items := s.items
	if items == nil {
		items = []Item{}
	}
	return json.Marshal(struct {
		Items []Item `json:"items"`
		Open  int    `json:"open"`
	}{items, open})
}
'''
open("todo.go", "w").write(s)
EOF
}
