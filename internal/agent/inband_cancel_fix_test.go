package agent

import (
	"encoding/json/jsontext"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/model"
)

func TestInBandSecondCallDeclinedAfterCancel(t *testing.T) {
	t.Parallel()
	h, thread, w := inBandHarness(t, nil)
	ctx := t.Context()
	first, err := w.inBandEvents(ctx, model.ToolCall{ID: "c1", Name: "durable", Args: jsontext.Value(`{}`)})
	if err != nil {
		t.Fatalf("first in-band call failed: %v", err)
	}
	if !first.OK {
		t.Fatalf("first in-band call not accepted: %+v", first)
	}
	w.inBand.markCancel()
	second, err := w.inBandEvents(ctx, model.ToolCall{ID: "c2", Name: "durable", Args: jsontext.Value(`{}`)})
	if err != nil {
		t.Fatalf("second in-band call failed: %v", err)
	}
	if second.OK {
		t.Fatalf("second in-band call ran after a cancel: %+v", second)
	}
	if !strings.Contains(second.Output, "cancelled by user") {
		t.Fatalf("second in-band output = %q, want cancellation", second.Output)
	}
	events := h.events(thread.ID())
	if started(events, "c2") {
		t.Fatal("second in-band call reached tool.started after a cancel")
	}
	if got := resolutions(events)["c2"]; got.Decision != "cancel" {
		t.Fatalf("second in-band resolution = %+v, want cancel", got)
	}
}

func TestInBandCancelConsumedOnEmptyReply(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := h.start()
	ctx := t.Context()
	if _, err := thread.appendEvents(ctx, event.TurnStarted{TurnID: "turn-test"}); err != nil {
		t.Fatalf("start turn: %v", err)
	}
	w := &worker{t: thread, turnID: "turn-test"}
	if err := w.prime(ctx); err != nil {
		t.Fatalf("prime worker: %v", err)
	}
	w.inBand = newInBandCalls()
	w.inBand.markCancel()
	v, messages, err := w.context(ctx)
	if err != nil {
		t.Fatalf("worker context: %v", err)
	}
	if err := w.step(ctx, v, messages); err != nil {
		t.Fatalf("step with empty reply failed: %v", err)
	}
	if w.inBand.takeCancel() {
		t.Fatal("cancel flag survived a step with an empty reply")
	}
}
