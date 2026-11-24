package transcript

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

func loaderRef(field string, total uint64) rpc.ContentReference {
	return rpc.ContentReference{ContentID: "content-" + field, Field: field, Total: rpc.FormatDecimal(total)}
}

func loaderItem(turn, id, field string, total uint64) state.ItemView {
	return state.ItemView{
		Key:    state.ItemKey{Turn: turn, Item: id},
		Header: rpc.ItemHeader{ID: id, Type: "tool", ItemRevision: "rev-" + id, Contents: []rpc.ContentReference{loaderRef(field, total)}},
		Field:  "output",
	}
}

func tailItem(turn, id, field, tail string, contents []rpc.ContentReference) state.ItemView {
	return state.ItemView{
		Key:    state.ItemKey{Turn: turn, Item: id},
		Header: rpc.ItemHeader{ID: id, Type: "tool", ItemRevision: "rev-" + id, Contents: contents},
		Field:  field,
		Tail:   []byte(tail),
	}
}

func loaderView(key state.ThreadKey, link client.Link, turns ...state.TurnView) *state.View {
	return state.Snapshot{Threads: []*state.ThreadView{{Key: key, Link: link, Turns: turns}}}.View()
}

func TestValidPrefixTrimsPartialRune(t *testing.T) {
	cases := []struct {
		name string
		data string
		want string
	}{
		{"ascii", "hello", "hello"},
		{"empty", "", ""},
		{"split euro", "ab\xe2\x82", "ab"},
		{"whole euro", "ab\xe2\x82\xac", "ab\xe2\x82\xac"},
		{"lone lead", "\xe2", ""},
		{"stray continuation", "a\x80", "a"},
		{"split emoji", "x\xf0\x9f\x98", "x"},
	}
	for _, c := range cases {
		if got := string(validPrefix([]byte(c.data))); got != c.want {
			t.Errorf("%s: validPrefix reads %q, want %q", c.name, got, c.want)
		}
	}
}

func TestBodyTextShowsReadablePrefix(t *testing.T) {
	cases := []struct {
		name string
		body body
		want string
	}{
		{"incomplete page", body{total: 10, next: 5, data: []byte("hello")}, "hello"},
		{"partial rune", body{total: 10, next: 5, data: []byte("ab\xe2\x82")}, "ab"},
		{"complete", body{total: 5, next: 5, data: []byte("hello")}, "hello"},
		{"complete scrubs", body{total: 3, next: 3, data: []byte("a\xffi")}, "a" + "�" + "i"},
		{"tail skips first line", body{total: 10, next: 5, start: 1, data: []byte("skip\nkeep")}, "keep"},
		{"tail without newline", body{total: 10, next: 5, start: 1, data: []byte("nonl")}, "nonl"},
	}
	for _, c := range cases {
		bd := c.body
		if got := bd.text(); got != c.want {
			t.Errorf("%s: text reads %q, want %q", c.name, got, c.want)
		}
	}
}

func TestBodyLargeFlagsInlineLimit(t *testing.T) {
	cases := []struct {
		total uint64
		want  bool
	}{
		{0, false},
		{1, false},
		{256 << 10, false},
		{256<<10 + 1, true},
	}
	for _, c := range cases {
		bd := body{total: c.total}
		if got := bd.large(); got != c.want {
			t.Errorf("total %d: large reads %v, want %v", c.total, got, c.want)
		}
	}
}

type serveAccessCase struct {
	name       string
	total      uint64
	next       uint64
	access     bool
	full       bool
	want       bool
	wantFlag   bool
	recheck    bool
	wantSecond bool
}

func checkServeAccessCase(t *testing.T, c serveAccessCase) {
	t.Helper()
	b := &body{total: c.total, next: c.next, accessSource: c.access}
	if got := b.serveIncompleteLargeOrFull(c.full); got != c.want {
		t.Errorf("%s: serve reads %v, want %v", c.name, got, c.want)
	}
	if b.accessSource != c.wantFlag {
		t.Errorf("%s: flag reads %v, want %v", c.name, b.accessSource, c.wantFlag)
	}
	if !c.recheck {
		return
	}
	b.next = b.total
	if got := b.serveIncompleteLargeOrFull(false); got != c.wantSecond {
		t.Errorf("%s: sticky serve reads %v, want %v", c.name, got, c.wantSecond)
	}
}

func TestBodyServeIncompleteLargeOrFull(t *testing.T) {
	cases := []serveAccessCase{
		{"incomplete sticks", 10, 0, false, false, true, true, true, true},
		{"small complete hidden", 10, 10, false, false, false, false, false, false},
		{"large served", 256<<10 + 1, 256<<10 + 1, false, false, true, true, false, false},
		{"full served", 10, 10, false, true, true, true, false, false},
		{"zero total waits", 0, 0, true, false, false, true, false, false},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) { checkServeAccessCase(t, c) })
	}
}

func TestLoaderConsumeLoadsBodyToComplete(t *testing.T) {
	l := newLoader()
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	item := state.ItemKey{Turn: "u1", Item: "i1"}
	b := l.wantRef(thread, item, loaderRef("output", 11), false)
	if b.start != 0 || b.next != 0 {
		t.Fatalf("fresh body starts at %d next %d, want 0 0", b.start, b.next)
	}
	l.consume(b, state.ContentPage{Total: 11, Next: 5, Data: []byte("hello")})
	if b.next != 5 || string(b.data) != "hello" {
		t.Fatalf("partial body holds next %d %q, want 5 hello", b.next, b.data)
	}
	if b.complete() {
		t.Fatal("partial body reads complete before its end")
	}
	l.consume(b, state.ContentPage{Total: 11, Next: 11, Data: []byte(" world")})
	if !b.complete() {
		t.Fatal("body is not complete after its final page")
	}
	if got := b.text(); got != "hello world" {
		t.Fatalf("text reads %q, want hello world", got)
	}
}

func TestLoaderConsumeResumesLargeBodiesAtTail(t *testing.T) {
	l := newLoader()
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	b := l.wantRef(thread, state.ItemKey{Turn: "u1", Item: "i1"}, loaderRef("output", 300000), false)
	if b.start != 300000-65536 || b.next != b.start {
		t.Fatalf("large body starts at %d next %d, want 234464 234464", b.start, b.next)
	}
	if !b.large() {
		t.Fatal("total 300000 does not read large")
	}
	l.consume(b, state.ContentPage{Total: 300000, Next: 300000, Data: []byte("tail")})
	if !b.complete() {
		t.Fatal("large body is not complete after its final page")
	}
	if got := b.text(); got != "tail" {
		t.Fatalf("text reads %q, want tail", got)
	}
	second := l.wantRef(thread, state.ItemKey{Turn: "u1", Item: "i2"}, loaderRef("other", 300000), false)
	l.consume(second, state.ContentPage{Total: 300000, Next: 300000, Data: []byte("head\ntail")})
	if got := second.text(); got != "tail" {
		t.Fatalf("tail text reads %q, want tail", got)
	}
}

func TestLoaderConsumeRejectsMismatch(t *testing.T) {
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	item := state.ItemKey{Turn: "u1", Item: "i1"}
	cases := []struct {
		name     string
		prime    uint64
		page     state.ContentPage
		wantNext uint64
	}{
		{"total shrinks", 0, state.ContentPage{Total: 9, Next: 9}, 0},
		{"next past total", 0, state.ContentPage{Total: 10, Next: 11}, 0},
		{"next regresses", 5, state.ContentPage{Total: 10, Next: 3}, 5},
	}
	for _, c := range cases {
		l := newLoader()
		b := l.wantRef(thread, item, loaderRef("output", 10), false)
		if c.prime != 0 {
			l.consume(b, state.ContentPage{Total: 10, Next: c.prime, Data: []byte("hello")})
		}
		l.consume(b, c.page)
		if !errors.Is(b.err, errMismatch) {
			t.Errorf("%s: err reads %v, want errMismatch", c.name, b.err)
		}
		if b.complete() {
			t.Errorf("%s: mismatched body reads complete", c.name)
		}
		if b.asked != 0 {
			t.Errorf("%s: asked reads %d, want 0", c.name, b.asked)
		}
		if b.next != c.wantNext {
			t.Errorf("%s: next reads %d, want %d", c.name, b.next, c.wantNext)
		}
	}
}

