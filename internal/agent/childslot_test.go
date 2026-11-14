package agent

import (
	"encoding/json/jsontext"
	"fmt"
	"sync/atomic"
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

func awaitArrivals(t *testing.T, h *harness, n int) {
	t.Helper()
	timer := time.NewTimer(10 * time.Second)
	defer timer.Stop()
	for range n {
		select {
		case <-h.model.arrived:
			if !timer.Stop() {
				select {
				case <-timer.C:
				default:
				}
			}
			timer.Reset(10 * time.Second)
		case <-timer.C:
			t.Fatalf("model arrivals timed out waiting for %d", n)
		}
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

func collectIDs(t *testing.T, out <-chan spawnResult, n int, ids map[string]bool) {
	t.Helper()
	for range n {
		r := collectSpawn(t, out)
		if r.err != nil {
			t.Fatalf("StartChild: %v", r.err)
		}
		ids[r.id] = true
	}
}

func TestStartChildBoundsRunningChildren(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	rel := make([]chan struct{}, 16)
	gates := make([]chan struct{}, 16)
	for i := range gates {
		rel[i] = make(chan struct{})
		gates[i] = make(chan struct{})
	}
	t.Cleanup(func() {
		closeOpenRels(rel)
	})
	var live atomic.Int64
	var peak atomic.Int64
	var next atomic.Int64
	h.model.route = func([]byte) (response, bool) {
		n := next.Add(1) - 1
		cur := live.Add(1)
		for {
			p := peak.Load()
			if cur <= p || peak.CompareAndSwap(p, cur) {
				break
			}
		}
		var gate chan struct{}
		if n < int64(len(gates)) {
			gate = gates[n]
			release := rel[n]
			go func() {
				<-release
				live.Add(-1)
				close(gate)
			}()
		}
		return response{body: sseText("child done"), gate: gate}, true
	}
	parent := h.start()
	seedSpawnCalls(t, h, parent, "bound", 16)
	done := make(chan spawnResult, 16)
	started := make(chan struct{}, 16)
	for i := range 16 {
		go func() {
			started <- struct{}{}
			id, err := parent.StartChild(t.Context(), fmt.Sprintf("bound-%d", i), task.Request{Prompt: "child"})
			done <- spawnResult{id: id, err: err}
		}()
	}
	for range 16 {
		<-started
	}
	awaitArrivals(t, h, 8)
	checkBoundSaturated(t, parent, done, &peak)
	ids := make(map[string]bool, 16)
	collectIDs(t, done, 8, ids)
	for k := range 8 {
		close(rel[k])
		awaitArrivals(t, h, 1)
		checkBoundPeak(t, &peak)
	}
	closeOpenRels(rel)
	collectIDs(t, done, 8, ids)
	checkBoundComplete(t, ids, &peak)
	checkSlotHeldAfterReturn(t, h)
}

func checkSlotHeldAfterReturn(t *testing.T, h *harness) {
	t.Helper()
	h.model.route = nil
	h.model.push(response{body: text("child done").body, gate: holdGate(t)})
	probeParent := h.start()
	seedSpawnCalls(t, h, probeParent, "probe", 1)
	_ = fillChildren(t, probeParent, "probe", 1)
	if got := len(probeParent.children); got != 1 {
		t.Fatalf("running children = %d, want 1", got)
	}
}

func checkBoundSaturated(t *testing.T, parent *Thread, done <-chan spawnResult, peak *atomic.Int64) {
	t.Helper()
	if got := peak.Load(); got != 8 {
		t.Fatalf("concurrent children = %d, want 8", got)
	}
	if got := len(parent.children); got != 8 {
		t.Fatalf("running children = %d, want 8", got)
	}
	if got := len(done); got != 8 {
		t.Fatalf("returned spawns = %d, want 8", got)
	}
}

func checkBoundPeak(t *testing.T, peak *atomic.Int64) {
	t.Helper()
	if got := peak.Load(); got != 8 {
		t.Fatalf("concurrent children = %d, want 8", got)
	}
}

func checkBoundComplete(t *testing.T, ids map[string]bool, peak *atomic.Int64) {
	t.Helper()
	if got := peak.Load(); got != 8 {
		t.Fatalf("concurrent children = %d, want 8", got)
	}
	if len(ids) != 16 {
		t.Fatalf("children = %d, want 16", len(ids))
	}
}

func closeOpenRels(rel []chan struct{}) {
	for _, r := range rel {
		select {
		case <-r:
		default:
			close(r)
		}
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

func startSignaledChild(t *testing.T, parent *Thread, callID string, out chan<- spawnResult, entered chan<- struct{}) {
	t.Helper()
	go func() {
		close(entered)
		id, err := parent.StartChild(t.Context(), callID, task.Request{Prompt: "child"})
		out <- spawnResult{id: id, err: err}
	}()
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
	if got := len(parent.children); got != 8 {
		t.Fatalf("running children = %d, want 8", got)
	}
	ninth := make(chan spawnResult, 1)
	entered := make(chan struct{})
	startSignaledChild(t, parent, "close-8", ninth, entered)
	<-entered
	if got := len(ninth); got != 0 {
		t.Fatalf("ninth spawn returned before the close, want it blocked")
	}
	closeChild(t, h, ids[0])
	r := collectSpawn(t, ninth)
	if r.err != nil || r.id == "" {
		t.Fatalf("ninth spawn = %q, %v, want a child id", r.id, r.err)
	}
}
