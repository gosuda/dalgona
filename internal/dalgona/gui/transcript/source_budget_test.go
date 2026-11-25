//go:build gui

package transcript

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"testing"

	"gioui.org/io/key"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

func TestWindowSourceBudgetEvictsAndPagesAgain(t *testing.T) {
	var wait <-chan struct{}
	path := filepath.Join(t.TempDir(), "markdown.md")
	deps := Deps{Save: func(ctx context.Context, _ platform.DialogRequest) (platform.DialogResult, error) {
		select {
		case <-wait:
			return platform.DialogResult{Files: []string{path}}, nil
		case <-ctx.Done():
			return platform.DialogResult{}, ctx.Err()
		}
	}}
	withTranscriptDeps(t, "", deps, commandTurn, func(t *testing.T, h *harness) {
		gate := make(chan struct{})
		wait = gate
		proceed := sync.OnceFunc(func() { close(gate) })
		defer proceed()
		source := holdMarkdownForSave(t, h)
		checkWindowSourceBudget(t, h, source)
		proceed()
		h.frame()
		checkFinishedMarkdownSave(t, h, path, source)
	})
}

func TestWindowDoesNotRetainThePublishedFrameSourceSnapshot(t *testing.T) {
	withTranscript(t, commandTurn, func(t *testing.T, h *harness) {
		key := state.ThreadKey{Endpoint: h.key.Endpoint, Thread: "inactive"}
		row := h.tr.paneFor(key).row(identity(key, "turn", "command"), synthNone)
		tail := state.ItemView{Tail: []byte("a frame-local projection")}
		row.turn.Items = []state.ItemView{tail}
		row.entry = Entry{Kind: KindGroup, Item: tail, Members: []Entry{{Item: tail}}}
		h.frame()
		if h.tr.env.View != nil {
			t.Fatal("the window pins the state's entire source snapshot after the frame")
		}
		if row.turn.Items != nil || row.entry.Item.Tail != nil || row.entry.Members[0].Item.Tail != nil {
			t.Fatal("the inactive pane pins shared live source slices outside layout")
		}
	})
}

func TestRetryWaitsForANewSourceSnapshot(t *testing.T) {
	l := newLoader()
	view := state.Snapshot{}.View()
	l.begin(view)
	b := &body{asked: l.epoch}
	var releases int
	submit := func(i state.Intent) {
		if _, ok := i.(state.ReleaseContentPage); ok {
			releases++
		}
	}
	oldError := errors.New("old request failed")
	l.begin(view)
	l.receive(b, state.ContentKey{}, state.ContentPage{Err: oldError}, submit)
	if b.err != nil || releases != 0 {
		t.Fatal("repainting the same snapshot accepted or released its stale error")
	}
	retryError := errors.New("new request failed")
	l.begin(state.Snapshot{}.View())
	l.receive(b, state.ContentKey{}, state.ContentPage{Err: retryError}, submit)
	if !errors.Is(b.err, retryError) || releases != 1 {
		t.Fatalf("retry outcome = %v after %d releases, want the new failure and one release", b.err, releases)
	}
}

func holdMarkdownForSave(t *testing.T, h *harness) *document.Document {
	t.Helper()
	first := h.tr.panes[h.key].rows[identity(h.key, "u1", "i2")]
	source := first.doc
	h.click("Document actions")
	h.click("Save Markdown")
	h.frame()
	if charged := h.tr.loader.heldSourceBytes(); charged != source.ResidentBytes() {
		t.Fatalf("pending Markdown dialog holds %d bytes, want %d", charged, source.ResidentBytes())
	}
	assertProjectedCharge(t, h, 0)
	replaceSavedRow(h)
	if first.doc == source {
		t.Fatal("the active pane did not retire the snapshot retained by Save Markdown")
	}
	return source
}

func replaceSavedRow(h *harness) {
	item := h.d.turns[0].Items[1]
	item.ItemRevision += "-updated"
	h.d.mu.Lock()
	item.Contents = []rpc.ContentReference{h.d.ref("updated-markdown", "text", "utf8", []byte("replacement"))}
	h.d.mu.Unlock()
	h.d.event(h.key.Thread, "item/completed", map[string]any{"turnId": h.d.turns[0].TurnID, "item": item})
	h.frame()
	h.frame()
}

func assertProjectedCharge(t *testing.T, h *harness, retired int) {
	t.Helper()
	l := h.tr.loader
	actual := sourceFootprint(h.tr) + retired
	if actual != l.presented+l.resident {
		t.Fatalf("window ledger charges %d bytes for %d resident bytes", l.presented+l.resident, actual)
	}
}

