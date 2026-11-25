//go:build gui

package transcript

import (
	"errors"
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/gui/access/accesstest"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

const studyRangeName = "Source bytes 16385 to 32768"

func TestAccessibilityFetchesOnceAndResumesFocus(t *testing.T) {
	withTranscript(t, spillTurn, func(t *testing.T, h *harness) {
		n, call := accessStudyRange(t, h)
		gate := holdAccessRead(h, call)
		before := h.ax.Tree.Snapshot()
		updates := new(rangeObserver)
		defer h.ax.Tree.Observe(updates)()
		queryStudyRange(t, h, n, platform.ErrAccessibilityBusy)
		queryStudyRange(t, h, n, platform.ErrAccessibilityBusy)
		h.ax.Request(t, n, platform.ActionFocus)
		h.frame()
		waiting := h.ax.Tree.Snapshot()
		if current, _ := waiting.Node(n.ID); current.TextState != platform.TextLoading || waiting.Focus() != before.Focus() {
			t.Fatalf("waiting range state %d, focus %q, expected loading with focus %q", current.TextState, waiting.Focus(), before.Focus())
		}
		close(gate)
		h.frame()
		h.frame()
		want := string(h.d.contents[call.content][call.offset : call.offset+accessPageBytes])
		if got := queryStudyRange(t, h, n, nil); got != want || len(got) > platform.TextObjectLimit {
			t.Fatalf("fetched text length %d, expected %d bytes", len(got), len(want))
		}
		if calls := rangeReads(h, call); calls != 1 {
			t.Fatalf("duplicate queries caused %d reads of offset %d", calls, call.offset)
		}
		assertPublishedBeforeFocus(t, updates, n.ID, before.Focus())
	})
}

func TestAccessibilityDiscardsNavigationToStaleRevision(t *testing.T) {
	withTranscript(t, spillTurn, func(t *testing.T, h *harness) {
		n, call := accessStudyRange(t, h)
		gate := holdAccessRead(h, call)
		previous := h.ax.Tree.Snapshot().Focus()
		h.ax.Request(t, n, platform.ActionFocus)
		h.frame()
		item := h.d.turns[0].Items[0]
		item.ItemRevision += "-new"
		h.d.event(h.key.Thread, "item/completed", map[string]any{"turnId": h.d.turns[0].TurnID, "item": item})
		h.frame()
		close(gate)
		h.frame()
		h.frame()
		current := h.ax.Tree.Snapshot()
		if updated, exists := current.Node(n.ID); !exists || updated.TextRevision != item.ItemRevision || updated.TextState != platform.TextUncached {
			t.Fatalf("new revision inherited old range: %+v", updated)
		}
		if current.Focus() != previous {
			t.Fatalf("stale navigation changed focus from %q to %q", previous, current.Focus())
		}
	})
}

func TestAccessibilityRangeFailureExposesRetry(t *testing.T) {
	withTranscript(t, spillTurn, func(t *testing.T, h *harness) {
		n, call := accessStudyRange(t, h)
		h.d.mu.Lock()
		h.d.failing[call] = true
		h.d.mu.Unlock()
		h.ax.Request(t, n, platform.ActionFocus)
		h.frame()
		h.frame()
		queryStudyRange(t, h, n, platform.ErrElementUnavailable)
		retry := h.ax.Find(t, "Retry loading text", platform.RoleButton)
		if !slices.ContainsFunc(h.ax.Nodes, func(errorNode platform.SemanticNode) bool {
			return errorNode.Role == platform.RoleError && errorNode.Parent == n.ID && errorNode.Target == retry.ID && strings.HasPrefix(errorNode.Name, "Text could not load: ")
		}) {
			t.Fatalf("no readable range error associated with retry %q", retry.ID)
		}
		h.ax.Request(t, retry, platform.ActionActivate)
		h.frame()
		h.frame()
		h.frame()
		if got := queryStudyRange(t, h, n, nil); len(got) != accessPageBytes || h.ax.Tree.Snapshot().Focus() != n.ID {
			t.Fatalf("retry finished with %d bytes and focus %q", len(got), h.ax.Tree.Snapshot().Focus())
		}
	})
}

func accessStudyRange(t *testing.T, h *harness) (platform.SemanticNode, readCall) {
	t.Helper()
	h.ax = accesstest.New()
	h.frame()
	n := h.ax.Find(t, studyRangeName, platform.RoleDocument)
	call := readCall{content: h.d.turns[0].Items[0].Contents[2].ContentID, offset: accessPageBytes}
	if calls := rangeReads(h, call); n.TextState != platform.TextUncached || calls != 0 {
		t.Fatalf("range is in state %d with %d reads before any query", n.TextState, calls)
	}
	return n, call
}

func rangeReads(h *harness, call readCall) int {
	count := 0
	for _, offset := range h.d.readsOf(call.content) {
		if offset == call.offset {
			count++
		}
	}
	return count
}

func holdAccessRead(h *harness, call readCall) chan struct{} {
	gate := make(chan struct{})
	h.d.mu.Lock()
	h.d.readGate = map[readCall]<-chan struct{}{call: gate}
	h.d.mu.Unlock()
	return gate
}

func queryStudyRange(t *testing.T, h *harness, n platform.SemanticNode, want error) string {
	t.Helper()
	text, err := h.ax.Tree.QueryText(n.ID, h.ax.Tree.Snapshot().Revision())
	if !errors.Is(err, want) || want != nil && text != "" {
		t.Fatalf("query returned %d bytes and %v, want %v", len(text), err, want)
	}
	return text
}

type rangeObserver struct {
	snapshots []*platform.SemanticSnapshot
}

func (o *rangeObserver) SnapshotChanged(_, next *platform.SemanticSnapshot) {
	o.snapshots = append(o.snapshots, next)
}

func (*rangeObserver) Announced(platform.Announcement) {}

func assertPublishedBeforeFocus(t *testing.T, o *rangeObserver, rangeID, originalFocus platform.NodeID) {
	t.Helper()
	readyBeforeFocus := false
	for _, s := range o.snapshots {
		n, _ := s.Node(rangeID)
		if s.Focus() == rangeID && readyBeforeFocus {
			return
		}
		readyBeforeFocus = readyBeforeFocus || (n.TextState == platform.TextReady && n.Value != "" && s.Focus() == originalFocus)
	}
	t.Fatal("the range did not publish ready content before its pending focus resumed")
}
