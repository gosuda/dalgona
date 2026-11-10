package agent

import (
	"context"
	"sync"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

type steerThenPark struct {
	baseExtension
	steer func()
}

func (s *steerThenPark) TurnEnd(_ context.Context, _, _ string) error {
	s.steer()
	return nil
}

func startThreadWithParkedLateSteer(t *testing.T) (*harness, *Thread) {
	t.Helper()
	h := newHarness(t, func(h *harness) {
		h.cfg.Extension = func(th *Thread) Extension {
			ext := &steerThenPark{}
			ext.steer = sync.OnceFunc(func() {
				_ = th.Submit(context.Background(), Input{Kind: InputSteer, Text: "follow up"})
				_ = askErr(th, func(a *actor) error {
					a.autoContinue = false
					return nil
				})
			})
			return ext
		}
	})
	return h, h.start()
}

func hasSteerText(events []event.Seqd, want string) bool {
	for _, item := range events {
		message, ok := item.Event.(event.UserMessage)
		if !ok {
			continue
		}
		if message.Kind == string(InputSteer) && message.Text == want {
			return true
		}
	}
	return false
}

func TestLateSteerWithAutoContinueOffStartsNoTurn(t *testing.T) {
	t.Parallel()
	h, thread := startThreadWithParkedLateSteer(t)
	h.model.push(text("first done"))
	h.submit(thread, "hello")
	h.waitIdle(thread)
	if got := countStarts(h, thread); got != 1 {
		t.Fatalf("turn.started count = %d, want 1", got)
	}
	if got := h.model.count(); got != 1 {
		t.Fatalf("model requests = %d, want 1", got)
	}
	status, err := thread.Status()
	if err != nil {
		t.Fatal(err)
	}
	if status.Busy {
		t.Fatalf("thread busy after parked steer: %+v", status)
	}
	if status.AutoContinue {
		t.Fatalf("auto-continue on after parked steer: %+v", status)
	}
	events := h.events(thread.ID())
	if turn := lastTurn(events); turn.Status != statusCompleted {
		t.Fatalf("turn status = %q, want completed", turn.Status)
	}
	if !hasSteerText(events, "follow up") {
		t.Fatal("late steer missing from journal")
	}
}
