package agent

import (
	"errors"
	"fmt"
	"strings"
	"sync"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
)

func TestChildDepthGuardOwnsBothEntryPoints(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	parent := h.start()
	child, err := h.host.startSession(t.Context(), ThreadSpec{Parent: parent.ID()}, "nested-child")
	if err != nil {
		t.Fatal(err)
	}
	_, direct := child.spawnChild(t.Context(), "direct", TaskRequest{Prompt: "nested"})
	_, script := child.StartChild(t.Context(), "script", TaskRequest{Prompt: "nested"})
	for _, err := range []error{direct, script} {
		if !errors.Is(err, errNestedSpawn) {
			t.Fatalf("nested spawn = %v, want %v", err, errNestedSpawn)
		}
	}
	if runningChildren(child) != 0 || len(spawnedChildren(h.events(child.ID()))) != 0 {
		t.Fatal("nested spawn reserved a slot or wrote child.spawned")
	}
}

func TestRunWideSubagentStartBudgetExhaustion(t *testing.T) {
	t.Parallel()
	h := newHarness(t, func(h *harness) { h.cfg.Config.MaxSubagentStarts = 2 })
	h.model.route = func([]byte) (response, bool) { return yieldData("done", `"done"`), true }
	parent := h.start()
	seedSpawnCalls(t, h, parent, "budget", 6)
	results := launchBudgetBatch(t, parent)
	if got := len(spawnedChildren(h.events(parent.ID()))); got != 2 {
		t.Fatalf("durable starts = %d, want 2", got)
	}
	assertBudgetResults(t, results)
	assertBudgetReuse(t, h, parent)
}

func admitBudgetBatch(t *testing.T, parent *Thread) []error {
	t.Helper()
	results := make([]error, 6)
	var group sync.WaitGroup
	for i := range results {
		group.Go(func() {
			_, results[i] = parent.journalChild(t.Context(), fmt.Sprintf("budget-%d", i), fmt.Sprintf("child-%d", i), ThreadSpec{Runner: "local"})
		})
	}
	group.Wait()
	return results
}

func assertBudgetResults(t *testing.T, results []error) {
	t.Helper()
	succeeded, exhausted := 0, 0
	for _, err := range results {
		switch {
		case err == nil:
			succeeded++
		case strings.Contains(err.Error(), "subagent budget exhausted: 2 of 2 starts used"):
			exhausted++
		default:
			t.Fatalf("admission = %v", err)
		}
	}
	if succeeded != 2 || exhausted != 4 {
		t.Fatalf("successful/exhausted = %d/%d, want 2/4", succeeded, exhausted)
	}
}

func assertBudgetReuse(t *testing.T, h *harness, parent *Thread) {
	t.Helper()
	before := spawnedChildren(h.events(parent.ID()))
	for _, child := range before {
		id, err := parent.journalChild(t.Context(), child.CallID, "unused", ThreadSpec{Runner: "local"})
		if err != nil || id != child.ChildID {
			t.Fatalf("reusing child = %q, %v", id, err)
		}
	}
	if len(spawnedChildren(h.events(parent.ID()))) != len(before) {
		t.Fatal("reopening existing children charged starts")
	}
}

func TestInvalidChildPromptConsumesNoStart(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	parent := h.start()
	_, direct := parent.spawnChild(t.Context(), "direct", TaskRequest{})
	_, script := parent.StartChild(t.Context(), "script", TaskRequest{})
	for _, err := range []error{direct, script} {
		if err == nil || err.Error() != "task needs a prompt" {
			t.Fatalf("empty prompt = %v", err)
		}
	}
	if len(spawnedChildren(h.events(parent.ID()))) != 0 || runningChildren(parent) != 0 {
		t.Fatal("invalid creation consumed a start or slot")
	}
}

func TestNewRootTurnResetsSubagentBudget(t *testing.T) {
	t.Parallel()
	h := newHarness(t, func(h *harness) { h.cfg.Config.MaxSubagentStarts = 2 })
	parent := h.start()
	seedSpawnCalls(t, h, parent, "budget", 6)
	assertBudgetResults(t, admitBudgetBatch(t, parent))
	completed := make([]event.Event, 0, 7)
	for i := range 6 {
		completed = append(completed, event.ToolCompleted{CallID: fmt.Sprintf("budget-%d", i), OK: true, Recovered: "none"})
	}
	completed = append(completed, event.TurnCompleted{TurnID: "turn-seed", Status: "completed"})
	if err := parent.Append(t.Context(), completed...); err != nil {
		t.Fatal(err)
	}
	if err := parent.Append(t.Context(), event.TurnStarted{TurnID: "next"}, event.ToolStarted{CallID: "next-call", TurnID: "next", Tool: "task", Class: "agent", Args: []byte(`{}`)}); err != nil {
		t.Fatal(err)
	}
	if _, err := parent.journalChild(t.Context(), "next-call", "next-child", ThreadSpec{Runner: "local"}); err != nil {
		t.Fatalf("first start of new root turn = %v", err)
	}
}

func launchBudgetBatch(t *testing.T, parent *Thread) []error {
	t.Helper()
	results := make([]error, 6)
	var group sync.WaitGroup
	for i := range results {
		group.Go(func() {
			callID := fmt.Sprintf("budget-%d", i)
			req := TaskRequest{Prompt: "child"}
			if i%2 == 0 {
				_, results[i] = parent.spawnChild(t.Context(), callID, req)
			} else {
				_, results[i] = parent.StartChild(t.Context(), callID, req)
			}
		})
	}
	group.Wait()
	return results
}
