package agent

import (
	"context"
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
