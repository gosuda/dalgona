package agent

import (
	"context"
	"encoding/json/jsontext"
	"io"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/model"
)

type emptyStream struct{}

func (emptyStream) Next(context.Context) (model.Event, error) { return nil, io.EOF }
func (emptyStream) Events() <-chan model.Event                { return nil }
func (emptyStream) Err() error                                { return nil }
func (emptyStream) Close() error                              { return nil }

type emptyProvider struct{}

func (emptyProvider) Stream(context.Context, model.Request) (model.Stream, error) {
	return emptyStream{}, nil
}

func (emptyProvider) Models(context.Context) ([]model.Model, error) { return nil, nil }

func strandedEvents(turnID string) (event.ToolCall, []event.Event) {
	call := event.ToolCall{ID: "c1", Name: "exec", Args: jsontext.Value(`{}`)}
	events := []event.Event{
		event.ApprovalRequested{RequestID: "req-1", CallID: "c1", Summary: "echo hi", Class: "exec"},
		event.ApprovalResolved{RequestID: "req-1", Decision: "accept", By: "mode:yolo"},
		event.ToolStarted{CallID: "c1", TurnID: turnID, Tool: "exec", Class: "exec", Args: jsontext.Value(`{}`)},
		event.ToolCompleted{CallID: "c1", OK: true, Output: "hi", Recovered: "none"},
	}
	return call, events
}

func TestStreamFlushesStrandedInBandEvents(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := h.start()
	if _, err := thread.appendEvents(t.Context(), event.TurnStarted{TurnID: "turn-test"}); err != nil {
		t.Fatalf("agent: start turn: %v", err)
	}
	registry := model.NewRegistry()
	if err := registry.Register("stub", emptyProvider{}); err != nil {
		t.Fatalf("agent: register provider: %v", err)
	}
	w := &worker{t: thread, turnID: "turn-test", registry: registry}
	w.settings.Model = "stub/m"
	w.inBand = newInBandCalls()
	call, events := strandedEvents("turn-test")
	w.inBand.put(call, events)
	status, err := thread.Status()
	if err != nil {
		t.Fatalf("agent: status: %v", err)
	}
	if _, err := w.stream(t.Context(), model.Request{}, uint64(status.Head)); err != nil {
		t.Fatalf("agent: stream: %v", err)
	}
	got := h.events(thread.ID())
	if !started(got, "c1") {
		t.Fatalf("agent: stranded in-band events lost across streams: %v", h.types(thread.ID()))
	}
	outputs := completedOutputs(got)
	completed, ok := outputs["c1"]
	if !ok || !completed.OK || string(completed.Output) != "hi" {
		t.Fatalf("agent: stranded in-band result missing: %v", outputs)
	}
}
