package agent

import (
	"context"
	"slices"
	"sync"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

type steerFlagOnTurnEnd struct {
	baseExtension
	thread *Thread
	mu     sync.Mutex
	bounds []Boundary
	steer  func()
}

func (s *steerFlagOnTurnEnd) hook(b Boundary) Wait {
	s.mu.Lock()
	s.bounds = append(s.bounds, b)
	s.mu.Unlock()
	return nil
}

func (s *steerFlagOnTurnEnd) TurnEnd(_ context.Context, _, _ string) error {
	s.steer()
	return nil
}

func lastAgentEnd(bounds []Boundary) (Boundary, bool) {
	for i := len(bounds) - 1; i >= 0; i-- {
		if bounds[i].Kind == BoundaryAgentEnd {
			return bounds[i], true
		}
	}
	return Boundary{}, false
}

func countTurnStarted(t *testing.T, h *harness, id string) int {
	t.Helper()
	n := 0
	for _, item := range h.events(id) {
		if _, ok := item.Event.(event.TurnStarted); ok {
			n++
		}
	}
	return n
}

func TestLateSteerStartsNormalTurn(t *testing.T) {
	t.Parallel()
	var ext *steerFlagOnTurnEnd
	h := newHarness(t, func(h *harness) {
		h.cfg.Extension = func(th *Thread) Extension {
			ext = &steerFlagOnTurnEnd{thread: th}
			ext.steer = sync.OnceFunc(func() {
				_ = th.Submit(context.Background(), Input{Kind: InputSteer, Text: "follow up"})
			})
			return ext
		}
	})
	h.model.push(text("first done"), text("steered done"))
	thread := h.start()
	if err := thread.SetBoundaryHook(ext.hook); err != nil {
		t.Fatal(err)
	}
	h.submit(thread, "hello")
	h.waitIdle(thread)
	ext.mu.Lock()
	bounds := slices.Clone(ext.bounds)
	ext.mu.Unlock()
	last, ok := lastAgentEnd(bounds)
	if !ok {
		t.Fatal("agent: no agent-end boundary recorded")
	}
	if last.Continuation {
		t.Fatalf("agent: late steer started a continuation turn: %+v", last)
	}
	if got := countTurnStarted(t, h, thread.ID()); got != 2 {
		t.Fatalf("agent: turn.started count = %d, want 2", got)
	}
}

func TestLateSteerInterruptedStartsNoTurn(t *testing.T) {
	t.Parallel()
	var ext *steerFlagOnTurnEnd
	h := newHarness(t, func(h *harness) {
		h.cfg.Extension = func(th *Thread) Extension {
			ext = &steerFlagOnTurnEnd{thread: th}
			ext.steer = sync.OnceFunc(func() {
				_ = th.Submit(context.Background(), Input{Kind: InputSteer, Text: "follow up"})
				th.Interrupt()
			})
			return ext
		}
	})
	h.model.push(text("first done"))
	thread := h.start()
	if err := thread.SetBoundaryHook(ext.hook); err != nil {
		t.Fatal(err)
	}
	h.submit(thread, "hello")
	h.waitIdle(thread)
	if got := countTurnStarted(t, h, thread.ID()); got != 1 {
		t.Fatalf("agent: turn.started count = %d, want 1 after interrupt", got)
	}
	if got := h.model.count(); got != 1 {
		t.Fatalf("agent: model requests = %d, want 1 after interrupt", got)
	}
}
