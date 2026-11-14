package agent

import (
	"encoding/json/jsontext"
	"fmt"
	"sync"
	"testing"
	"testing/synctest"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
)

func TestStartChildBoundsConcurrentSpawns(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		release := make(chan struct{})
		fire := sync.OnceFunc(func() { close(release) })
		entered := make(chan struct{}, 16)
		h := newHarness(t, func(h *harness) {
			h.cfg.OnChildStarted = func(*Thread, TaskRequest) { entered <- struct{}{}; <-release }
		})
		for range 16 {
			h.model.push(text("child done"))
		}
		parent := h.start()
		seedAgentCalls(t, h, parent)
		var wg sync.WaitGroup
		startChildren(t, h, parent, &wg)
		drainEntries(t, entered, 8, "fewer than 8 spawns reached the child")
		expectCapped(t, entered, fire)
		fire()
		drainEntries(t, entered, 8, "remaining spawns never reached the child")
		wg.Wait()
		checkAllSpawned(t, h, parent)
	})
}

func seedAgentCalls(t *testing.T, h *harness, parent *Thread) {
	t.Helper()
	starts := make([]event.Event, 0, 16)
	for i := range 16 {
		starts = append(starts, event.ToolStarted{CallID: fmt.Sprintf("sc-%d", i), TurnID: "turn-seed", Tool: "task", Args: jsontext.Value(`{}`), Class: "agent"})
	}
	if err := parent.Append(h.t.Context(), append([]event.Event{event.TurnStarted{TurnID: "turn-seed"}}, starts...)...); err != nil {
		t.Fatal(err)
	}
}

func startChildren(t *testing.T, h *harness, parent *Thread, wg *sync.WaitGroup) {
	t.Helper()
	for i := range 16 {
		wg.Go(func() {
			if _, err := parent.StartChild(h.t.Context(), fmt.Sprintf("sc-%d", i), TaskRequest{Prompt: "child", Advisor: true}); err != nil {
				t.Errorf("StartChild sc-%d: %v", i, err)
			}
		})
	}
}

func drainEntries(t *testing.T, entered <-chan struct{}, n int, msg string) {
	t.Helper()
	timer := time.NewTimer(10 * time.Second)
	defer timer.Stop()
	for range n {
		select {
		case <-entered:
			if !timer.Stop() {
				select {
				case <-timer.C:
				default:
				}
			}
			timer.Reset(10 * time.Second)
		case <-timer.C:
			t.Fatal(msg)
		}
	}
}

func expectCapped(t *testing.T, entered <-chan struct{}, fire func()) {
	t.Helper()
	synctest.Wait()
	select {
	case <-entered:
		fire()
		t.Fatal("more than 8 concurrent script spawns; StartChild bypasses the children cap")
	default:
	}
}

func checkAllSpawned(t *testing.T, h *harness, parent *Thread) {
	t.Helper()
	events := h.events(parent.ID())
	if spawned := countType(events, event.TypeChildSpawned); spawned != 16 {
		t.Fatalf("child.spawned = %d, want 16", spawned)
	}
}