func TestLoaderConsumeRejectsStalledPage(t *testing.T) {
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	item := state.ItemKey{Turn: "u1", Item: "i1"}
	cases := []struct {
		name  string
		count bool
		prime uint64
		page  state.ContentPage
	}{
		{"empty first page", false, 0, state.ContentPage{Total: 10, Next: 0}},
		{"repeated offset", false, 5, state.ContentPage{Total: 10, Next: 5}},
		{"counted stall", true, 0, state.ContentPage{Total: 10, Next: 0}},
	}
	for _, c := range cases {
		l := newLoader()
		var b *body
		if c.count {
			b = l.counter(thread, item, loaderRef("output", 10))
		} else {
			b = l.wantRef(thread, item, loaderRef("output", 10), false)
		}
		if c.prime != 0 {
			l.consume(b, state.ContentPage{Total: 10, Next: c.prime, Data: []byte("hello")})
		}
		l.consume(b, c.page)
		if !errors.Is(b.err, errNoProgress) {
			t.Errorf("%s: err reads %v, want errNoProgress", c.name, b.err)
		}
		if b.next != c.prime {
			t.Errorf("%s: next reads %d, want %d", c.name, b.next, c.prime)
		}
	}
}

func TestLoaderCounterSeparatesCountBodies(t *testing.T) {
	l := newLoader()
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	item := state.ItemKey{Turn: "u1", Item: "i1"}
	ref := loaderRef("output", 300000)
	c := l.counter(thread, item, ref)
	w := l.wantRef(thread, item, ref, false)
	if c == w {
		t.Fatal("counter and content share one body")
	}
	if !c.key.count || w.key.count {
		t.Fatal("counter key does not carry the count mark")
	}
	if c.start != 0 {
		t.Fatalf("counted body starts at %d, want 0", c.start)
	}
	if got := c.end(); got != 300000 {
		t.Fatalf("counted end reads %d, want 300000", got)
	}
	if w.start != 300000-65536 {
		t.Fatalf("large body starts at %d, want 234464", w.start)
	}
	big := l.counter(thread, state.ItemKey{Turn: "u1", Item: "i2"}, loaderRef("big", 16<<20+100))
	if got := big.end(); got != 16<<20 {
		t.Fatalf("capped end reads %d, want 16777216", got)
	}
}

func TestLoaderRejectsUncanonicalTotal(t *testing.T) {
	l := newLoader()
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	bad := l.wantRef(thread, state.ItemKey{Turn: "u1", Item: "i1"}, rpc.ContentReference{ContentID: "bad", Field: "output", Total: "007"}, false)
	if bad.err == nil {
		t.Fatal("uncanonical total leaves no error")
	}
	if bad.complete() {
		t.Fatal("body with an unparsable total reads complete")
	}
}

func TestLoaderCounterCountsLines(t *testing.T) {
	l := newLoader()
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	c := l.counter(thread, state.ItemKey{Turn: "u1", Item: "i1"}, loaderRef("output", 11))
	l.consume(c, state.ContentPage{Total: 11, Next: 11, Data: []byte("a\nb\nc")})
	if c.next != 11 {
		t.Fatalf("counted next reads %d, want 11", c.next)
	}
	if len(c.data) != 0 {
		t.Fatalf("counted body stores %d bytes, want none", len(c.data))
	}
	if got := c.lineTotal(); got != 3 {
		t.Fatalf("line total reads %d, want 3", got)
	}
	if !c.complete() {
		t.Fatal("counted body is not complete at its end")
	}
	capped := l.counter(thread, state.ItemKey{Turn: "u1", Item: "i2"}, loaderRef("big", 16<<20+100))
	l.consume(capped, state.ContentPage{Total: 16<<20 + 100, Next: 16 << 20, Data: []byte("x\ny\n")})
	if !capped.complete() {
		t.Fatal("capped body is not complete at the count limit")
	}
	if got := capped.lineTotal(); got != 2 {
		t.Fatalf("capped line total reads %d, want 2", got)
	}
	quiet := l.counter(thread, state.ItemKey{Turn: "u1", Item: "i3"}, loaderRef("quiet", 10))
	l.consume(quiet, state.ContentPage{Total: 10, Next: 5})
	if quiet.next != 5 || quiet.lines != 0 {
		t.Fatalf("empty page leaves next %d lines %d, want 5 0", quiet.next, quiet.lines)
	}
}

func TestLoaderSourceHoldSharesBackingCharge(t *testing.T) {
	t.Run("shares", func(t *testing.T) { checkSourceHoldShares(t) })
	t.Run("releases", func(t *testing.T) { checkSourceHoldReleases(t) })
}

func checkSourceHoldShares(t *testing.T) {
	t.Helper()
	if document.ResidentSourceBudget != 32<<20 {
		t.Fatalf("resident budget reads %d, want 33554432", document.ResidentSourceBudget)
	}
	l := newLoader()
	if got := l.heldSourceBytes(); got != 0 {
		t.Fatalf("fresh holds read %d bytes, want 0", got)
	}
	backing := new(byte)
	_, ok := l.holdSource(backing, 100)
	if !ok {
		t.Fatal("hold of 100 bytes in an empty window fails")
	}
	if got := l.heldSourceBytes(); got != 100 {
		t.Fatalf("holds read %d bytes, want 100", got)
	}
	_, ok = l.holdSource(backing, 60)
	if !ok {
		t.Fatal("second owner of a held backing fails")
	}
	if got := l.heldSourceBytes(); got != 100 {
		t.Fatalf("shared holds read %d bytes, want 100", got)
	}
}

func checkSourceHoldReleases(t *testing.T) {
	t.Helper()
	l := newLoader()
	backing := new(byte)
	releaseFirst, _ := l.holdSource(backing, 100)
	releaseSecond, _ := l.holdSource(backing, 60)
	releaseFirst()
	if got := l.heldSourceBytes(); got != 100 {
		t.Fatalf("holds read %d bytes with one owner left, want 100", got)
	}
	releaseFirst()
	if got := l.heldSourceBytes(); got != 100 {
		t.Fatalf("holds read %d bytes after a double release, want 100", got)
	}
	releaseSecond()
	if got := l.heldSourceBytes(); got != 0 {
		t.Fatalf("holds read %d bytes after every release, want 0", got)
	}
}

func TestLoaderSourceHoldEnforcesResidentBudget(t *testing.T) {
	l := newLoader()
	backing := new(byte)
	if _, over := l.holdSource(backing, 32<<20+1); over {
		t.Fatal("hold past the resident budget succeeds")
	}
	if got := l.heldSourceBytes(); got != 0 {
		t.Fatalf("refused hold charges %d bytes, want 0", got)
	}
	grown, ok := l.holdSource(backing, 200)
	if !ok {
		t.Fatal("hold of 200 bytes in an empty window fails")
	}
	if got := l.heldSourceBytes(); got != 200 {
		t.Fatalf("holds read %d bytes, want 200", got)
	}
	grown()
	if got := l.heldSourceBytes(); got != 0 {
		t.Fatalf("holds read %d bytes after release, want 0", got)
	}
}

func TestLoaderProjectionRoomTracksOwnerBudget(t *testing.T) {
	l := newLoader()
	l.cachedSource = 100
	if got := l.projectionRoom(); got != 32<<20-100 {
		t.Fatalf("room reads %d, want 33554332", got)
	}
	if !l.holdProjection(1000) {
		t.Fatal("hold of 1000 projection bytes fails")
	}
	if got := l.projectionRoom(); got != 32<<20-100-1000 {
		t.Fatalf("room reads %d, want 33553332", got)
	}
	if l.holdProjection(32 << 20) {
		t.Fatal("hold past the remaining room succeeds")
	}
	if got := l.projectionRoom(); got != 32<<20-100-1000 {
		t.Fatalf("room reads %d after a refused hold, want 33553332", got)
	}
	l.releaseProjections()
	if got := l.projectionRoom(); got != 32<<20-100 {
		t.Fatalf("room reads %d after release, want 33554332", got)
	}
}