func checkFinishedMarkdownSave(t *testing.T, h *harness, path string, source *document.Document) {
	t.Helper()
	if h.tr.loader.heldSourceBytes() != 0 {
		t.Fatal("the finished export still holds window source bytes")
	}
	got, err := os.ReadFile(path)
	if err != nil || string(got) != source.Source() {
		t.Fatalf("save wrote %q from the pending revision; want %q: %v", got, source.Source(), err)
	}
}

func checkWindowSourceBudget(t *testing.T, h *harness, saved *document.Document) {
	t.Helper()
	h.click("Open full text")
	r := h.tr.review
	if !r.active() {
		t.Fatal("review did not open")
	}
	l := h.tr.loader
	p := h.tr.panes[h.key]
	coldKey := state.ThreadKey{Endpoint: h.key.Endpoint, Thread: "inactive"}
	cold := h.tr.paneFor(coldKey)
	coldID := identity(coldKey, "turn", "row")
	stored := cold.row(coldID, synthNone)
	stored.sheet = sheet{source: strings.Repeat("d", 240<<10), format: document.FormatCode}
	cold.reconcile([]*row{stored})
	ak := accessRange{document: coldID, end: accessPageBytes}
	cold.accessPages = map[accessRange]*accessPage{ak: {value: strings.Repeat("a", accessPageBytes)}}

	const packet = 240 << 10
	for i := range 136 {
		id := strconv.Itoa(i)
		ref := rpc.ContentReference{ContentID: id, Field: "text", Total: strconv.Itoa(packet)}
		b := l.wantRef(coldKey, state.ItemKey{Turn: "turn", Item: id}, ref, false)
		l.consume(b, state.ContentPage{Total: packet, Next: packet, Data: make([]byte, packet)})
	}
	first := r.fields[0]
	first.reset()
	first.total = 1 << 20
	first.accept(state.ContentPage{Total: first.total, Next: 512 << 10, Data: []byte(strings.Repeat("r", 512<<10))})
	if before := l.resident; before >= document.ResidentSourceBudget {
		t.Fatalf("raw loader alone occupies %d bytes; fixture should require all consumers to cross the limit", before)
	}
	bodies := len(l.bodies)
	h.frame()
	assertProjectedCharge(t, h, saved.ResidentBytes())
	total := sourceFootprint(h.tr) + saved.ResidentBytes()
	if total > document.ResidentSourceBudget {
		t.Fatalf("window owns %d source bytes across loader, two panes, and review, limit %d", total, document.ResidentSourceBudget)
	}
	checkInactiveSourceReleased(t, cold, stored, ak)
	if len(l.bodies) >= bodies {
		t.Fatal("the shared source pressure did not evict raw pages after retiring inactive projections")
	}
	checkSourceRepage(t, h, p)
}

func checkInactiveSourceReleased(t *testing.T, cold *pane, stored *row, ak accessRange) {
	t.Helper()
	if stored.doc != nil || stored.sheet.source != "" || cold.accessPages[ak] != nil || !stored.sourceEvicted {
		t.Fatal("an inactive row or accessibility range kept its source under pressure")
	}
	if loaded, _ := cold.scope.Loaded(0); loaded != nil {
		t.Fatal("the inactive scope pinned its unloaded source")
	}
}

func checkSourceRepage(t *testing.T, h *harness, p *pane) {
	t.Helper()
	l := h.tr.loader
	h.keyPress(key.NameEscape, 0)
	rw := p.rows[identity(h.key, "u1", "i1")]
	original := rw.sheet.sources[0]
	before := len(h.d.readsOf(original.ref.ContentID))
	l.presented = document.ResidentSourceBudget
	l.evict()
	for range 6 {
		h.frame()
	}
	if got := len(h.d.readsOf(original.ref.ContentID)); got <= before {
		t.Fatalf("revisiting an evicted backing source made %d reads, before eviction %d", got, before)
	}
}

func sourceFootprint(t *Transcript) int {
	var size int
	for _, b := range t.loader.bodies {
		size += cap(b.data)
	}
	for _, p := range t.panes {
		size += paneFootprint(p)
	}
	if !t.review.active() {
		return size
	}
	for _, f := range t.review.fields {
		size += cap(f.data) + len(f.suffix)
	}
	return size + paneFootprint(t.review.pane)
}

func paneFootprint(p *pane) int {
	n := 0
	for _, r := range p.rows {
		if r.doc != nil {
			n += r.doc.ResidentBytes()
		}
		for _, img := range r.images {
			n += cap(img.data)
		}
	}
	for _, page := range p.accessPages {
		n += len(page.value)
	}
	return n
}
