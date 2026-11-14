package agent

import (
	"context"
	"errors"
	"slices"
	"sync"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/model"
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
	for _, bound := range slices.Backward(bounds) {
		if bound.Kind == BoundaryAgentEnd {
			return bound, true
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

func lateSteerHarness(t *testing.T, steer func(*Thread), configure ...func(*harness)) (*harness, *Thread, *steerFlagOnTurnEnd) {
	t.Helper()
	var ext *steerFlagOnTurnEnd
	base := func(h *harness) {
		h.cfg.Extension = func(th *Thread) Extension {
			ext = &steerFlagOnTurnEnd{thread: th}
			ext.steer = sync.OnceFunc(func() { steer(th) })
			return ext
		}
	}
	opts := append([]func(*harness){base}, configure...)
	h := newHarness(t, opts...)
	thread := h.start()
	if err := thread.SetBoundaryHook(ext.hook); err != nil {
		t.Fatal(err)
	}
	return h, thread, ext
}

func TestLateSteerStartsNormalTurn(t *testing.T) {
	t.Parallel()
	ctx := t.Context()
	h, thread, ext := lateSteerHarness(t, func(th *Thread) {
		_ = th.Submit(ctx, Input{Kind: InputSteer, Text: "follow up"})
	})
	h.model.push(text("first done"), text("steered done"))
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
	ctx := t.Context()
	h, thread, _ := lateSteerHarness(t, func(th *Thread) {
		_ = th.Submit(ctx, Input{Kind: InputSteer, Text: "follow up"})
		th.Interrupt()
	})
	h.model.push(text("first done"))
	h.submit(thread, "hello")
	h.waitIdle(thread)
	if got := countTurnStarted(t, h, thread.ID()); got != 1 {
		t.Fatalf("agent: turn.started count = %d, want 1 after interrupt", got)
	}
	if got := h.model.count(); got != 1 {
		t.Fatalf("agent: model requests = %d, want 1 after interrupt", got)
	}
}

func TestLateSteerFailedStartsNoTurn(t *testing.T) {
	t.Parallel()
	ctx := t.Context()
	h, thread, _ := lateSteerHarness(t, func(th *Thread) {
		_ = th.Submit(ctx, Input{Kind: InputSteer, Text: "follow up"})
	}, func(h *harness) {
		h.cfg.Providers = func(RetryFunc) (*model.Registry, error) {
			return nil, errors.New("no providers")
		}
	})
	h.submit(thread, "hello")
	h.waitIdle(thread)
	if got := countTurnStarted(t, h, thread.ID()); got != 1 {
		t.Fatalf("agent: turn.started count = %d, want 1 after failure", got)
	}
	if turn := lastTurn(h.events(thread.ID())); turn.Status != statusFailed {
		t.Fatalf("agent: turn.completed status = %q, want failed", turn.Status)
	}
}
