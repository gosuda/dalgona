//go:build gui

package transcript

import (
	"testing"

	"gioui.org/f32"
	"gioui.org/io/event"
	"gioui.org/io/key"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/access/accesstest"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

func TestFullTextTrapsKeyboardAndRestoresOpener(t *testing.T) {
	turn := func(d *daemon) []rpc.HistoryTurn {
		return []rpc.HistoryTurn{frozenTurn("u", rpc.StatusCompleted, d.header("cmd", rpc.KindCommand, map[string]any{"status": rpc.StatusCompleted}))}
	}
	withTranscript(t, turn, checkReviewModal)
}

func checkReviewModal(t *testing.T, h *harness) {
	h.ax = accesstest.New()
	h.frame()
	opener := h.ax.Find(t, "Open full text", platform.RoleButton)
	clickReviewOnce(t, h, opener)
	r := h.tr.review
	openerTag := r.opener
	h.frame()
	h.ax.Find(t, "Full text", platform.RoleDialog)
	if _, ok := h.ax.Tree.Snapshot().Node(opener.ID); ok {
		t.Fatal("covered transcript opener remained accessible with the dialog mounted")
	}
	checkReviewTraversal(t, h)
	h.keyPress(key.NameEscape, 0)
	h.frame()
	checkReviewRestored(t, h, opener.ID, openerTag)
	h.click("Open full text")
	h.frame()
	h.click("Close")
	h.frame()
	checkReviewRestored(t, h, opener.ID, openerTag)
	checkPendingReviewRejectsChangedOpener(t, h, opener)
}

func checkPendingReviewRejectsChangedOpener(t *testing.T, h *harness, opener platform.SemanticNode) {
	t.Helper()
	clickReviewOnce(t, h, opener)
	item := h.d.turns[0].Items[0]
	item.ItemRevision += "-updated"
	h.d.event(h.key.Thread, "item/completed", map[string]any{"turnId": h.d.turns[0].TurnID, "item": item})
	h.frame()
	if h.tr.review.active() || h.has("Full text") {
		t.Fatal("the pending review activated for an obsolete item revision")
	}
}

func clickReviewOnce(t *testing.T, h *harness, opener platform.SemanticNode) {
	t.Helper()
	b, ok := h.button("Open full text")
	if !ok {
		t.Fatal("no pointer opener")
	}
	at := b.Desc.Bounds.Min.Add(b.Desc.Bounds.Size().Div(2))
	h.tap(f32.Pt(float32(at.X), float32(at.Y)), 0)
	if !h.tr.review.active() {
		h.frame()
	}
	if !h.tr.review.active() {
		t.Fatal("pointer click did not request the review")
	}
	for _, n := range h.ax.Nodes {
		if n.Role == platform.RoleDialog {
			t.Fatal("pointer opening published the dialog before removing the backdrop")
		}
	}
	if _, ok := h.ax.Tree.Snapshot().Node(opener.ID); !ok {
		t.Fatal("the opening transition lost both the opener and the dialog")
	}
}

func checkReviewTraversal(t *testing.T, h *harness) {
	t.Helper()
	r := h.tr.review
	stop := []struct {
		name string
		tag  event.Tag
	}{
		{"Copy all", &r.copyAll},
		{"Export", &r.export},
		{"Close", &r.close},
		{"Find input", r.find.Tag()},
		{"Find", &r.search},
		{"Review text", &r.focus},
	}
	for _, target := range stop {
		h.keyPress(key.NameTab, 0)
		if !h.router.Source().Focused(target.tag) {
			t.Fatalf("Tab did not focus %s", target.name)
		}
	}
	for i := len(stop) - 2; i >= 0; i-- {
		h.keyPress(key.NameTab, key.ModShift)
		if !h.router.Source().Focused(stop[i].tag) {
			t.Fatalf("Shift+Tab did not focus %s", stop[i].name)
		}
	}
}

func checkReviewRestored(t *testing.T, h *harness, id platform.NodeID, tag event.Tag) {
	t.Helper()
	if h.tr.review.active() || !h.router.Source().Focused(tag) {
		t.Fatal("dismissing the review did not restore keyboard focus to its opener")
	}
	if got := h.ax.Tree.Snapshot().Focus(); got != id {
		t.Fatalf("accessibility focus after dismissal = %q, want %q", got, id)
	}
}