func TestLoaderProjectionDelegatesToOwner(t *testing.T) {
	l := newLoader()
	l.cachedSource = 100
	child := newLoader()
	child.projectionOwner = l
	if !child.holdProjection(500) {
		t.Fatal("child hold against its owner fails")
	}
	if got := l.heldSourceBytes(); got != 500 {
		t.Fatalf("owner holds read %d bytes, want 500", got)
	}
	child.releaseProjections()
	if got := l.heldSourceBytes(); got != 0 {
		t.Fatalf("owner holds read %d bytes after release, want 0", got)
	}
	if len(child.projectionHolds) != 0 {
		t.Fatalf("child keeps %d projection holds after release", len(child.projectionHolds))
	}
}

func TestSourceBudgetErrorWithoutRoom(t *testing.T) {
	l := newLoader()
	if err := l.reserveJSONInput(32<<20+1, 0); !errors.Is(err, errSourceBudget) {
		t.Fatalf("oversize text input returns %v, want errSourceBudget", err)
	}
	if err := l.reserveJSONInput(0, 32<<20+1); !errors.Is(err, errSourceBudget) {
		t.Fatalf("oversize output input returns %v, want errSourceBudget", err)
	}
	if err := l.reserveJSONInput(0, 0); err != nil {
		t.Fatalf("empty input returns %v, want nil", err)
	}
	if got := l.heldSourceBytes(); got != 40962 {
		t.Fatalf("holds read %d bytes, want 40962", got)
	}
	l.releaseProjections()
	if !l.holdProjection(32<<20 - 40961) {
		t.Fatal("hold of the room minus 40961 fails")
	}
	if err := l.reserveJSONInput(0, 0); !errors.Is(err, errSourceBudget) {
		t.Fatalf("tight room returns %v, want errSourceBudget", err)
	}
	l.releaseProjections()
}

func TestLoaderBeginTracksEpochAndWanted(t *testing.T) {
	l := newLoader()
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	first := state.Snapshot{}.View()
	l.begin(first)
	if l.epoch != 1 || l.clock != 1 {
		t.Fatalf("begin reads epoch %d clock %d, want 1 1", l.epoch, l.clock)
	}
	item := loaderItem("u1", "i1", "output", 10)
	l.want(thread, item, "output", false)
	if len(l.wanted) != 1 {
		t.Fatalf("wanted holds %d bodies, want 1", len(l.wanted))
	}
	l.begin(first)
	if l.epoch != 1 || l.clock != 2 {
		t.Fatalf("repeat begin reads epoch %d clock %d, want 1 2", l.epoch, l.clock)
	}
	if len(l.wanted) != 0 {
		t.Fatalf("repeat begin keeps %d wanted bodies, want 0", len(l.wanted))
	}
	second := state.Snapshot{}.View()
	l.begin(second)
	if l.epoch != 2 || l.clock != 3 {
		t.Fatalf("new view begin reads epoch %d clock %d, want 2 3", l.epoch, l.clock)
	}
}

func TestRefNamedFindsFieldReference(t *testing.T) {
	header := rpc.ItemHeader{Contents: []rpc.ContentReference{loaderRef("output", 10), loaderRef("error", 5)}}
	cases := []struct {
		name   string
		field  string
		wantID string
		wantOK bool
		header rpc.ItemHeader
	}{
		{"output", "output", "content-output", true, header},
		{"error", "error", "content-error", true, header},
		{"absent spill", "outputSpill", "", false, header},
		{"empty header", "output", "", false, rpc.ItemHeader{}},
	}
	for _, c := range cases {
		ref, ok := refNamed(c.header, c.field)
		if ok != c.wantOK {
			t.Errorf("%s: ok reads %v, want %v", c.name, ok, c.wantOK)
		}
		if ref.ContentID != c.wantID {
			t.Errorf("%s: content reads %q, want %q", c.name, ref.ContentID, c.wantID)
		}
	}
}

func TestLoaderWantDedupsByContentKey(t *testing.T) {
	l := newLoader()
	l.begin(state.Snapshot{}.View())
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	item := state.ItemView{
		Key:    state.ItemKey{Turn: "u1", Item: "i1"},
		Header: rpc.ItemHeader{ID: "i1", Type: "tool", Contents: []rpc.ContentReference{loaderRef("output", 10), loaderRef("error", 5)}},
	}
	first, ok := l.want(thread, item, "output", false)
	if !ok || first == nil {
		t.Fatal("want of a recorded field misses")
	}
	again, ok := l.want(thread, item, "output", false)
	if !ok || again != first {
		t.Fatal("want of one field twice splits bodies")
	}
	if len(l.wanted) != 1 {
		t.Fatalf("wanted holds %d bodies, want 1", len(l.wanted))
	}
	ref, _ := refNamed(item.Header, "output")
	if direct := l.wantRef(thread, item.Key, ref, false); direct != first {
		t.Fatal("want and wantRef of one reference split bodies")
	}
	other, ok := l.want(thread, item, "error", false)
	if !ok || other == first {
		t.Fatal("want of another field reuses the first body")
	}
}

func TestLoaderWantKeysThreadAndField(t *testing.T) {
	l := newLoader()
	l.begin(state.Snapshot{}.View())
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	item := state.ItemView{
		Key:    state.ItemKey{Turn: "u1", Item: "i1"},
		Header: rpc.ItemHeader{ID: "i1", Type: "tool", Contents: []rpc.ContentReference{loaderRef("output", 10)}},
	}
	first, _ := l.want(thread, item, "output", false)
	ref, _ := refNamed(item.Header, "output")
	foreign := l.wantRef(state.ThreadKey{Endpoint: "local", Thread: "t2"}, item.Key, ref, false)
	if foreign == first {
		t.Fatal("want across threads reuses the first body")
	}
	if _, absent := l.want(thread, item, "outputSpill", false); absent {
		t.Fatal("want of an absent field hits")
	}
}

func TestLoaderPromoteRestartsLargeBodies(t *testing.T) {
	l := newLoader()
	l.begin(state.Snapshot{}.View())
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	w := l.wantRef(thread, state.ItemKey{Turn: "u1", Item: "i1"}, loaderRef("output", 300000), false)
	l.consume(w, state.ContentPage{Total: 300000, Next: 240000, Data: make([]byte, 5536)})
	if w.next != 240000 {
		t.Fatalf("partial next reads %d, want 240000", w.next)
	}
	space := cap(w.data)
	before := l.resident
	l.promote(w)
	if !w.full || w.start != 0 || w.next != 0 || w.asked != 0 {
		t.Fatalf("promote leaves full %v start %d next %d asked %d, want true 0 0 0", w.full, w.start, w.next, w.asked)
	}
	if w.data != nil {
		t.Fatalf("promote keeps %d bytes, want none", len(w.data))
	}
	if l.resident != before-space {
		t.Fatalf("resident reads %d, want %d", l.resident, before-space)
	}
}

func TestLoaderFullWantCompletesFromStart(t *testing.T) {
	l := newLoader()
	l.begin(state.Snapshot{}.View())
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	small := l.wantRef(thread, state.ItemKey{Turn: "u1", Item: "i1"}, loaderRef("output", 100), true)
	if !small.full {
		t.Fatal("full want of a small body leaves it partial")
	}
	fresh := l.wantRef(thread, state.ItemKey{Turn: "u1", Item: "i2"}, loaderRef("big", 300000), true)
	if !fresh.full || fresh.start != 0 || fresh.next != 0 {
		t.Fatalf("full want leaves full %v start %d next %d, want true 0 0", fresh.full, fresh.start, fresh.next)
	}
}

