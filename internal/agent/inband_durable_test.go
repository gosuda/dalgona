package agent

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/tool"
)

type durableStub struct {
	fail error
}

func (durableStub) Spec() tool.Spec {
	return tool.Spec{Name: "durable", Description: "in-band durability probe", Schema: jsontext.Value(`{"type":"object"}`)}
}

func (durableStub) Class() tool.Class { return tool.ClassAgent }

func (s durableStub) Run(context.Context, tool.Call, tool.Env) (tool.Result, error) {
	if s.fail != nil {
		return tool.Result{}, s.fail
	}
	return tool.Result{Output: []byte("done"), OK: true}, nil
}

func startInBandTurn(t *testing.T, thread *Thread) {
	t.Helper()
	if _, err := thread.appendEvents(t.Context(), event.TurnStarted{TurnID: "turn-test"}); err != nil {
		t.Fatalf("agent: start turn: %v", err)
	}
}

func inBandHarness(t *testing.T, fail error) (*harness, *Thread, *worker) {
	t.Helper()
	h := newHarness(t, func(h *harness) {
		h.cfg.Tools = func(string, string) ([]tool.Tool, error) {
			return []tool.Tool{durableStub{fail: fail}}, nil
		}
	})
	thread := h.start()
	startInBandTurn(t, thread)
	ts, err := thread.toolsFor("openai/gpt-test")
	if err != nil {
		t.Fatalf("agent: tools: %v", err)
	}
	w := &worker{
		t:        thread,
		turnID:   "turn-test",
		settings: Settings{Approval: config.ApprovalYolo},
		tools:    ts,
		inBand:   newInBandCalls(),
	}
	return h, thread, w
}

func callTypes(events []event.Seqd, callID string) map[string]int {
	out := map[string]int{}
	for _, item := range events {
		switch value := item.Event.(type) {
		case event.ApprovalRequested:
			if value.CallID == callID {
				out["requested"]++
			}
		case event.ApprovalResolved:
			out["resolved"]++
		case event.ToolStarted:
			if value.CallID == callID {
				out["started"]++
			}
		case event.ToolCompleted:
			if value.CallID == callID {
				out["completed"]++
			}
		}
	}
	return out
}

func TestInBandRecordsDurableBeforeExecute(t *testing.T) {
	t.Parallel()
	h, thread, w := inBandHarness(t, nil)
	if _, err := w.inBandEvents(t.Context(), model.ToolCall{ID: "c1", Name: "durable", Args: jsontext.Value(`{}`)}); err != nil {
		t.Fatalf("agent: in-band events: %v", err)
	}
	got := callTypes(h.events(thread.ID()), "c1")
	for _, kind := range []string{"requested", "resolved", "started", "completed"} {
		if got[kind] != 1 {
			t.Fatalf("agent: in-band records without journalInBand = %v, want one of each", got)
		}
	}
	before := len(h.events(thread.ID()))
	if pending, err := w.journalInBand(t.Context(), []event.ToolCall{{ID: "c1", Resolved: true}}); err != nil || len(pending) != 0 {
		t.Fatalf("agent: journalInBand = %v, %v, want dedupe with no pending", pending, err)
	}
	if after := len(h.events(thread.ID())); after != before {
		t.Fatalf("agent: journalInBand appended %d events, want dedupe with no records", after-before)
	}
}

func TestInBandStartedSurvivesFailedFollowupAppend(t *testing.T) {
	t.Parallel()
	h, thread, w := inBandHarness(t, nil)
	if _, err := w.inBandEvents(t.Context(), model.ToolCall{ID: "c1", Name: "durable", Args: jsontext.Value(`{}`)}); err != nil {
		t.Fatalf("agent: in-band events: %v", err)
	}
	if _, err := ask(thread, func(a *actor) bool {
		a.damage = errors.New("agent: test damage")
		return true
	}); err != nil {
		t.Fatalf("agent: damage thread: %v", err)
	}
	if _, err := w.journalInBand(t.Context(), []event.ToolCall{{ID: "c1", Resolved: true}}); err == nil {
		t.Fatal("agent: follow-up append on damaged thread returned no error")
	}
	got := callTypes(h.events(thread.ID()), "c1")
	for _, kind := range []string{"requested", "resolved", "started", "completed"} {
		if got[kind] != 1 {
			t.Fatalf("agent: records after failed follow-up = %v, want pre-execute records durable", got)
		}
	}
}

func TestInBandHonorsCancel(t *testing.T) {
	t.Parallel()
	_, _, w := inBandHarness(t, nil)
	ctx, cancel := context.WithCancelCause(t.Context())
	cancel(context.Canceled)
	if _, err := w.inBandEvents(ctx, model.ToolCall{ID: "c1", Name: "durable", Args: jsontext.Value(`{}`)}); err == nil {
		t.Fatal("agent: in-band events ignored cancelled context")
	}
}
