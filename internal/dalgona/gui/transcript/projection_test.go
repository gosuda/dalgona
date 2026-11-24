package transcript

import (
	"encoding/json/jsontext"
	"errors"
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

func toKind(b byte) jsontext.Kind { return jsontext.Kind(b) }

func TestArgumentIndentIsTwoSpaces(t *testing.T) {
	if argumentIndent != "  " {
		t.Fatalf("indent reads %q, want two spaces", argumentIndent)
	}
	if len(argumentIndent) != 2 {
		t.Fatalf("indent holds %d bytes, want 2", len(argumentIndent))
	}
}

type jsonSizeCase struct {
	name   string
	raw    string
	limit  int
	size   int
	valid  bool
	budget bool
}

func checkIndentedJSONSize(t *testing.T, c jsonSizeCase) {
	t.Helper()
	size, valid, err := indentedJSONSize([]byte(c.raw), c.limit)
	if c.budget {
		if !errors.Is(err, errSourceBudget) {
			t.Fatalf("%s: err reads %v, want errSourceBudget", c.name, err)
		}
		if !valid {
			t.Fatalf("%s: valid reads false, want true with a budget error", c.name)
		}
		return
	}
	if err != nil {
		t.Fatalf("%s: err reads %v, want nil", c.name, err)
	}
	if valid != c.valid || (valid && size != c.size) {
		t.Fatalf("%s: size reads %d valid %v, want %d %v", c.name, size, valid, c.size, c.valid)
	}
}

func TestIndentedJSONSizeComputesIndentedExtent(t *testing.T) {
	cases := []jsonSizeCase{
		{name: "null", raw: `null`, limit: 1 << 20, size: 4, valid: true},
		{name: "empty array", raw: `[]`, limit: 1 << 20, size: 2, valid: true},
		{name: "empty object", raw: `{}`, limit: 1 << 20, size: 2, valid: true},
		{name: "object", raw: `{"a":1}`, limit: 1 << 20, size: 12, valid: true},
		{name: "invalid", raw: `{"a":}`, limit: 1 << 20, size: 6, valid: false},
		{name: "two roots", raw: `{} {}`, limit: 1 << 20, size: 0, valid: false},
		{name: "oversize", raw: `{"a":1}`, limit: 0, budget: true},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) { checkIndentedJSONSize(t, c) })
	}
}

type jsonResultCase struct {
	name   string
	size   int
	roots  int64
	over   bool
	want   int
	valid  bool
	budget bool
}

func checkIndentedJSONResult(t *testing.T, c jsonResultCase) {
	t.Helper()
	size, valid, err := indentedJSONResult(c.size, c.roots, c.over)
	if c.budget {
		if !errors.Is(err, errSourceBudget) {
			t.Fatalf("%s: err reads %v, want errSourceBudget", c.name, err)
		}
		return
	}
	if err != nil {
		t.Fatalf("%s: err reads %v, want nil", c.name, err)
	}
	if size != c.want || valid != c.valid {
		t.Fatalf("%s: size reads %d valid %v, want %d %v", c.name, size, valid, c.want, c.valid)
	}
}

func TestIndentedJSONResultDecidesValidity(t *testing.T) {
	cases := []jsonResultCase{
		{name: "two roots", size: 9, roots: 2, over: false, want: 0, valid: false},
		{name: "oversize", size: 0, roots: 1, over: true, want: 0, valid: true, budget: true},
		{name: "fits", size: 12, roots: 1, over: false, want: 12, valid: true},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) { checkIndentedJSONResult(t, c) })
	}
}

type jsonExtentCase struct {
	name  string
	size  int
	next  int
	limit int
	want  int
	full  bool
}

func checkAddJSONExtent(t *testing.T, c jsonExtentCase) {
	t.Helper()
	got, full := addJSONExtent(c.size, c.next, c.limit)
	if got != c.want || full != c.full {
		t.Fatalf("%s: extent reads %d full %v, want %d %v", c.name, got, full, c.want, c.full)
	}
}

func TestAddJSONExtentCapsAtLimit(t *testing.T) {
	cases := []jsonExtentCase{
		{name: "fits", size: 5, next: 3, limit: 10, want: 8, full: false},
		{name: "exact", size: 5, next: 5, limit: 10, want: 10, full: false},
		{name: "over", size: 5, next: 6, limit: 10, want: 5, full: true},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) { checkAddJSONExtent(t, c) })
	}
}

type tokenSpacingCase struct {
	name  string
	depth int
	cont  byte
	count int64
	next  byte
	want  int
}

func TestJSONTokenSpacingIndentsHoldsAndCloses(t *testing.T) {
	cases := []tokenSpacingCase{
		{name: "top", depth: 0, cont: '{', count: 3, next: '}', want: 0},
		{name: "empty close", depth: 1, cont: '{', count: 0, next: '}', want: 0},
		{name: "close hold", depth: 1, cont: '{', count: 2, next: '}', want: 1},
		{name: "nested close", depth: 2, cont: '[', count: 1, next: ']', want: 3},
		{name: "colon", depth: 1, cont: '{', count: 1, next: '"', want: 2},
		{name: "first value", depth: 1, cont: '[', count: 0, next: '"', want: 3},
		{name: "later value", depth: 1, cont: '[', count: 1, next: '"', want: 4},
		{name: "object later", depth: 2, cont: '{', count: 2, next: '"', want: 6},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) { checkTokenSpacing(t, c) })
	}
}

