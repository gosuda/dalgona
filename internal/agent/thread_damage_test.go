package agent

import (
	"context"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

func TestCleanJournalFailureKeepsThreadUsable(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := h.start()
	ctx, cancel := context.WithCancelCause(t.Context())
	cancel(context.Canceled)
	if _, err := thread.appendEvents(ctx, event.ThreadNamed{Name: "cancelled"}); err == nil {
		t.Fatal("agent: cancelled append returned no error")
	}
	if _, err := thread.appendEvents(t.Context(), event.ThreadNamed{Name: "recovered"}); err != nil {
		t.Fatalf("agent: healthy append after clean failure: %v", err)
	}
	status, err := thread.Status()
	if err != nil {
		t.Fatalf("agent: status: %v", err)
	}
	if status.State == event.StatusDamaged {
		t.Fatalf("agent: thread damaged by a clean journal failure: %+v", status)
	}
}
