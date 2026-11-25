//go:build gui

package transcript

import (
	"bytes"
	"slices"
	"strings"
	"sync"
	"testing"

	"gioui.org/io/key"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

func TestSpillCopyReleasesConsumedAndAbandonedPages(t *testing.T) {
	for _, tc := range []struct {
		name   string
		cancel bool
	}{
		{"completed", false},
		{"cancelled before page settles", true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			withTranscript(t, spillTurn, func(t *testing.T, h *harness) {
				checkSpillPageRelease(t, h, tc.cancel)
			})
		})
	}
}

const coldCopyMarkdown = "cold **projected** sentinel"

func TestMixedSpillCopyPagesTruncatedAndColdRows(t *testing.T) {
	want := strings.Join(mixedCopySources(), "\n\n")
	withTranscript(t, mixedCopyTurn, func(t *testing.T, h *harness) {
		defer h.tr.Close()
		h.settle()
		h.router.Source().Execute(key.FocusCmd{Tag: h.tr.FocusTag()})
		h.keyPress("A", key.ModShortcut)
		p := h.tr.panes[h.key]
		assertMixedSelectAll(t, p)
		ordinary := p.rows[identity(h.key, "u1", "ordinary")].sheet.sources[0]
		if ordinary.start == 0 || !ordinary.complete() {
			t.Fatal("ordinary command must have only its completed output tail")
		}
		cold := evictMixedCopyMarkdown(t, h, p)
		h.keyPress("C", key.ModShortcut)
		waitForMixedCopy(t, h)
		if ordinary.full || ordinary.start == 0 {
			t.Fatal("copy promoted the ordinary command to whole-source residency")
		}
		_, copied, ok := h.router.WriteClipboard()
		if !ok || string(copied) != want {
			t.Fatalf("copy published %d bytes, want %d exact projected bytes; notice %q", len(copied), len(want), h.tr.notice)
		}
		if !slices.Contains(h.d.readsOf(cold), 0) {
			t.Fatal("copy did not read the evicted source")
		}
	})
}

func assertMixedSelectAll(t *testing.T, p *pane) {
	t.Helper()
	if p.scope.Len() != 4 || !p.sel.Selected || !coversAll(p, p.sel.Selection) {
		t.Fatalf("Ctrl+A did not select all four fixture rows: scope %d, selection %+v", p.scope.Len(), p.sel.Selection)
	}
}

func waitForMixedCopy(t *testing.T, h *harness) {
	t.Helper()
	r := h.tr.review
	if h.tr.pendingCopy == nil || r == nil || r.spill != h.tr.pendingCopy.pane {
		t.Fatalf("Ctrl+C did not stage the mixed selection: notice %q", h.tr.notice)
	}
	for range 10 {
		h.wait()
		if !r.bridge.waiting() {
			break
		}
		h.frame()
		h.frame()
	}
	h.frame()
	if h.tr.pendingCopy != nil {
		t.Fatalf("mixed copy did not settle after its page and worker events: notice %q", h.tr.notice)
	}
}

func evictMixedCopyMarkdown(t *testing.T, h *harness, p *pane) string {
	t.Helper()
	r := p.rows[identity(h.key, "u1", "cold")]
	if r.doc == nil || len(r.sheet.sources) != 1 || !r.sheet.sources[0].complete() {
		t.Fatal("Markdown must be available before eviction")
	}
	b := r.sheet.sources[0]
	l := h.tr.loader
	p.discardSource(r, l)
	l.resident -= cap(b.data)
	delete(l.bodies, b.key)
	b.data, b.next = nil, b.start
	h.d.mu.Lock()
	h.d.reads = slices.DeleteFunc(h.d.reads, func(call readCall) bool { return call.content == b.ref.ContentID })
	h.d.mu.Unlock()
	if r.doc != nil || !r.sourceEvicted {
		t.Fatal("Markdown did not become cold")
	}
	return b.ref.ContentID
}

func mixedCopySources() []string {
	rows := strings.Repeat("bulk output 123456789\n", 29998) + "row 30000\n"
	return []string{"first spill prefix\n" + rows, "ordinary command prefix\n" + rows, "cold projected sentinel", "second spill prefix\n" + rows}
}

func mixedCopyTurn(d *daemon) []rpc.HistoryTurn {
	source := mixedCopySources()
	spilled := func(id, text string) rpc.ItemHeader {
		return d.header(id, rpc.KindCommand, nil, fieldData{name: "aggregatedOutput", data: "[preview]"}, fieldData{name: fieldSpill, data: text})
	}
	return []rpc.HistoryTurn{frozenTurn("u1", rpc.StatusCompleted,
		spilled("first", source[0]),
		d.header("ordinary", rpc.KindCommand, nil, fieldData{name: "aggregatedOutput", data: source[1]}),
		d.header("cold", rpc.KindAgent, nil, fieldData{name: "text", data: coldCopyMarkdown}),
		spilled("last", source[3]),
	)}
}

