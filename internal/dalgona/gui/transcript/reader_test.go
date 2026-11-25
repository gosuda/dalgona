package transcript

import (
	"errors"
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

type absentCase struct {
	name       string
	item       state.ItemView
	field      string
	wantText   string
	wantAbsent bool
}

func checkAbsentCase(t *testing.T, thread state.ThreadKey, c absentCase) {
	t.Helper()
	l := newLoader()
	r := reader{loader: l, thread: thread, item: c.item}
	got := r.load(c.field, false)
	if got.text != c.wantText {
		t.Fatalf("%s: text reads %q, want %q", c.name, got.text, c.wantText)
	}
	if !got.ready {
		t.Fatalf("%s: ready reads false, want true for an absent field", c.name)
	}
	if got.absent != c.wantAbsent {
		t.Fatalf("%s: absent reads %v, want %v", c.name, got.absent, c.wantAbsent)
	}
	if got.failed != nil {
		t.Fatalf("%s: failed reads %+v, want nil", c.name, got.failed)
	}
	if got.content != nil {
		t.Fatalf("%s: content reads %+v, want nil for an absent field", c.name, got.content)
	}
}

func TestReaderLoadAbsentMarksMissing(t *testing.T) {
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	cases := []absentCase{
		{name: "missing", item: loaderItem("u1", "i1", "output", 10), field: "error", wantText: "", wantAbsent: true},
		{name: "tail", item: tailItem("u1", "i1", "output", "live", nil), field: "output", wantText: "live", wantAbsent: false},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) { checkAbsentCase(t, thread, c) })
	}
}

type readyCase struct {
	name      string
	total     uint64
	data      string
	wantText  string
	wantReady bool
}

func checkReadyCase(t *testing.T, c readyCase) {
	t.Helper()
	l := newLoader()
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	item := loaderItem("u1", "i1", "output", c.total)
	r := reader{loader: l, thread: thread, item: item}
	b, ok := l.want(thread, item, "output", false)
	if !ok {
		t.Fatalf("%s: want misses a recorded field", c.name)
	}
	if c.data != "" {
		l.consume(b, state.ContentPage{Total: c.total, Next: c.total, Data: []byte(c.data)})
	}
	var got fieldText
	if c.wantReady {
		got = r.text("output")
	} else {
		got = r.fullText("output")
	}
	if got.text != c.wantText {
		t.Fatalf("%s: text reads %q, want %q", c.name, got.text, c.wantText)
	}
	if got.ready != c.wantReady {
		t.Fatalf("%s: ready reads %v, want %v", c.name, got.ready, c.wantReady)
	}
	if got.absent {
		t.Fatalf("%s: absent reads true, want false for a recorded field", c.name)
	}
	if got.failed != nil {
		t.Fatalf("%s: failed reads %+v, want nil", c.name, got.failed)
	}
	if got.content != b {
		t.Fatalf("%s: content misses the loader body", c.name)
	}
}

func TestReaderLoadServesCompleteBody(t *testing.T) {
	cases := []readyCase{
		{name: "complete", total: 5, data: "hello", wantText: "hello", wantReady: true},
		{name: "incomplete", total: 11, data: "", wantText: "", wantReady: false},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) { checkReadyCase(t, c) })
	}
}

func TestReaderLoadFailsOnMismatch(t *testing.T) {
	l := newLoader()
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	item := loaderItem("u1", "i1", "output", 10)
	r := reader{loader: l, thread: thread, item: item}
	b, ok := l.want(thread, item, "output", false)
	if !ok {
		t.Fatal("want of a recorded field misses")
	}
	l.consume(b, state.ContentPage{Total: 9, Next: 9})
	if !errors.Is(b.err, errMismatch) {
		t.Fatalf("consume sets %v, want errMismatch", b.err)
	}
	got := r.load("output", false)
	if got.failed != b {
		t.Fatalf("failed reads %+v, want the mismatched body", got.failed)
	}
	if got.ready {
		t.Fatal("mismatched field reads ready")
	}
	if got.text != "" {
		t.Fatalf("mismatched text reads %q, want empty", got.text)
	}
}

func TestReaderTextFullTextPromotesLargeTail(t *testing.T) {
	l := newLoader()
	thread := state.ThreadKey{Endpoint: "local", Thread: "t1"}
	item := loaderItem("u1", "i1", "output", 300000)
	r := reader{loader: l, thread: thread, item: item}
	small := r.text("output")
	if small.content == nil {
		t.Fatal("small read of a large body holds no content")
	}
	if !small.large {
		t.Fatal("large body reads small")
	}
	if small.content.start != 300000-65536 {
		t.Fatalf("tail start reads %d, want 234464", small.content.start)
	}
	full := r.fullText("output")
	if full.content != small.content {
		t.Fatal("full read splits the large body")
	}
	if full.content.start != 0 || !full.content.full {
		t.Fatalf("promoted body holds start %d full %v, want 0 true", full.content.start, full.content.full)
	}
	direct := r.load("output", true)
	if direct.content != small.content {
		t.Fatal("load with full splits the large body")
	}
}
