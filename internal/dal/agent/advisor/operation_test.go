package advisor

import (
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/agent/event"
)

func TestRuntimeDropsTheOperationOfTheRequestThatAttachedIt(t *testing.T) {
	t.Parallel()
	h := newHarness(t, nil)
	primary := startThread(t, h, agent.ThreadSpec{})
	const operationID = "019a3b7c-1d2e-7f00-8a00-0123456789ab"
	runtime, err := Attach(agent.WithOperation(t.Context(), operationID), h.host, primary, h.cfg)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = runtime.Close() })
	reviewOnce(t, h, primary, 1)
	for _, item := range journalEvents(t, h.store, childOf(t, h.host, primary.ID())) {
		if got := event.OperationOf(item.Event); got != "" {
			t.Fatalf("advisor session journaled %s with operation %q from the attaching request", item.Event.Type(), got)
		}
	}
}
