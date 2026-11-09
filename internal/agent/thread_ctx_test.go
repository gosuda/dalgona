package agent

import (
	"context"
	"encoding/json/jsontext"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

func cancelledCause(t *testing.T) context.Context {
	t.Helper()
	ctx, cancel := context.WithCancelCause(t.Context())
	cancel(context.Canceled)
	return ctx
}

func TestCancelledContextRejectsAppendWithoutCompleted(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := h.start()
	if _, err := thread.appendEvents(cancelledCause(t), event.ThreadNamed{Name: "cancelled"}); err == nil {
		t.Fatal("agent: append without completed side effect ignored cancelled context")
	}
}

func TestCompletedResultsSurviveCancelledContext(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := h.start()
	w := &worker{t: thread}
	if _, err := thread.appendEvents(t.Context(), event.TurnStarted{TurnID: "turn-test"}, event.ToolStarted{CallID: "c1", TurnID: "turn-test", Tool: "exec", Class: "exec", Args: jsontext.Value(`{}`)}); err != nil {
		t.Fatalf("agent: record tool.started: %v", err)
	}
	ctx := cancelledCause(t)
	if err := w.appendResults(ctx, []event.ToolCompleted{{CallID: "c1", Output: "hi", OK: true, Recovered: "none"}}); err != nil {
		t.Fatalf("agent: completed batch dropped on cancelled context: %v", err)
	}
	outputs := completedOutputs(h.events(thread.ID()))
	completed, ok := outputs["c1"]
	if !ok || string(completed.Output) != "hi" {
		t.Fatalf("agent: completed result missing after cancelled append: %v", outputs)
	}
}