func TestLoaderRetryResubmitsAtNext(t *testing.T) {
	l := newLoader()
	l.begin(state.Snapshot{}.View())
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	key := state.ItemKey{Turn: "u1", Item: "i1"}
	b := l.wantRef(thread, key, loaderRef("output", 10), false)
	l.consume(b, state.ContentPage{Total: 10, Next: 6, Data: []byte("abcdef")})
	b.err = errors.New("daemon blew up")
	var got []state.Intent
	l.retry(b, func(i state.Intent) { got = append(got, i) })
	if b.err != nil {
		t.Fatalf("retry keeps %v, want nil", b.err)
	}
	if b.asked != l.epoch {
		t.Fatalf("retry asks at %d, want epoch %d", b.asked, l.epoch)
	}
	if len(got) != 1 {
		t.Fatalf("retry submits %d intents, want 1", len(got))
	}
	read, ok := got[0].(state.ReadContent)
	if !ok {
		t.Fatalf("retry submits %T, want ReadContent", got[0])
	}
	if read.Offset != 6 || read.ContentID != "content-output" || read.Item != key || read.Thread != thread {
		t.Fatalf("retry submits %+v, want offset 6 of content-output", read)
	}
}

func TestLoaderPumpAsksForMissingPages(t *testing.T) {
	l := newLoader()
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	link := client.Link{Endpoint: "local", Generation: 1}
	view := loaderView(thread, link, state.TurnView{ID: "u1", Items: []state.ItemView{loaderItem("u1", "i1", "output", 10)}})
	l.begin(view)
	b, _ := l.want(thread, loaderItem("u1", "i1", "output", 10), "output", false)
	var got []state.Intent
	submit := func(i state.Intent) { got = append(got, i) }
	if !l.pump(view, submit) {
		t.Fatal("pump with a missing page reports no work")
	}
	if len(got) != 1 {
		t.Fatalf("pump submits %d intents, want 1", len(got))
	}
	read, ok := got[0].(state.ReadContent)
	if !ok || read.Offset != 0 || read.ContentID != "content-output" {
		t.Fatalf("pump submits %+v, want ReadContent at offset 0", got[0])
	}
	if b.asked != 1 {
		t.Fatalf("body asks at %d, want epoch 1", b.asked)
	}
	if !l.pump(view, submit) {
		t.Fatal("pump with a recent ask reports no work")
	}
	if len(got) != 1 {
		t.Fatalf("pump submits %d intents while the ask settles, want 1", len(got))
	}
}

func TestLoaderPumpAdmitsFourReadsInFlight(t *testing.T) {
	l := newLoader()
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	link := client.Link{Endpoint: "local", Generation: 1}
	items := []state.ItemView{
		loaderItem("u1", "i1", "output", 10),
		loaderItem("u1", "i2", "output", 10),
		loaderItem("u1", "i3", "output", 10),
		loaderItem("u1", "i4", "output", 10),
		loaderItem("u1", "i5", "output", 10),
	}
	view := loaderView(thread, link, state.TurnView{ID: "u1", Items: items})
	l.begin(view)
	bodies := make([]*body, 0, 5)
	for _, it := range items {
		b, ok := l.want(thread, it, "output", false)
		if !ok {
			t.Fatal("want of a recorded field misses")
		}
		bodies = append(bodies, b)
	}
	var got []state.Intent
	if !l.pump(view, func(i state.Intent) { got = append(got, i) }) {
		t.Fatal("pump with missing pages reports no work")
	}
	if len(got) != 4 {
		t.Fatalf("pump submits %d intents, want 4", len(got))
	}
	for _, b := range bodies[:4] {
		if b.asked != 1 {
			t.Fatalf("admitted body asks at %d, want epoch 1", b.asked)
		}
	}
	if bodies[4].asked != 0 {
		t.Fatalf("fifth body asks at %d, want 0", bodies[4].asked)
	}
}

func TestLoaderPumpWaitsForSettleEpochs(t *testing.T) {
	l := newLoader()
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	link := client.Link{Endpoint: "local", Generation: 1}
	item := loaderItem("u1", "i1", "output", 10)
	turn := state.TurnView{ID: "u1", Items: []state.ItemView{item}}
	var got []state.Intent
	submit := func(i state.Intent) { got = append(got, i) }
	first := loaderView(thread, link, turn)
	l.begin(first)
	l.want(thread, item, "output", false)
	l.pump(first, submit)
	l.pump(first, submit)
	second := loaderView(thread, link, turn)
	l.begin(second)
	l.want(thread, item, "output", false)
	l.pump(second, submit)
	third := loaderView(thread, link, turn)
	l.begin(third)
	l.want(thread, item, "output", false)
	l.pump(third, submit)
	if len(got) != 2 {
		t.Fatalf("pump submits %d intents across settle epochs, want 2", len(got))
	}
	for _, i := range got {
		read, ok := i.(state.ReadContent)
		if !ok || read.Offset != 0 {
			t.Fatalf("pump submits %+v, want ReadContent at offset 0", i)
		}
	}
}

func TestLoaderReceiveIgnoresLoadingPages(t *testing.T) {
	l := newLoader()
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	link := client.Link{Endpoint: "local", Generation: 1}
	item := loaderItem("u1", "i1", "output", 10)
	view := loaderView(thread, link, state.TurnView{ID: "u1", Items: []state.ItemView{item}})
	l.begin(view)
	b, _ := l.want(thread, item, "output", false)
	key := state.ContentKey{Item: item.Key, ContentID: "content-output", Offset: 0}
	var got []state.Intent
	submit := func(i state.Intent) { got = append(got, i) }
	if !l.receive(b, key, state.ContentPage{Loading: true}, submit) {
		t.Fatal("receive of a loading page reports no work")
	}
	if len(got) != 0 || b.err != nil || b.next != 0 {
		t.Fatalf("loading page submits %d intents with err %v next %d, want 0 nil 0", len(got), b.err, b.next)
	}
}

func TestLoaderReceiveLoadsDataPages(t *testing.T) {
	l := newLoader()
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	link := client.Link{Endpoint: "local", Generation: 1}
	item := loaderItem("u1", "i1", "output", 10)
	view := loaderView(thread, link, state.TurnView{ID: "u1", Items: []state.ItemView{item}})
	l.begin(view)
	b, _ := l.want(thread, item, "output", false)
	key := state.ContentKey{Item: item.Key, ContentID: "content-output", Offset: 0}
	var got []state.Intent
	submit := func(i state.Intent) { got = append(got, i) }
	if !l.receive(b, key, state.ContentPage{Total: 10, Next: 4, Data: []byte("abcd")}, submit) {
		t.Fatal("receive of a data page reports no work")
	}
	if b.next != 4 {
		t.Fatalf("body holds next %d, want 4", b.next)
	}
	if string(b.data) != "abcd" {
		t.Fatalf("body holds %q, want abcd", b.data)
	}
	if len(got) != 1 {
		t.Fatalf("receive submits %d intents, want 1", len(got))
	}
	release, ok := got[0].(state.ReleaseContentPage)
	if !ok || release.Key != key || release.Thread != thread {
		t.Fatalf("receive submits %+v, want ReleaseContentPage", got[0])
	}
}

func TestLoaderReceiveDefersAskedEpochErrors(t *testing.T) {
	l := newLoader()
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	link := client.Link{Endpoint: "local", Generation: 1}
	item := loaderItem("u1", "i1", "output", 10)
	view := loaderView(thread, link, state.TurnView{ID: "u1", Items: []state.ItemView{item}})
	l.begin(view)
	b, _ := l.want(thread, item, "output", false)
	key := state.ContentKey{Item: item.Key, ContentID: "content-output", Offset: 0}
	var got []state.Intent
	submit := func(i state.Intent) { got = append(got, i) }
	b.asked = l.epoch
	failed := errors.New("daemon blew up")
	if !l.receive(b, key, state.ContentPage{Err: failed}, submit) {
		t.Fatal("receive of an error at the asked epoch reports no work")
	}
	if b.err != nil {
		t.Fatalf("asked-epoch error sets %v, want nil", b.err)
	}
	if len(got) != 0 {
		t.Fatalf("asked-epoch error submits %d intents, want 0", len(got))
	}
}