func checkTokenSpacing(t *testing.T, c tokenSpacingCase) {
	t.Helper()
	got := jsonTokenSpacing(c.depth, toKind(c.cont), c.count, toKind(c.next))
	if got != c.want {
		t.Fatalf("%s: spacing reads %d, want %d", c.name, got, c.want)
	}
}

type prettyCase struct {
	name   string
	text   string
	output int
	want   string
	budget bool
}

func checkPrettyJSON(t *testing.T, c prettyCase) {
	t.Helper()
	l := newLoader()
	got, err := prettyJSON(c.text, c.output, l)
	if c.budget {
		if !errors.Is(err, errSourceBudget) {
			t.Fatalf("%s: err reads %v, want errSourceBudget", c.name, err)
		}
		return
	}
	if err != nil {
		t.Fatalf("%s: err reads %v, want nil", c.name, err)
	}
	if got != c.want {
		t.Fatalf("%s: pretty reads %q, want %q", c.name, got, c.want)
	}
	l.releaseProjections()
}

func TestPrettyJSONFormatsOrKeepsRaw(t *testing.T) {
	cases := []prettyCase{
		{name: "empty", text: "", output: 0, want: "{}"},
		{name: "invalid", text: "not json", output: 0, want: "not json"},
		{name: "object", text: `{"b":2}`, output: 0, want: "{\n  \"b\": 2\n}"},
		{name: "null", text: `null`, output: 0, want: `null`},
		{name: "over output", text: `{}`, output: 32<<20 + 1, budget: true},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) { checkPrettyJSON(t, c) })
	}
}

func TestPrettyJSONFormatsLoaderBody(t *testing.T) {
	l := newLoader()
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	item := loaderItem("u1", "i1", "output", 7)
	b, ok := l.want(thread, item, "output", false)
	if !ok {
		t.Fatal("want of a recorded field misses")
	}
	l.consume(b, state.ContentPage{Total: 7, Next: 7, Data: []byte(`{"b":2}`)})
	got, err := prettyJSON(b.text(), 0, l)
	if err != nil {
		t.Fatalf("pretty returns %v, want nil", err)
	}
	if want := "{\n  \"b\": 2\n}"; got != want {
		t.Fatalf("pretty reads %q, want %q", got, want)
	}
	l.releaseProjections()
}

func TestReserveJSONExtentHoldsFiveExtents(t *testing.T) {
	t.Run("valid", func(t *testing.T) { checkReserveExtentValid(t) })
	t.Run("invalid", func(t *testing.T) { checkReserveExtentInvalid(t) })
	t.Run("tight", func(t *testing.T) { checkReserveExtentTight(t) })
}

func checkReserveExtentValid(t *testing.T) {
	t.Helper()
	l := newLoader()
	size, valid, err := l.reserveJSONExtent([]byte(`{"b":2}`))
	if err != nil || !valid || size != 12 {
		t.Fatalf("valid extent reads %d %v %v, want 12 true nil", size, valid, err)
	}
	l.releaseProjections()
}

func checkReserveExtentInvalid(t *testing.T) {
	t.Helper()
	l := newLoader()
	size, valid, err := l.reserveJSONExtent([]byte(`not json`))
	if err != nil || valid {
		t.Fatalf("invalid extent reads %d %v %v, want raw length false nil", size, valid, err)
	}
	if size != len("not json") {
		t.Fatalf("invalid size reads %d, want %d", size, len("not json"))
	}
	l.releaseProjections()
}

func checkReserveExtentTight(t *testing.T) {
	t.Helper()
	l := newLoader()
	l.cachedSource = 32 << 20
	_, _, err := l.reserveJSONExtent([]byte(`{"b":2}`))
	if !errors.Is(err, errSourceBudget) {
		t.Fatalf("tight extent returns %v, want errSourceBudget", err)
	}
}

func TestReserveJSONInputHoldsStructuredLimit(t *testing.T) {
	l := newLoader()
	if err := l.reserveJSONInput(8<<20, 0); err != nil {
		t.Fatalf("structured limit returns %v, want nil", err)
	}
	if got := l.heldSourceBytes(); got != 16818178 {
		t.Fatalf("holds read %d bytes, want 16818178", got)
	}
}

func TestReserveJSONInputRefusesOverStructuredLimit(t *testing.T) {
	l := newLoader()
	if err := l.reserveJSONInput(8<<20+1, 0); !errors.Is(err, errSourceBudget) {
		t.Fatalf("over structured limit returns %v, want errSourceBudget", err)
	}
	if got := l.heldSourceBytes(); got != 0 {
		t.Fatalf("holds read %d bytes, want 0", got)
	}
}
