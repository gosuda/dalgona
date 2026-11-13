package agent

import (
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
)

func TestAppendStampsTurnAndToolTimes(t *testing.T) {
	base := time.Date(2026, 11, 11, 10, 0, 0, 0, time.UTC)
	var ticks atomic.Int64
	var mu sync.Mutex
	issued := map[int64]bool{}
	clock := func() time.Time {
		at := base.Add(time.Duration(ticks.Add(1)) * time.Second)
		mu.Lock()
		issued[at.UnixNano()] = true
		mu.Unlock()
		return at
	}
	h := newHarness(t, func(h *harness) { h.cfg.Clock = clock })
	h.model.push(calls(call{id: "c1", name: "read", args: `{"path":"a.txt"}`}), text("done"))
	h.write("a.txt", "one\n")
	thread := h.start()
	h.submit(thread, "read it")
	h.waitIdle(thread)

	mu.Lock()
	defer mu.Unlock()
	timing := collectTiming(h.events(thread.ID()))
	requireClockValue(t, issued, "turn.started", timing.turnStart.StartedAt)
	requireClockValue(t, issued, "turn.completed", timing.turnDone.CompletedAt)
	requireClockValue(t, issued, "tool.started", timing.toolStart.StartedAt)
	requireClockValue(t, issued, "tool.completed", timing.toolDone.CompletedAt)
	if !timing.turnDone.CompletedAt.After(timing.turnStart.StartedAt) || !timing.toolDone.CompletedAt.After(timing.toolStart.StartedAt) {
		t.Fatal("a completed_at is not after its started_at")
	}
}

type timingEvents struct {
	turnStart event.TurnStarted
	turnDone  event.TurnCompleted
	toolStart event.ToolStarted
	toolDone  event.ToolCompleted
}

func collectTiming(events []event.Seqd) timingEvents {
	var out timingEvents
	for _, item := range events {
		switch value := item.Event.(type) {
		case event.TurnStarted:
			out.turnStart = value
		case event.TurnCompleted:
			out.turnDone = value
		case event.ToolStarted:
			out.toolStart = value
		case event.ToolCompleted:
			out.toolDone = value
		}
	}
	return out
}

func requireClockValue(t *testing.T, issued map[int64]bool, label string, at time.Time) {
	t.Helper()
	if !issued[at.UnixNano()] {
		t.Fatalf("%s %v is not a clock value", label, at)
	}
}

func TestStampTimesKeepsPresetValue(t *testing.T) {
	now := time.Date(2026, 11, 11, 10, 0, 0, 0, time.UTC)
	preset := now.Add(-time.Hour)
	events := []event.Event{
		event.TurnStarted{TurnID: "t", StartedAt: preset},
		event.TurnStarted{TurnID: "u"},
		event.ToolCompleted{CallID: "c", OK: true},
	}
	got := stampTimes(events, now)
	if at := got[0].(event.TurnStarted).StartedAt; !at.Equal(preset) {
		t.Fatalf("preset started_at changed to %v", at)
	}
	if at := got[1].(event.TurnStarted).StartedAt; !at.Equal(now) {
		t.Fatalf("zero started_at stamped %v, want %v", at, now)
	}
	if at := got[2].(event.ToolCompleted).CompletedAt; !at.Equal(now) {
		t.Fatalf("zero completed_at stamped %v, want %v", at, now)
	}
	if !events[1].(event.TurnStarted).StartedAt.IsZero() {
		t.Fatal("stampTimes modified its input slice")
	}
}