func TestLoaderReceiveRecordsSettledErrors(t *testing.T) {
	l := newLoader()
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	link := client.Link{Endpoint: "local", Generation: 1}
	item := loaderItem("u1", "i1", "output", 10)
	view := loaderView(thread, link, state.TurnView{ID: "u1", Items: []state.ItemView{item}})
	l.begin(view)
	b, _ := l.want(thread, item, "output", false)
	key := state.ContentKey{Item: item.Key, ContentID: "content-output", Offset: 0}
	var got []state.Intent
	submit := func(i state.Intent) { got = append(got, i) }
	if !l.receive(b, key, state.ContentPage{Total: 10, Next: 4, Data: []byte("abcd")}, submit) {
		t.Fatal("receive of a data page reports no work")
	}
	failed := errors.New("daemon blew up")
	if l.receive(b, key, state.ContentPage{Err: failed}, submit) {
		t.Fatal("receive of a settled error reports work")
	}
	if !errors.Is(b.err, failed) {
		t.Fatalf("settled error sets %v, want the daemon error", b.err)
	}
	if len(got) != 2 {
		t.Fatalf("settled error submits %d intents, want 2", len(got))
	}
}

func TestLoaderPumpSkipsSettledBodies(t *testing.T) {
	l := newLoader()
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	link := client.Link{Endpoint: "local", Generation: 1}
	badItem := loaderItem("u1", "i1", "output", 10)
	doneItem := loaderItem("u1", "i2", "output", 4)
	view := loaderView(thread, link, state.TurnView{ID: "u1", Items: []state.ItemView{badItem, doneItem}})
	l.begin(view)
	bad, _ := l.want(thread, badItem, "output", false)
	bad.err = errors.New("daemon blew up")
	done, _ := l.want(thread, doneItem, "output", false)
	l.consume(done, state.ContentPage{Total: 4, Next: 4, Data: []byte("done")})
	if l.pump(view, func(state.Intent) {}) {
		t.Fatal("pump with only failed and complete bodies reports work")
	}
}

func TestLoaderVictimsPreferStaleBodies(t *testing.T) {
	l := newLoader()
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	view := loaderView(thread, client.Link{Endpoint: "local", Generation: 1})
	ref := loaderRef("output", 3)
	l.begin(view)
	first := l.wantRef(thread, state.ItemKey{Turn: "u1", Item: "i1"}, ref, false)
	l.consume(first, state.ContentPage{Total: 3, Next: 3, Data: []byte("abc")})
	l.begin(view)
	second := l.wantRef(thread, state.ItemKey{Turn: "u1", Item: "i2"}, ref, false)
	l.consume(second, state.ContentPage{Total: 3, Next: 3, Data: []byte("def")})
	l.begin(view)
	third := l.wantRef(thread, state.ItemKey{Turn: "u1", Item: "i3"}, ref, false)
	l.consume(third, state.ContentPage{Total: 3, Next: 3, Data: []byte("ghi")})
	if got := l.unusedVictim(); got != first {
		t.Fatal("stale victim is not the stalest body")
	}
	fresh := newLoader()
	fresh.begin(view)
	only := fresh.wantRef(thread, state.ItemKey{Turn: "u1", Item: "i1"}, ref, false)
	fresh.consume(only, state.ContentPage{Total: 3, Next: 3, Data: []byte("abc")})
	if got := fresh.unusedVictim(); got != nil {
		t.Fatal("current body reads as a stale victim")
	}
}

func TestLoaderEvictRestoresResident(t *testing.T) {
	t.Run("keeps", func(t *testing.T) { checkEvictKeepsNewer(t) })
	t.Run("restores", func(t *testing.T) { checkEvictRestoresCounts(t) })
}

type evictFixture struct {
	l            *loader
	first        *body
	second       *body
	third        *body
	roomB, roomC int
}

func seedEvictFixture(t *testing.T) evictFixture {
	t.Helper()
	l := newLoader()
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	view := loaderView(thread, client.Link{Endpoint: "local", Generation: 1})
	ref := loaderRef("output", 3)
	l.begin(view)
	first := l.wantRef(thread, state.ItemKey{Turn: "u1", Item: "i1"}, ref, false)
	l.consume(first, state.ContentPage{Total: 3, Next: 3, Data: []byte("abc")})
	l.begin(view)
	second := l.wantRef(thread, state.ItemKey{Turn: "u1", Item: "i2"}, ref, false)
	l.consume(second, state.ContentPage{Total: 3, Next: 3, Data: []byte("def")})
	l.begin(view)
	third := l.wantRef(thread, state.ItemKey{Turn: "u1", Item: "i3"}, ref, false)
	l.consume(third, state.ContentPage{Total: 3, Next: 3, Data: []byte("ghi")})
	roomB, roomC := cap(second.data), cap(third.data)
	if !l.overBudget() {
		l.presented = 32 << 20
	}
	if !l.overBudget() {
		t.Fatal("resident and a full window read within budget")
	}
	l.presented = 32<<20 - roomB - roomC
	l.evict()
	return evictFixture{l: l, first: first, second: second, third: third, roomB: roomB, roomC: roomC}
}

func checkEvictKeepsNewer(t *testing.T) {
	t.Helper()
	f := seedEvictFixture(t)
	if _, ok := f.l.bodies[f.first.key]; ok {
		t.Fatal("evict keeps the stalest body")
	}
	if _, ok := f.l.bodies[f.second.key]; !ok {
		t.Fatal("evict drops a newer body")
	}
	if _, ok := f.l.bodies[f.third.key]; !ok {
		t.Fatal("evict drops the current body")
	}
}

func checkEvictRestoresCounts(t *testing.T) {
	t.Helper()
	f := seedEvictFixture(t)
	if f.l.resident != f.roomB+f.roomC {
		t.Fatalf("resident reads %d, want %d", f.l.resident, f.roomB+f.roomC)
	}
	if f.first.data != nil || f.first.next != f.first.start || f.first.asked != 0 {
		t.Fatalf("evicted body keeps next %d asked %d, want start 0", f.first.next, f.first.asked)
	}
}

func TestLiveTailPageReadsRecordedSlice(t *testing.T) {
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	link := client.Link{Endpoint: "local", Generation: 1}
	key := state.ItemKey{Turn: "u1", Item: "i1"}
	cases := []struct {
		name   string
		offset uint64
		limit  uint64
		want   string
		fail   bool
	}{
		{"full", 0, 11, "hello world", false},
		{"slice", 2, 5, "llo w", false},
		{"clamp", 9, 10, "ld", false},
		{"empty", 0, 0, "", false},
		{"past end", 11, 5, "", true},
		{"past tail", 12, 1, "", true},
	}
	for _, c := range cases {
		item := tailItem("u1", "i1", "output", "hello world", []rpc.ContentReference{loaderRef("output", 11)})
		tv, _ := loaderView(thread, link, state.TurnView{ID: "u1", Items: []state.ItemView{item}}).Thread(thread)
		got, err := captureTail(item).page(tv, key, c.offset, c.limit)
		if c.fail {
			if !errors.Is(err, errLiveSourceChanged) {
				t.Errorf("%s: err reads %v, want errLiveSourceChanged", c.name, err)
			}
			continue
		}
		if err != nil {
			t.Errorf("%s: page returns %v, want nil", c.name, err)
			continue
		}
		if string(got) != c.want {
			t.Errorf("%s: page reads %q, want %q", c.name, got, c.want)
		}
	}
}

func TestLiveTailPageRejectsChangedSource(t *testing.T) {
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	link := client.Link{Endpoint: "local", Generation: 1}
	key := state.ItemKey{Turn: "u1", Item: "i1"}
	cases := []struct {
		name   string
		mutate func(*state.ItemView)
		view   bool
	}{
		{"revision", func(it *state.ItemView) { it.Header.ItemRevision = "rev-other" }, true},
		{"field", func(it *state.ItemView) { it.Field = "error" }, true},
		{"shrunk", func(it *state.ItemView) { it.Tail = []byte("hello") }, true},
		{"regrown", func(it *state.ItemView) { it.Tail = []byte("hello world!") }, true},
		{"wrong key", func(it *state.ItemView) { it.Key = state.ItemKey{Turn: "u1", Item: "i9"} }, true},
		{"missing turn", func(*state.ItemView) {}, false},
	}
	for _, c := range cases {
		base := tailItem("u1", "i1", "output", "hello world", []rpc.ContentReference{loaderRef("output", 11)})
		shown := base
		c.mutate(&shown)
		var tv *state.ThreadView
		if c.view {
			tv, _ = loaderView(thread, link, state.TurnView{ID: "u1", Items: []state.ItemView{shown}}).Thread(thread)
		} else {
			tv, _ = loaderView(thread, link, state.TurnView{ID: "u9", Items: []state.ItemView{shown}}).Thread(thread)
		}
		if _, err := captureTail(base).page(tv, key, 0, 11); !errors.Is(err, errLiveSourceChanged) {
			t.Errorf("%s: err reads %v, want errLiveSourceChanged", c.name, err)
		}
	}
}

