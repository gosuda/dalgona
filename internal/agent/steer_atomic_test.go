package agent

import (
	"errors"
	"sync/atomic"
	"testing"
	"time"

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

func requireSteerRejected(t *testing.T, submitted <-chan error, updates <-chan Update) {
	t.Helper()
	select {
	case err := <-submitted:
		if !errors.Is(err, ErrTurnNotRunning) {
			t.Fatalf("rejected steer = %v, want turn not running", err)
		}
	case update := <-updates:
		t.Fatalf("rejected steer published update kind %d tool %q instead of failing", update.Kind, update.Approval.Tool)
	case <-time.After(10 * time.Second):
		t.Fatal("rejected steer did not return")
	}
}

func requireNoTrustPrompt(t *testing.T, updates <-chan Update) {
	t.Helper()
	select {
	case update := <-updates:
		t.Fatalf("rejected steer published unexpected update kind %d tool %q", update.Kind, update.Approval.Tool)
	case <-time.After(200 * time.Millisecond):
	}
}

func TestRejectedSteerSkipsWorkspaceTrust(t *testing.T) {
	t.Parallel()
	var calls atomic.Int32
	h := newHarness(t, func(h *harness) {
		h.cfg.Tools = approvingTools(&calls)
	})
	thread := h.start()
	updates := thread.Subscribe()
	defer thread.Unsubscribe(updates)
	submitted := make(chan error, 1)
	go func() {
		submitted <- thread.Submit(t.Context(), Input{Kind: InputSteer, Text: "stray steer", ExpectedTurnID: "turn-does-not-run"})
	}()
	requireSteerRejected(t, submitted, updates)
	if got := calls.Load(); got != 0 {
		t.Fatalf("tool builds during rejected steer = %d, want none before the rejection", got)
	}
	requireNoTrustPrompt(t, updates)
}
