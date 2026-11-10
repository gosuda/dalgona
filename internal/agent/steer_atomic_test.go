package agent

import (
	"errors"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

func TestSteerAfterTurnEndIsRejected(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := h.start()
	h.submit(thread, "hello")
	h.waitIdle(thread)
	old, head := lastCompletedTurn(t, h, thread)
	err := thread.Submit(t.Context(), Input{Kind: InputSteer, Text: "late steer", ExpectedTurnID: old})
	if !errors.Is(err, ErrTurnNotRunning) {
		t.Fatalf("steer after turn end = %v, want turn not running", err)
	}
	assertIdleHead(t, thread, head)
	if starts := countStarts(h, thread); starts != 1 {
		t.Fatalf("turn starts = %d, want 1", starts)
	}
}

func lastCompletedTurn(t *testing.T, h *harness, thread *Thread) (string, uint64) {
	t.Helper()
	old := lastTurn(h.events(thread.ID())).TurnID
	if old == "" {
		t.Fatal("no completed turn")
	}
	status, err := thread.Status()
	if err != nil {
		t.Fatal(err)
	}
	return old, status.Head
}

func assertIdleHead(t *testing.T, thread *Thread, head uint64) {
	t.Helper()
	after, err := thread.Status()
	if err != nil {
		t.Fatal(err)
	}
	if after.Busy || after.Head != head {
		t.Fatalf("status after rejected steer = %+v, want idle head %d", after, head)
	}
}

func countStarts(h *harness, thread *Thread) int {
	starts := 0
	for _, item := range h.events(thread.ID()) {
		if _, ok := item.Event.(event.TurnStarted); ok {
			starts++
		}
	}
	return starts
}