func TestPageRequestMatchesRecordedContents(t *testing.T) {
	ref := loaderRef("output", 10)
	key := state.ItemKey{Turn: "u1", Item: "i1"}
	with := &state.ThreadView{Turns: []state.TurnView{{ID: "u1", Items: []state.ItemView{tailItem("u1", "i1", "output", "hello", []rpc.ContentReference{ref})}}}}
	req := pageRequest{thread: state.ThreadKey{Endpoint: "local", Thread: "t1"}, item: key, ref: ref}
	if !req.matches(with) {
		t.Fatal("request with a recorded reference misses")
	}
	without := &state.ThreadView{Turns: []state.TurnView{{ID: "u1", Items: []state.ItemView{tailItem("u1", "i1", "output", "hello", nil)}}}}
	if req.matches(without) {
		t.Fatal("request with a dropped reference matches")
	}
	empty := &state.ThreadView{}
	if req.matches(empty) {
		t.Fatal("request with no turns matches")
	}
}

type pageStepSetup struct {
	view *state.View
	req  pageRequest
}

type pageStepCase struct {
	name        string
	build       func() pageStepSetup
	epoch       uint64
	wantData    string
	wantErr     error
	wantOK      bool
	wantSubmits int
	wantAsked   uint64
}

func checkPageRequestStep(t *testing.T, c pageStepCase) {
	t.Helper()
	s := c.build()
	var got []state.Intent
	res, ok := s.req.step(s.view, c.epoch, func(i state.Intent) { got = append(got, i) })
	if res.data != c.wantData {
		t.Errorf("%s: data reads %q, want %q", c.name, res.data, c.wantData)
	}
	checkPageRequestStepErr(t, c, res.err)
	if ok != c.wantOK {
		t.Errorf("%s: ok reads %v, want %v", c.name, ok, c.wantOK)
	}
	if len(got) != c.wantSubmits {
		t.Errorf("%s: submits %d intents, want %d", c.name, len(got), c.wantSubmits)
	}
	if s.req.asked != c.wantAsked {
		t.Errorf("%s: asked reads %d, want %d", c.name, s.req.asked, c.wantAsked)
	}
}

type pageStepErr struct {
	name    string
	wantErr error
	gotErr  error
}

func checkPageRequestStepErr(t *testing.T, c pageStepCase, got error) {
	t.Helper()
	in := pageStepErr{name: c.name, wantErr: c.wantErr, gotErr: got}
	checkPageStepErrValue(t, in)
}

func checkPageStepErrValue(t *testing.T, in pageStepErr) {
	t.Helper()
	if in.wantErr == nil {
		if in.gotErr != nil {
			t.Errorf("%s: err reads %v, want nil", in.name, in.gotErr)
		}
		return
	}
	if !errors.Is(in.gotErr, in.wantErr) {
		t.Errorf("%s: err reads %v, want %v", in.name, in.gotErr, in.wantErr)
	}
}

func TestPageRequestStepRoutesLiveAndRecorded(t *testing.T) {
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	link := client.Link{Endpoint: "local", Generation: 1}
	ref := loaderRef("output", 100)
	key := state.ItemKey{Turn: "u1", Item: "i1"}
	tail := "hello world"
	cases := []pageStepCase{
		{
			"missing thread",
			func() pageStepSetup {
				return pageStepSetup{loaderView(state.ThreadKey{Endpoint: "local", Thread: "t9"}, link), pageRequest{thread: thread, item: key, ref: ref}}
			}, 9, "", errThreadClosed, true, 0, 0,
		},
		{
			"stale link",
			func() pageStepSetup {
				item := tailItem("u1", "i1", "output", tail, []rpc.ContentReference{ref})
				return pageStepSetup{
					loaderView(thread, client.Link{Endpoint: "local", Generation: 2}, state.TurnView{ID: "u1", Items: []state.ItemView{item}}),
					pageRequest{thread: thread, item: key, ref: ref, asked: 1, link: link},
				}
			}, 9, "", errThreadClosed, true, 0, 1,
		},
		{
			"live hit",
			func() pageStepSetup {
				item := tailItem("u1", "i1", "output", tail, []rpc.ContentReference{ref})
				view := loaderView(thread, link, state.TurnView{ID: "u1", Items: []state.ItemView{item}})
				return pageStepSetup{view, pageRequest{thread: thread, item: key, ref: ref, live: captureTail(item), offset: 2, limit: 5}}
			}, 9, "llo w", nil, true, 0, 0,
		},
		{
			"live stale",
			func() pageStepSetup {
				kept := tailItem("u1", "i1", "output", tail, []rpc.ContentReference{ref})
				shown := kept
				shown.Header.ItemRevision = "rev-other"
				view := loaderView(thread, link, state.TurnView{ID: "u1", Items: []state.ItemView{shown}})
				return pageStepSetup{view, pageRequest{thread: thread, item: key, ref: ref, live: captureTail(kept), offset: 0, limit: 11}}
			}, 9, "", errLiveSourceChanged, true, 0, 0,
		},
		{
			"recorded ask",
			func() pageStepSetup {
				item := tailItem("u1", "i1", "output", "", []rpc.ContentReference{ref})
				return pageStepSetup{
					loaderView(thread, link, state.TurnView{ID: "u1", Items: []state.ItemView{item}}),
					pageRequest{thread: thread, item: key, ref: ref, offset: 4, limit: 10},
				}
			}, 7, "", nil, false, 1, 7,
		},
		{
			"recent ask",
			func() pageStepSetup {
				item := tailItem("u1", "i1", "output", "", []rpc.ContentReference{ref})
				return pageStepSetup{
					loaderView(thread, link, state.TurnView{ID: "u1", Items: []state.ItemView{item}}),
					pageRequest{thread: thread, item: key, ref: ref, offset: 4, limit: 10, asked: 7, link: link},
				}
			}, 8, "", nil, false, 0, 7,
		},
		{
			"abandoned",
			func() pageStepSetup {
				item := tailItem("u1", "i1", "output", "", []rpc.ContentReference{ref})
				return pageStepSetup{
					loaderView(thread, link, state.TurnView{ID: "u1", Items: []state.ItemView{item}}),
					pageRequest{thread: thread, item: key, ref: ref, offset: 4, limit: 10, asked: 5, link: link, abandoned: true},
				}
			}, 8, "", errThreadClosed, true, 0, 5,
		},
		{
			"recorded changed",
			func() pageStepSetup {
				item := tailItem("u1", "i1", "output", "", nil)
				return pageStepSetup{
					loaderView(thread, link, state.TurnView{ID: "u1", Items: []state.ItemView{item}}),
					pageRequest{thread: thread, item: key, ref: ref, offset: 4, limit: 10, asked: 5, link: link},
				}
			}, 8, "", errRecordedSourceChanged, true, 0, 5,
		},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) { checkPageRequestStep(t, c) })
	}
}

