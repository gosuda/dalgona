package agent

import (
	"encoding/json/jsontext"
	"fmt"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/tool/task"
)

type spawnResult struct {
	id  string
	err error
}

func seedSpawnCalls(t *testing.T, h *harness, parent *Thread, prefix string, n int) {
	t.Helper()
	starts := make([]event.Event, 0, n)
	for i := range n {
		starts = append(starts, event.ToolStarted{
			CallID: fmt.Sprintf("%s-%d", prefix, i),
			TurnID: "turn-seed",
			Tool:   "task",
			Args:   jsontext.Value(`{}`),
			Class:  "agent",
		})
	}
	if err := parent.Append(h.t.Context(), append([]event.Event{event.TurnStarted{TurnID: "turn-seed"}}, starts...)...); err != nil {
		t.Fatal(err)
	}
}

func startGatedChild(t *testing.T, parent *Thread, callID string, out chan<- spawnResult) {
	t.Helper()
	go func() {
		id, err := parent.StartChild(t.Context(), callID, task.Request{Prompt: "child"})
		out <- spawnResult{id: id, err: err}
	}()
}

func awaitArrivals(t *testing.T, h *harness, n int) {
	t.Helper()
	for range n {
		select {
		case <-h.model.arrived:
		case <-time.After(10 * time.Second):
			t.Fatalf("model arrivals timed out waiting for %d", n)
		}
	}
}

func expectNoArrival(t *testing.T, h *harness, msg string) {
	t.Helper()
	select {
	case <-h.model.arrived:
		t.Fatal(msg)
	case <-time.After(500 * time.Millisecond):
	}
}

func collectSpawn(t *testing.T, out <-chan spawnResult) spawnResult {
	t.Helper()
	select {
	case r := <-out:
		return r
	case <-time.After(10 * time.Second):
		t.Fatal("spawn did not return")
		return spawnResult{}
	}
}

func TestStartChildBoundsRunningChildren(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	gate := make(chan struct{})
	for range 16 {
		h.model.push(response{body: text("child done").body, gate: gate})
	}
	parent := h.start()
	seedSpawnCalls(t, h, parent, "bound", 16)
	done := make(chan spawnResult, 16)
	for i := range 16 {
		startGatedChild(t, parent, fmt.Sprintf("bound-%d", i), done)
	}
	awaitArrivals(t, h, 8)
	expectNoArrival(t, h, "more than 8 children run at once; StartChild releases its slot on return")
	close(gate)
	ids := make(map[string]bool, 16)
	for range 16 {
		r := collectSpawn(t, done)
		if r.err != nil {
			t.Fatalf("StartChild: %v", r.err)
		}
		ids[r.id] = true
	}
	if len(ids) != 16 {
		t.Fatalf("children = %d, want 16", len(ids))
	}
}

func holdGate(t *testing.T) chan struct{} {
	t.Helper()
	gate := make(chan struct{})
	t.Cleanup(func() {
		select {
		case <-gate:
		default:
			close(gate)
		}
	})
	return gate
}

func fillChildren(t *testing.T, parent *Thread, prefix string, n int) []string {
	t.Helper()
	ids := make([]string, 0, n)
	for i := range n {
		id, err := parent.StartChild(t.Context(), fmt.Sprintf("%s-%d", prefix, i), task.Request{Prompt: "child"})
		if err != nil {
			t.Fatalf("StartChild %s-%d: %v", prefix, i, err)
		}
		ids = append(ids, id)
	}
	return ids
}

func expectSpawnPending(t *testing.T, out <-chan spawnResult, msg string) {
	t.Helper()
	select {
	case r := <-out:
		t.Fatalf("%s: %v %v", msg, r.id, r.err)
	case <-time.After(500 * time.Millisecond):
	}
}

func closeChild(t *testing.T, h *harness, id string) {
	t.Helper()
	child, err := h.host.Resume(t.Context(), id)
	if err != nil {
		t.Fatalf("resume child: %v", err)
	}
	if err := child.Close(); err != nil {
		t.Fatalf("close child: %v", err)
	}
}

func TestStartChildCloseFreesSlot(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	gate := holdGate(t)
	for range 9 {
		h.model.push(response{body: text("child done").body, gate: gate})
	}
	parent := h.start()
	seedSpawnCalls(t, h, parent, "close", 9)
	ids := fillChildren(t, parent, "close", 8)
	awaitArrivals(t, h, 8)
	ninth := make(chan spawnResult, 1)
	startGatedChild(t, parent, "close-8", ninth)
	expectSpawnPending(t, ninth, "ninth spawn ran while 8 children run")
	closeChild(t, h, ids[0])
	r := collectSpawn(t, ninth)
	if r.err != nil || r.id == "" {
		t.Fatalf("ninth spawn = %q, %v, want a child id", r.id, r.err)
	}
}
