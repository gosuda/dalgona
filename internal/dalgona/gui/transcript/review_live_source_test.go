//go:build gui

package transcript

import (
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func TestFullTextCopiesCapturedLiveTailByPages(t *testing.T) {
	withTranscript(t, emptyReviewHistory, func(t *testing.T, h *harness) {
		tail, _ := openLiveTail(t, h)
		h.click("Copy all")
		waitReviewWork(h)
		_, data, ok := h.router.WriteClipboard()
		if !ok || string(data) != "seq\n\n"+tail {
			t.Fatalf("copy published %d bytes, want complete %d-byte captured command and tail", len(data), len(tail)+5)
		}
	})
}

func TestFullTextRejectsReplacedLiveTailBetweenPages(t *testing.T) {
	withTranscript(t, emptyReviewHistory, func(t *testing.T, h *harness) {
		tail, item := openLiveTail(t, h)
		h.click("Copy all")
		waitLiveContinuation(t, h)
		publishLiveTail(h, item, strings.Repeat("x", len(tail)))
		waitReviewWork(h)
		if _, data, ok := h.router.WriteClipboard(); ok {
			t.Fatalf("source replacement published %d mixed-revision bytes", len(data))
		}
		h.tr.review.mu.Lock()
		status := h.tr.review.status
		h.tr.review.mu.Unlock()
		if !strings.Contains(status, errLiveSourceChanged.Error()) {
			t.Fatalf("copy reported %q, want explicit source replacement failure", status)
		}
	})
}

func emptyReviewHistory(_ *daemon) []rpc.HistoryTurn {
	return []rpc.HistoryTurn{frozenTurn("u1", rpc.StatusCompleted)}
}

func openLiveTail(t *testing.T, h *harness) (string, rpc.ItemHeader) {
	t.Helper()
	h.d.event(h.key.Thread, "turn/started", map[string]any{"turn": map[string]any{"id": "live", "status": rpc.StatusInProgress}})
	h.d.mu.Lock()
	item := h.d.header("command", rpc.KindCommand, map[string]any{"status": rpc.StatusInProgress}, fieldData{name: "command", data: "seq"})
	h.d.mu.Unlock()
	tail := strings.Repeat("live output with CRLF\r\n", 27000)
	publishLiveTail(h, item, tail)
	h.settle()
	h.click("Open full text")
	h.frame()
	r := h.tr.review
	if !r.active() || len(r.fields) < 2 {
		t.Fatal("the live command did not open its review")
	}
	output := r.fields[1]
	if got := len(output.data); got != inlineLimit || output.suffix != "" {
		t.Fatalf("first review frame retained %d raw bytes and %d suffix bytes; want one %d-byte page only", got, len(output.suffix), inlineLimit)
	}
	return tail, item
}

func publishLiveTail(h *harness, item rpc.ItemHeader, tail string) {
	h.d.event(h.key.Thread, "item/started", map[string]any{"turnId": "live", "item": item})
	h.d.event(h.key.Thread, "item/commandExecution/outputDelta", map[string]any{"turnId": "live", "itemId": item.ID, "delta": tail})
}

func waitLiveContinuation(t *testing.T, h *harness) {
	t.Helper()
	for range 12 {
		h.wait()
		b := &h.tr.review.bridge
		b.mu.Lock()
		ready := len(b.pending) > 0 && b.pending[0].live.size > 0 && b.pending[0].offset > 0
		b.mu.Unlock()
		if ready {
			return
		}
		h.frame()
	}
	t.Fatal("copy did not ask for a second live page before the fixture changed the payload")
}