func TestBridgeStepAsksRecordedSource(t *testing.T) {
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	link := client.Link{Endpoint: "local", Generation: 1}
	ref := loaderRef("output", 100)
	key := state.ItemKey{Turn: "u1", Item: "i1"}
	item := tailItem("u1", "i1", "output", "", []rpc.ContentReference{ref})
	view := loaderView(thread, link, state.TurnView{ID: "u1", Items: []state.ItemView{item}})
	req := pageRequest{thread: thread, item: key, ref: ref, offset: 4, limit: 10}
	var got []state.Intent
	res, ok := req.step(view, 7, func(i state.Intent) { got = append(got, i) })
	if ok || res.data != "" || res.err != nil {
		t.Fatalf("ask returns %+v %v, want empty false", res, ok)
	}
	if len(got) != 1 {
		t.Fatalf("ask submits %d intents, want 1", len(got))
	}
	read, isRead := got[0].(state.ReadContent)
	if !isRead {
		t.Fatalf("ask submits %T, want ReadContent", got[0])
	}
	want := state.ReadContent{Thread: thread, Item: key, ContentID: "content-output", Offset: 4}
	if read != want {
		t.Fatalf("ask submits %+v, want %+v", read, want)
	}
	if req.asked != 7 || req.link != link {
		t.Fatalf("ask leaves asked %d link %+v, want 7 the thread link", req.asked, req.link)
	}
}

type pageConsumeCase struct {
	name        string
	contents    bool
	asked       uint64
	abandoned   bool
	page        state.ContentPage
	limit       uint64
	wantData    string
	wantErr     error
	wantOK      bool
	wantSubmits int
}

type pageConsumeInput struct {
	name        string
	contents    []rpc.ContentReference
	asked       uint64
	abandoned   bool
	page        state.ContentPage
	limit       uint64
	wantData    string
	wantErr     error
	wantOK      bool
	wantSubmits int
}

type pageConsumeHarness struct {
	thread  state.ThreadKey
	ref     rpc.ContentReference
	key     state.ItemKey
	pageKey state.ContentKey
}

func checkPageRequestConsume(t *testing.T, h pageConsumeHarness, c pageConsumeCase) {
	t.Helper()
	var contents []rpc.ContentReference
	if c.contents {
		contents = []rpc.ContentReference{h.ref}
	}
	in := pageConsumeInput{name: c.name, contents: contents, asked: c.asked, abandoned: c.abandoned, page: c.page, limit: c.limit, wantData: c.wantData, wantErr: c.wantErr, wantOK: c.wantOK, wantSubmits: c.wantSubmits}
	runPageConsumeCase(t, h, in)
}

func runPageConsumeCase(t *testing.T, h pageConsumeHarness, in pageConsumeInput) {
	t.Helper()
	tv := &state.ThreadView{Turns: []state.TurnView{{ID: "u1", Items: []state.ItemView{tailItem("u1", "i1", "output", "", in.contents)}}}}
	req := pageRequest{thread: h.thread, item: h.key, ref: h.ref, offset: 0, limit: in.limit, asked: in.asked, abandoned: in.abandoned}
	var got []state.Intent
	res, ok := req.consume(tv, h.pageKey, in.page, func(i state.Intent) { got = append(got, i) })
	if res.data != in.wantData {
		t.Errorf("%s: data reads %q, want %q", in.name, res.data, in.wantData)
	}
	checkPageConsumeErr(t, in, res.err)
	if ok != in.wantOK {
		t.Errorf("%s: ok reads %v, want %v", in.name, ok, in.wantOK)
	}
	if len(got) != in.wantSubmits {
		t.Errorf("%s: submits %d intents, want %d", in.name, len(got), in.wantSubmits)
	}
}

func checkPageConsumeErr(t *testing.T, in pageConsumeInput, got error) {
	t.Helper()
	if in.wantErr == nil {
		if got != nil {
			t.Errorf("%s: err reads %v, want nil", in.name, got)
		}
		return
	}
	if !errors.Is(got, in.wantErr) {
		t.Errorf("%s: err reads %v, want %v", in.name, got, in.wantErr)
	}
}

func TestPageRequestConsumeDeliversSettledPages(t *testing.T) {
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	ref := loaderRef("output", 10)
	key := state.ItemKey{Turn: "u1", Item: "i1"}
	pageKey := state.ContentKey{Item: key, ContentID: "content-output", Offset: 0}
	failed := errors.New("daemon blew up")
	cases := []pageConsumeCase{
		{"loading", true, 0, false, state.ContentPage{Loading: true}, 10, "", nil, false, 0},
		{"abandoned", true, 2, true, state.ContentPage{Total: 10, Next: 10, Data: []byte("0123456789")}, 10, "", nil, true, 1},
		{"changed", false, 2, false, state.ContentPage{Total: 10, Next: 10, Data: []byte("0123456789")}, 10, "", errRecordedSourceChanged, true, 1},
		{"error before ask", true, 0, false, state.ContentPage{Err: failed}, 10, "", nil, false, 1},
		{"error after ask", true, 2, false, state.ContentPage{Err: failed}, 10, "", failed, true, 1},
		{"truncated", true, 2, false, state.ContentPage{Total: 10, Next: 10, Data: []byte("0123456789")}, 4, "0123", nil, true, 1},
	}
	for _, c := range cases {
		h := pageConsumeHarness{thread: thread, ref: ref, key: key, pageKey: pageKey}
		t.Run(c.name, func(t *testing.T) { checkPageRequestConsume(t, h, c) })
	}
}

func TestPageRequestRecentSettlesEpochs(t *testing.T) {
	cases := []struct {
		asked uint64
		epoch uint64
		want  bool
	}{
		{0, 5, false},
		{5, 5, true},
		{5, 6, true},
		{5, 7, false},
		{5, 4, false},
	}
	for _, c := range cases {
		req := pageRequest{asked: c.asked}
		if got := req.recent(c.epoch); got != c.want {
			t.Errorf("asked %d at epoch %d: recent reads %v, want %v", c.asked, c.epoch, got, c.want)
		}
	}
}

func TestBridgeFetchServesPagesOnce(t *testing.T) {
	b := &bridge{}
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	if b.waiting() {
		t.Fatal("empty bridge reports waiting requests")
	}
	view := loaderView(state.ThreadKey{Endpoint: "local", Thread: "t9"}, client.Link{Endpoint: "local", Generation: 1})
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	got := make(chan pageResult, 1)
	req := pageRequest{thread: thread, item: state.ItemKey{Turn: "u1", Item: "i1"}, ref: loaderRef("output", 10), limit: 10}
	go func() { got <- b.fetch(ctx, req) }()
	start := time.Now()
	for !b.waiting() {
		if time.Since(start) > 5*time.Second {
			t.Fatal("fetch did not queue its request")
		}
		time.Sleep(time.Millisecond)
	}
	b.serve(view, 9, func(state.Intent) {})
	select {
	case res := <-got:
		if !errors.Is(res.err, errThreadClosed) {
			t.Fatalf("fetch returns %v, want errThreadClosed", res.err)
		}
	case <-ctx.Done():
		t.Fatal("fetch did not return after serve")
	}
	if b.waiting() {
		t.Fatal("bridge still waits after serving its request")
	}
}

func TestBridgeFetchCancelDropsQuietRequest(t *testing.T) {
	b := &bridge{}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	got := make(chan pageResult, 1)
	req := pageRequest{thread: state.ThreadKey{Endpoint: "local", Thread: "t1"}, limit: 10}
	go func() { got <- b.fetch(ctx, req) }()
	start := time.Now()
	for !b.waiting() {
		if time.Since(start) > 5*time.Second {
			t.Fatal("fetch did not queue its request")
		}
		time.Sleep(time.Millisecond)
	}
	cancel()
	res := <-got
	if !errors.Is(res.err, context.Canceled) {
		t.Fatalf("fetch returns %v, want context.Canceled", res.err)
	}
	if b.waiting() {
		t.Fatal("bridge still waits after a canceled fetch")
	}
}

func TestBridgeDropRemovesQuietRequests(t *testing.T) {
	b := &bridge{}
	fresh := pageRequest{done: make(chan pageResult, 1)}
	b.pending = []*pageRequest{&fresh}
	b.drop(&fresh)
	if len(b.pending) != 0 {
		t.Fatalf("drop keeps %d requests, want 0", len(b.pending))
	}
	asked := pageRequest{asked: 3, done: make(chan pageResult, 1)}
	b.pending = []*pageRequest{&asked}
	b.drop(&asked)
	if len(b.pending) != 1 || !asked.abandoned {
		t.Fatalf("drop leaves %d requests abandoned %v, want 1 true", len(b.pending), asked.abandoned)
	}
	b.drop(&pageRequest{})
	if len(b.pending) != 1 {
		t.Fatalf("drop of an unknown request leaves %d requests, want 1", len(b.pending))
	}
}

