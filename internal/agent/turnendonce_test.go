package agent

import (
	"context"
	"sync"
	"testing"
)

type steerOnTurnEnd struct {
	baseExtension
	thread *Thread
	steer  func()
	mu     sync.Mutex
	counts map[string]int
}

func (s *steerOnTurnEnd) TurnEnd(_ context.Context, turnID, _ string) error {
	s.mu.Lock()
	s.counts[turnID]++
	s.mu.Unlock()
	s.steer()
	return nil
}

func TestTurnEndHookRunsOncePerTurn(t *testing.T) {
	t.Parallel()
	var ext *steerOnTurnEnd
	h := newHarness(t, func(h *harness) {
		h.cfg.Extension = func(th *Thread) Extension {
			ext = &steerOnTurnEnd{
				thread: th,
				counts: make(map[string]int),
				steer: sync.OnceFunc(func() {
					_ = th.Submit(context.Background(), Input{Kind: InputSteer, Text: "follow up"})
				}),
			}
			return ext
		}
	})
	h.model.push(text("first done"), text("steered done"))
	thread := h.start()
	h.submit(thread, "hello")
	h.waitIdle(thread)
	checkHookOncePerTurn(t, ext)
}

func checkHookOncePerTurn(t *testing.T, ext *steerOnTurnEnd) {
	t.Helper()
	ext.mu.Lock()
	defer ext.mu.Unlock()
	total := 0
	for turnID, count := range ext.counts {
		total += count
		if count != 1 {
			t.Fatalf("turn_end hook ran %d times for turn %s", count, turnID)
		}
	}
	if total != 2 {
		t.Fatalf("turn_end hook ran %d times across %d turns, want once per turn", total, len(ext.counts))
	}
}