func TestMultiRowSpillSelectionStreamsExactBytes(t *testing.T) {
	for _, variant := range []string{"select all", "reverse partial endpoints", "unreadable later page"} {
		t.Run(variant, func(t *testing.T) {
			withTranscript(t, spillAndMessage, func(t *testing.T, h *harness) {
				defer h.tr.Close()
				h.settleUntil("Showing the last 10 of 30000 lines.")
				h.tap(h.textPoint("row 30000"), 0)
				p := h.tr.panes[h.key]
				row := p.rows[identity(h.key, "u1", "c1")]
				b := row.sheet.sources[0]
				raw := h.d.contents[b.ref.ContentID]
				want := string(raw) + "\n\nordinary sentinel"
				if variant == "reverse partial endpoints" {
					want = selectPartialSpill(t, p, row, raw)
				} else {
					h.keyPress("A", key.ModShortcut)
				}
				h.copyMultiRowSpill(t, b, variant == "unreadable later page", want)
			})
		})
	}
}

func spillAndMessage(d *daemon) []rpc.HistoryTurn {
	cmd := spillTurn(d)[0].Items[0]
	msg := d.header("after", rpc.KindAgent, nil, fieldData{name: "text", data: "ordinary **sentinel**"})
	return []rpc.HistoryTurn{frozenTurn("u1", rpc.StatusCompleted, cmd, msg)}
}

func selectPartialSpill(t *testing.T, p *pane, spill *row, raw []byte) string {
	t.Helper()
	tail := "30000"
	from, err := spill.doc.Position(strings.LastIndex(spill.doc.Source(), tail), document.After)
	if err != nil {
		t.Fatal(err)
	}
	msg := p.rows[identity(p.key, "u1", "after")].doc
	to, err := msg.Position(len("ordinary"), document.Before)
	if err != nil {
		t.Fatal(err)
	}
	p.sel.Selection = document.Selection{Anchor: to, Focus: from}
	p.sel.Selected = true
	return string(raw[bytes.LastIndex(raw, []byte(tail)):]) + "\n\nordinary"
}

func (h *harness) copyMultiRowSpill(t *testing.T, b *body, unreadablePage bool, want string) {
	t.Helper()
	call := readCall{content: b.ref.ContentID, offset: 256 << 10}
	if unreadablePage {
		h.d.mu.Lock()
		h.d.failing[call] = true
		h.d.mu.Unlock()
	}
	h.keyPress("C", key.ModShortcut)
	if b.full || b.start == 0 || h.tr.review == nil || h.tr.review.spill == nil {
		t.Fatal("the multi-row selection did not enter bounded copy without promoting its spill")
	}
	waitReviewWork(h)
	h.frame()
	if unreadablePage {
		checkSpillClipboard(t, h, true, nil)
		if !strings.HasPrefix(h.tr.notice, "Copy failed:") || rangeReads(h, call) < 2 {
			t.Fatalf("later page failure not observed: notice %q, reads %d", h.tr.notice, rangeReads(h, call))
		}
		return
	}
	checkSpillClipboard(t, h, false, []byte(want))
}

func checkSpillPageRelease(t *testing.T, h *harness, cancel bool) {
	t.Helper()
	h.settleUntil("Showing the last 10 of 30000 lines.")
	row := h.tr.panes[h.key].rows[identity(h.key, "u1", "c1")]
	b := row.sheet.sources[0]
	call := readCall{content: b.ref.ContentID, offset: 256 << 10}
	gate := holdAccessRead(h, call)
	release := sync.OnceFunc(func() { close(gate) })
	defer release()
	pageKey := state.ContentKey{Item: b.key.item, ContentID: b.ref.ContentID, Offset: call.offset}
	h.tap(h.textPoint("row 30000"), 0)
	h.keyPress("A", key.ModShortcut)
	h.keyPress("C", key.ModShortcut)
	checkCopyRequest(t, h, pageKey)
	if b.start == 0 {
		t.Error("select-all copy promoted the displayed tail to whole-source residency")
	}
	if cancel {
		h.keyPress("O", key.ModShortcut)
		if !h.tr.review.active() {
			t.Fatal("opening the modal did not replace the pending copy")
		}
	}
	release()
	waitReviewWork(h)
	checkSpillClipboard(t, h, cancel, h.d.contents[call.content])
	tv, _ := h.d.owner.View().Thread(h.key)
	if page, ok := tv.Content(pageKey); ok {
		t.Fatalf("completed page retained %d bytes in daemon state after copy or cancellation", len(page.Data))
	}
}

func checkCopyRequest(t *testing.T, h *harness, pageKey state.ContentKey) {
	t.Helper()
	for range 10 {
		h.frame()
		tv, _ := h.d.owner.View().Thread(h.key)
		if page, ok := tv.Content(pageKey); ok && page.Loading {
			return
		}
	}
	t.Fatal("copy did not request the second spill page")
}

func waitReviewWork(h *harness) {
	for range 20 {
		h.frame()
		if !h.tr.review.busy() {
			return
		}
	}
}

func checkSpillClipboard(t *testing.T, h *harness, cancelled bool, want []byte) {
	t.Helper()
	_, copied, ok := h.router.WriteClipboard()
	if cancelled {
		if ok {
			t.Fatal("the cancelled operation published a clipboard transfer")
		}
		return
	}
	if !ok || !bytes.Equal(copied, want) {
		t.Fatalf("copy published %d bytes, want full exact spill of %d bytes", len(copied), len(want))
	}
}