func TestBridgeAbortFailsBlockedFetches(t *testing.T) {
	b := &bridge{}
	first := pageRequest{done: make(chan pageResult, 1)}
	second := pageRequest{asked: 3, done: make(chan pageResult, 1)}
	b.pending = []*pageRequest{&first, &second}
	b.abort()
	if !first.abandoned || !second.abandoned {
		t.Fatal("abort leaves a pending request unmarked")
	}
	if res := <-first.done; !errors.Is(res.err, errThreadClosed) {
		t.Fatalf("abort delivers %v, want errThreadClosed", res.err)
	}
	if res := <-second.done; !errors.Is(res.err, errThreadClosed) {
		t.Fatalf("abort delivers %v, want errThreadClosed", res.err)
	}
	if len(b.pending) != 1 || b.pending[0] != &second {
		t.Fatal("abort does not keep only the asked request")
	}
	if !b.waiting() {
		t.Fatal("bridge reports no wait with an asked request held")
	}
}

func TestBridgeServeIgnoresEmptyQueue(t *testing.T) {
	b := &bridge{}
	b.serve(state.Snapshot{}.View(), 1, func(state.Intent) {
		t.Error("serve submitted an intent with no pending request")
	})
	if b.waiting() {
		t.Error("empty bridge reports waiting requests")
	}
}

func TestBridgeServeDropsAbandonedQuietly(t *testing.T) {
	b := &bridge{}
	held := pageRequest{thread: state.ThreadKey{Endpoint: "local", Thread: "t1"}, asked: 3, abandoned: true, done: make(chan pageResult, 1)}
	b.pending = []*pageRequest{&held}
	view := loaderView(state.ThreadKey{Endpoint: "local", Thread: "t9"}, client.Link{Endpoint: "local", Generation: 1})
	b.serve(view, 9, func(state.Intent) {
		t.Error("serve submitted an intent for an abandoned request")
	})
	if b.waiting() {
		t.Fatal("bridge still waits after serving its abandoned request")
	}
	select {
	case res := <-held.done:
		t.Fatalf("serve delivers %+v for an abandoned request, want nothing", res)
	default:
	}
}

func TestLoaderPromoteRejectsOverBudgetBody(t *testing.T) {
	over := uint64(32<<20 + 1)
	l := newLoader()
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	link := client.Link{Endpoint: "local", Generation: 1}
	item := loaderItem("u1", "i1", "output", over)
	view := loaderView(thread, link, state.TurnView{ID: "u1", Items: []state.ItemView{item}})
	l.begin(view)
	b := l.wantRef(thread, item.Key, loaderRef("output", over), false)
	key := state.ContentKey{Item: item.Key, ContentID: "content-output", Offset: b.next}
	l.receive(b, key, state.ContentPage{Total: over, Next: b.next + 5, Data: []byte("hello")}, func(state.Intent) {})
	l.promote(b)
	if !errors.Is(b.err, errSourceBudget) {
		t.Fatalf("promote err reads %v, want errSourceBudget", b.err)
	}
	var got []state.Intent
	l.pump(view, func(i state.Intent) { got = append(got, i) })
	if len(got) != 0 {
		t.Fatalf("pump submits %d intents, want 0", len(got))
	}
}

func TestLoaderPromoteRestartsSmallTailBody(t *testing.T) {
	small := uint64(300000)
	l := newLoader()
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	link := client.Link{Endpoint: "local", Generation: 1}
	item := loaderItem("u1", "i1", "output", small)
	view := loaderView(thread, link, state.TurnView{ID: "u1", Items: []state.ItemView{item}})
	l.begin(view)
	b := l.wantRef(thread, item.Key, loaderRef("output", small), false)
	key := state.ContentKey{Item: item.Key, ContentID: "content-output", Offset: b.next}
	l.receive(b, key, state.ContentPage{Total: small, Next: b.next + 5, Data: []byte("hello")}, func(state.Intent) {})
	l.promote(b)
	if b.err != nil {
		t.Fatalf("promote err reads %v, want nil", b.err)
	}
	if !b.full || b.start != 0 || b.next != 0 {
		t.Fatalf("promote leaves full %v start %d next %d, want true 0 0", b.full, b.start, b.next)
	}
	var got []state.Intent
	l.pump(view, func(i state.Intent) { got = append(got, i) })
	if len(got) != 1 {
		t.Fatalf("pump submits %d intents, want 1", len(got))
	}
	read, ok := got[0].(state.ReadContent)
	if !ok || read.Offset != 0 {
		t.Fatalf("pump submits %+v, want ReadContent at offset 0", got[0])
	}
}

func loaderWithTwoBodies(t *testing.T) (*loader, *body, *body) {
	t.Helper()
	l := newLoader()
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	view := loaderView(thread, client.Link{Endpoint: "local", Generation: 1})
	ref := loaderRef("output", 3)
	pages := []struct {
		item state.ItemKey
		text string
	}{{state.ItemKey{Turn: "u1", Item: "i1"}, "abc"}, {state.ItemKey{Turn: "u1", Item: "i2"}, "def"}}
	var bodies []*body
	for _, p := range pages {
		l.begin(view)
		b := l.wantRef(thread, p.item, ref, false)
		l.receive(b, state.ContentKey{Item: p.item, ContentID: "content-output"}, state.ContentPage{Total: 3, Next: 3, Data: []byte(p.text)}, func(state.Intent) {})
		bodies = append(bodies, b)
	}
	return l, bodies[0], bodies[1]
}

func TestLoaderUnusedVictimPrefersStaleBody(t *testing.T) {
	l, stale, current := loaderWithTwoBodies(t)
	if got := l.unusedVictim(); got != stale {
		t.Fatal("unused victim is not the stale body")
	}
	if got := l.oldestVictim(); got != stale {
		t.Fatal("oldest victim is not the least recently used body")
	}
	stale.used, current.used = l.clock, l.clock
	if got := l.unusedVictim(); got != nil {
		t.Fatalf("unused victim reads %p, want nil when every body is current", got)
	}
}

func TestLoaderEvictSparesBodyInFlight(t *testing.T) {
	l, free, inFlight := loaderWithTwoBodies(t)
	view := loaderView(state.ThreadKey{Endpoint: "local", Thread: "t1"}, client.Link{Endpoint: "local", Generation: 1})
	l.begin(view)
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	free = l.wantRef(thread, free.key.item, free.ref, false)
	inFlight = l.wantRef(thread, inFlight.key.item, inFlight.ref, false)
	inFlight.asked = l.epoch
	l.presented = residentBudget - cap(inFlight.data)
	if !l.overBudget() {
		t.Fatal("resident source and the window read within budget")
	}
	l.evict()
	if _, ok := l.bodies[free.key]; ok {
		t.Fatal("evict kept the free body while the window is over budget")
	}
	if _, ok := l.bodies[inFlight.key]; !ok {
		t.Fatal("evict dropped the body in flight while a free body remained")
	}
	if l.overBudget() {
		t.Fatalf("resident source is %d bytes over budget after evict", l.resident)
	}
}

func TestLoaderEvictTakesBodyInFlightWhenAlone(t *testing.T) {
	l, spare, only := loaderWithTwoBodies(t)
	l.resident -= cap(spare.data)
	delete(l.bodies, spare.key)
	view := loaderView(state.ThreadKey{Endpoint: "local", Thread: "t1"}, client.Link{Endpoint: "local", Generation: 1})
	l.begin(view)
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	only = l.wantRef(thread, only.key.item, only.ref, false)
	only.asked = l.epoch
	l.presented = 32 << 20
	l.evict()
	if len(l.bodies) != 0 {
		t.Fatalf("evict kept %d bodies over the window budget, want 0", len(l.bodies))
	}
}
